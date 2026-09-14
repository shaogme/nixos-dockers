use crate::error::LoaderError;
use crate::merge::MergedBootstrap;
use crate::raw::{parse_profile, validate_extends, validate_profile_id};
use crate::source::{LoadedBootstrap, LoadedProfile, ProfileSource};
use bootstrap_model::SourceKind;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// In-memory registry and loader for image, admin, and explicitly supplied
/// overlay profiles.
#[derive(Clone, Debug, Default)]
pub struct ProfileLoader {
    profiles: std::collections::BTreeMap<String, ProfileSource>,
}

/// Short name for callers that think in terms of the binary rather than the
/// profile registry.
pub type BootstrapLoader = ProfileLoader;

impl ProfileLoader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a profile, indexing it by the id declared in its document.
    pub fn add_profile(&mut self, profile: ProfileSource) -> Result<(), LoaderError> {
        validate_profile_id(&profile.id)?;
        let parsed = parse_profile(
            &profile.contents,
            profile
                .location
                .clone()
                .unwrap_or_else(|| format!("profile {}", profile.id)),
        )?;
        if parsed.id != profile.id {
            return Err(LoaderError::Invalid {
                location: profile
                    .location
                    .clone()
                    .unwrap_or_else(|| format!("profile {}", profile.id)),
                message: format!(
                    "profile source id {:?} does not match document id {:?}",
                    profile.id, parsed.id
                ),
            });
        }
        if let Some(previous) = self.profiles.get(&profile.id) {
            return Err(LoaderError::DuplicateProfile {
                id: profile.id,
                first: previous.location.clone(),
                second: profile.location,
            });
        }
        self.profiles.insert(profile.id.clone(), profile);
        Ok(())
    }

    pub fn add_str(
        &mut self,
        id: impl Into<String>,
        source: SourceKind,
        contents: impl Into<String>,
    ) -> Result<(), LoaderError> {
        self.add_profile(ProfileSource::new(id, source, contents))
    }

    /// Load every `*.toml` file in a directory. Profile ids come from the
    /// documents, and files are read in sorted path order only to make
    /// duplicate-id diagnostics deterministic.
    pub fn from_directory(path: impl AsRef<Path>) -> Result<Self, LoaderError> {
        let directory = path.as_ref().to_path_buf();
        let mut paths = fs::read_dir(&directory)
            .map_err(|source| LoaderError::Io {
                path: directory.clone(),
                source,
            })?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|source| LoaderError::Io {
                        path: directory.clone(),
                        source,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();

        let mut loader = Self::new();
        for path in paths {
            if path.extension().and_then(|extension| extension.to_str()) == Some("toml") {
                loader.add_profile(ProfileSource::from_file(path, SourceKind::ImageProfile)?)?;
            }
        }
        Ok(loader)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.profiles.contains_key(id)
    }

    pub fn profile_ids(&self) -> impl Iterator<Item = &str> {
        self.profiles.keys().map(String::as_str)
    }

    /// Resolve `profile_id` and all of its parents, then project and merge
    /// only their bootstrap namespaces.
    pub fn load(&self, profile_id: &str) -> Result<LoadedBootstrap, LoaderError> {
        let mut visiting = Vec::new();
        let mut visited = BTreeSet::new();
        let mut chain = Vec::new();
        let mut merged = MergedBootstrap::default();
        self.visit(
            profile_id,
            &mut visiting,
            &mut visited,
            &mut chain,
            &mut merged,
        )?;
        let config = merged.finish()?;
        Ok(LoadedBootstrap {
            config,
            profile_chain: chain,
        })
    }

    fn visit(
        &self,
        id: &str,
        visiting: &mut Vec<String>,
        visited: &mut BTreeSet<String>,
        chain: &mut Vec<LoadedProfile>,
        merged: &mut MergedBootstrap,
    ) -> Result<(), LoaderError> {
        if visited.contains(id) {
            return Ok(());
        }
        if let Some(index) = visiting.iter().position(|current| current == id) {
            let mut cycle = visiting[index..].to_vec();
            cycle.push(id.to_owned());
            return Err(LoaderError::InheritanceCycle(cycle));
        }

        let profile = self
            .profiles
            .get(id)
            .ok_or_else(|| LoaderError::MissingProfile(id.to_owned()))?;
        let location = profile
            .location
            .clone()
            .unwrap_or_else(|| format!("profile {id}"));
        let raw = parse_profile(&profile.contents, location.clone())?;
        if raw.id != profile.id {
            return Err(LoaderError::Invalid {
                location,
                message: format!(
                    "profile source id {:?} does not match document id {:?}",
                    profile.id, raw.id
                ),
            });
        }
        validate_profile_id(&raw.id)?;
        validate_extends(&raw.extends, &raw.id)?;

        visiting.push(id.to_owned());
        for parent in &raw.extends {
            self.visit(parent, visiting, visited, chain, merged)?;
        }
        visiting.pop();
        visited.insert(id.to_owned());

        let profile_info = LoadedProfile {
            id: raw.id.clone(),
            source: profile.source.clone(),
            location: profile.location.clone(),
        };
        merged.merge_profile(&raw, profile, &profile_info)?;
        chain.push(profile_info);
        Ok(())
    }
}
