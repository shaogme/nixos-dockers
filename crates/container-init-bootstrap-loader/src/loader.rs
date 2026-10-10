use crate::error::LoaderError;
use crate::merge::MergedBootstrap;
use crate::raw::{parse_profile, validate_profile_id};
use crate::source::{LoadedBootstrap, LoadedProfile, ProfileSource};
use container_init_bootstrap_model::SourceKind;
use profile_graph::{traverse_with_max_depth, ProfileGraphError, TraverseError};
use std::fs;
use std::path::Path;

const MAX_PROFILE_COLLECTION_BYTES: usize = 32 * 1024 * 1024;
const MAX_PROFILE_INHERITANCE_DEPTH: usize = 64;

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
        if profile.contents.len() > 8 * 1024 * 1024 {
            return Err(LoaderError::Invalid {
                location: profile
                    .location
                    .clone()
                    .unwrap_or_else(|| format!("profile {}", profile.id)),
                message: "profile file exceeds the 8388608 byte limit".to_owned(),
            });
        }
        let existing_bytes = self
            .profiles
            .values()
            .try_fold(0_usize, |total, entry| {
                total.checked_add(entry.contents.len())
            })
            .ok_or_else(|| LoaderError::Invalid {
                location: "profiles".to_owned(),
                message: "profile collection byte count overflowed".to_owned(),
            })?;
        let collection_bytes = existing_bytes
            .checked_add(profile.contents.len())
            .ok_or_else(|| LoaderError::Invalid {
                location: "profiles".to_owned(),
                message: "profile collection byte count overflowed".to_owned(),
            })?;
        if collection_bytes > MAX_PROFILE_COLLECTION_BYTES {
            return Err(LoaderError::Invalid {
                location: "profiles".to_owned(),
                message: format!(
                    "profile collection is {collection_bytes} bytes; maximum is {MAX_PROFILE_COLLECTION_BYTES} bytes"
                ),
            });
        }
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
        let mut chain = Vec::new();
        let mut merged = MergedBootstrap::default();
        let profiles = &self.profiles;
        traverse_with_max_depth(
            profile_id,
            MAX_PROFILE_INHERITANCE_DEPTH,
            |id| {
                let Some(profile) = profiles.get(id) else {
                    return Ok(None);
                };
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
                let parents = raw.extends.clone();
                Ok(Some((raw, parents)))
            },
            |id, raw| {
                let profile = profiles.get(id).expect("traversed profile was loaded");
                let profile_info = LoadedProfile {
                    id: raw.id.clone(),
                    source: profile.source.clone(),
                    location: profile.location.clone(),
                };
                merged.merge_profile(&raw, profile, &profile_info)?;
                chain.push(profile_info);
                Ok(())
            },
        )
        .map_err(map_graph_error)?;
        let config = merged.finish()?;
        Ok(LoadedBootstrap {
            config,
            profile_chain: chain,
        })
    }
}

fn map_graph_error(error: TraverseError<LoaderError>) -> LoaderError {
    match error {
        TraverseError::Source(error) => error,
        TraverseError::Graph(ProfileGraphError::MissingProfile(id)) => {
            LoaderError::MissingProfile(id)
        }
        TraverseError::Graph(ProfileGraphError::MissingParent { parent, .. }) => {
            LoaderError::MissingProfile(parent)
        }
        TraverseError::Graph(ProfileGraphError::Cycle(cycle)) => {
            LoaderError::InheritanceCycle(cycle)
        }
        TraverseError::Graph(ProfileGraphError::InvalidId(id)) => LoaderError::Invalid {
            location: "id".to_owned(),
            message: format!("invalid profile id {id:?}"),
        },
        TraverseError::Graph(ProfileGraphError::DuplicateParent { profile, parent }) => {
            LoaderError::Invalid {
                location: format!("profile {profile}.extends"),
                message: format!("parent profile {parent:?} is listed more than once"),
            }
        }
        TraverseError::Graph(ProfileGraphError::InheritanceDepthExceeded { limit }) => {
            LoaderError::Invalid {
                location: "profile inheritance".to_owned(),
                message: format!("inheritance exceeds the maximum depth of {limit}"),
            }
        }
    }
}
