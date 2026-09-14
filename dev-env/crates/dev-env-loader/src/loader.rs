use crate::error::LoaderError;
use crate::merge::MergedConfig;
use crate::raw::parse_profile;
use crate::runtime;
use crate::source::{CliPatch, LoadedConfig, LoadedProfile, ProfileSource};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// In-memory profile graph and environment loader.
#[derive(Clone, Debug, Default)]
pub struct ProfileLoader {
    profiles: BTreeMap<String, ProfileSource>,
    overlays: Vec<ProfileSource>,
}

/// Name callers can use when they think of the binary rather than the graph.
pub type DevEnvLoader = ProfileLoader;

impl ProfileLoader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an indexed profile. The id is read from the source document and is
    /// checked against the id supplied by the caller.
    pub fn add_profile(&mut self, profile: ProfileSource) -> Result<(), LoaderError> {
        let parsed = parse_profile(
            &profile.contents,
            profile
                .location
                .clone()
                .unwrap_or_else(|| format!("profile {}", profile.id)),
        )?;
        if parsed.document.id != profile.id {
            return Err(LoaderError::ProfileIdMismatch {
                source_id: profile.id,
                document_id: parsed.document.id,
            });
        }
        if !valid_source_id(&profile.id) {
            return Err(LoaderError::InvalidSourceId { id: profile.id });
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
        source: impl Into<crate::SourceKind>,
        contents: impl Into<String>,
    ) -> Result<(), LoaderError> {
        self.add_profile(ProfileSource::new(id, source, contents))
    }

    /// Add a non-inherited overlay to be applied after the selected profile
    /// graph. This is useful for user, workspace, and test overlays.
    pub fn add_overlay(&mut self, overlay: ProfileSource) -> Result<(), LoaderError> {
        let parsed = parse_profile(
            &overlay.contents,
            overlay
                .location
                .clone()
                .unwrap_or_else(|| format!("profile {}", overlay.id)),
        )?;
        if parsed.document.id != overlay.id {
            return Err(LoaderError::ProfileIdMismatch {
                source_id: overlay.id,
                document_id: parsed.document.id,
            });
        }
        if !valid_source_id(&overlay.id) {
            return Err(LoaderError::InvalidSourceId { id: overlay.id });
        }
        self.overlays.push(overlay);
        Ok(())
    }

    /// Load every TOML profile in a directory. Profile ids come from the
    /// documents, not from filenames; sorted paths make duplicate diagnostics
    /// deterministic.
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
                loader.add_profile(ProfileSource::from_file(
                    path,
                    crate::SourceKind::ImageProfile,
                )?)?;
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

    /// Resolve a profile graph and all registered overlays.
    pub fn load(&self, profile_id: &str) -> Result<LoadedConfig, LoaderError> {
        self.load_internal(profile_id, self.overlays.iter(), None, std::iter::empty())
    }

    /// Resolve a graph and apply the supplied overlays after registered ones.
    pub fn load_with_overlays<'a, I>(
        &self,
        profile_id: &str,
        overlays: I,
    ) -> Result<LoadedConfig, LoaderError>
    where
        I: IntoIterator<Item = &'a ProfileSource>,
    {
        self.load_internal(profile_id, self.overlays.iter(), None, overlays)
    }

    /// Resolve a graph and apply declared runtime environment inputs.
    pub fn load_with_runtime<I, K, V>(
        &self,
        profile_id: &str,
        environment: I,
    ) -> Result<LoadedConfig, LoaderError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let environment = environment
            .into_iter()
            .map(|(name, value)| (name.as_ref().to_owned(), value.as_ref().to_owned()))
            .collect::<BTreeMap<_, _>>();
        self.load_internal(
            profile_id,
            self.overlays.iter(),
            Some(&environment),
            std::iter::empty(),
        )
    }

    /// Resolve a graph and apply CLI patches last.
    pub fn load_with_cli<I>(
        &self,
        profile_id: &str,
        patches: I,
    ) -> Result<LoadedConfig, LoaderError>
    where
        I: IntoIterator<Item = CliPatch>,
    {
        let patches = patches.into_iter().collect::<Vec<_>>();
        self.load_internal(profile_id, self.overlays.iter(), None, std::iter::empty())
            .and_then(|loaded| {
                // CLI patches are applied through a second small merger so the
                // public LoadedConfig remains immutable and validated.
                let mut merger = MergedConfig::from_config(loaded.config.clone())?;
                for patch in &patches {
                    merger.apply_cli_patch(&patch.path, &patch.spec)?;
                }
                let (config, _) = merger.finish(loaded.profile_chain.clone())?;
                Ok(LoadedConfig {
                    config,
                    profile_chain: loaded.profile_chain,
                })
            })
    }

    /// Resolve a profile with every runtime layer in the documented order:
    /// registered overlays, supplied overlays, declared process inputs, and
    /// finally CLI patches.
    ///
    /// Keeping this operation in the loader is important.  Callers must not
    /// deserialize a resolved config and then overwrite fields themselves,
    /// because doing so would lose the strict merge checks and provenance.
    pub fn load_with_overlays_runtime_and_cli<'a, 'b, I, J, K, V, P>(
        &self,
        profile_id: &str,
        overlays: I,
        environment: J,
        patches: P,
    ) -> Result<LoadedConfig, LoaderError>
    where
        I: IntoIterator<Item = &'a ProfileSource>,
        J: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
        P: IntoIterator<Item = CliPatch>,
    {
        let environment = environment
            .into_iter()
            .map(|(name, value)| (name.as_ref().to_owned(), value.as_ref().to_owned()))
            .collect::<BTreeMap<_, _>>();
        let loaded = self.load_internal(
            profile_id,
            self.overlays.iter(),
            Some(&environment),
            overlays,
        )?;
        self.apply_cli_patches(loaded, patches)
    }

    fn apply_cli_patches<P>(
        &self,
        loaded: LoadedConfig,
        patches: P,
    ) -> Result<LoadedConfig, LoaderError>
    where
        P: IntoIterator<Item = CliPatch>,
    {
        let mut merger = MergedConfig::from_config(loaded.config.clone())?;
        for patch in patches {
            merger.apply_cli_patch(&patch.path, &patch.spec)?;
        }
        let (config, _) = merger.finish(loaded.profile_chain.clone())?;
        Ok(LoadedConfig {
            config,
            profile_chain: loaded.profile_chain,
        })
    }

    fn load_internal<'a, 'b, I, J>(
        &self,
        profile_id: &str,
        registered_overlays: I,
        runtime_environment: Option<&BTreeMap<String, String>>,
        extra_overlays: J,
    ) -> Result<LoadedConfig, LoaderError>
    where
        I: IntoIterator<Item = &'a ProfileSource>,
        J: IntoIterator<Item = &'b ProfileSource>,
    {
        let mut visiting = Vec::new();
        let mut visited = BTreeSet::new();
        let mut chain = Vec::new();
        let mut merged = MergedConfig::default();
        self.visit(
            profile_id,
            &mut visiting,
            &mut visited,
            &mut chain,
            &mut merged,
        )?;

        for overlay in registered_overlays {
            self.apply_source(overlay, &mut merged, &mut chain)?;
        }
        for overlay in extra_overlays {
            self.apply_source(overlay, &mut merged, &mut chain)?;
        }
        if let Some(environment) = runtime_environment {
            runtime::apply(environment, &mut merged)?;
        }
        let (config, chain) = merged.finish(chain)?;
        Ok(LoadedConfig {
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
        merged: &mut MergedConfig,
    ) -> Result<(), LoaderError> {
        if visited.contains(id) {
            return Ok(());
        }
        if let Some(index) = visiting.iter().position(|current| current == id) {
            let mut cycle = visiting[index..].to_vec();
            cycle.push(id.to_owned());
            return Err(LoaderError::InheritanceCycle { profiles: cycle });
        }

        let profile = self
            .profiles
            .get(id)
            .ok_or_else(|| LoaderError::MissingProfile { id: id.to_owned() })?;
        let parsed = parse_profile(
            &profile.contents,
            profile
                .location
                .clone()
                .unwrap_or_else(|| format!("profile {id}")),
        )?;
        if parsed.document.id != profile.id {
            return Err(LoaderError::ProfileIdMismatch {
                source_id: profile.id.clone(),
                document_id: parsed.document.id,
            });
        }

        visiting.push(id.to_owned());
        for parent in &parsed.document.extends {
            self.visit(parent, visiting, visited, chain, merged)?;
        }
        visiting.pop();
        visited.insert(id.to_owned());

        let info = LoadedProfile {
            id: parsed.document.id.clone(),
            source: profile.source,
            location: profile.location.clone(),
        };
        merged.merge_profile(&parsed, profile)?;
        chain.push(info);
        Ok(())
    }

    fn apply_source(
        &self,
        source: &ProfileSource,
        merged: &mut MergedConfig,
        chain: &mut Vec<LoadedProfile>,
    ) -> Result<(), LoaderError> {
        let parsed = parse_profile(
            &source.contents,
            source
                .location
                .clone()
                .unwrap_or_else(|| format!("profile {}", source.id)),
        )?;
        if parsed.document.id != source.id {
            return Err(LoaderError::ProfileIdMismatch {
                source_id: source.id.clone(),
                document_id: parsed.document.id,
            });
        }
        merged.merge_profile(&parsed, source)?;
        chain.push(LoadedProfile {
            id: source.id.clone(),
            source: source.source,
            location: source.location.clone(),
        });
        Ok(())
    }
}

fn valid_source_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}
