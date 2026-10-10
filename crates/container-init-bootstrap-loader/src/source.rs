use crate::error::LoaderError;
use crate::raw::parse_profile;
use container_init_bootstrap_model::{BootstrapConfig, Plan, SourceKind};
use std::fs::File;
use std::io::Read;
use std::path::Path;

const MAX_PROFILE_FILE_BYTES: usize = 8 * 1024 * 1024;

/// A profile's source determines which bootstrap declarations are trusted.
/// Image and admin profiles are trusted; workspace and user profiles are not.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileSource {
    pub(crate) id: String,
    pub(crate) source: SourceKind,
    pub(crate) location: Option<String>,
    pub(crate) contents: String,
}

impl ProfileSource {
    /// Construct an in-memory profile. `location` is used only in diagnostics
    /// and provenance; it may be omitted for callers such as unit tests.
    pub fn new(id: impl Into<String>, source: SourceKind, contents: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            source,
            location: None,
            contents: contents.into(),
        }
    }

    /// Construct an in-memory profile with a diagnostic location.
    pub fn with_location(
        id: impl Into<String>,
        source: SourceKind,
        location: impl Into<String>,
        contents: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            source,
            location: Some(location.into()),
            contents: contents.into(),
        }
    }

    /// Read a profile from a file. The profile id is read from the TOML
    /// document, rather than inferred from the filename.
    pub fn from_file(path: impl AsRef<Path>, source: SourceKind) -> Result<Self, LoaderError> {
        let path = path.as_ref().to_path_buf();
        let mut contents = String::new();
        File::open(&path)
            .map_err(|source| LoaderError::Io {
                path: path.clone(),
                source,
            })?
            .take((MAX_PROFILE_FILE_BYTES + 1) as u64)
            .read_to_string(&mut contents)
            .map_err(|source| LoaderError::Io {
                path: path.clone(),
                source,
            })?;
        if contents.len() > MAX_PROFILE_FILE_BYTES {
            return Err(LoaderError::Invalid {
                location: path.display().to_string(),
                message: format!(
                    "profile file is larger than the {MAX_PROFILE_FILE_BYTES} byte limit"
                ),
            });
        }
        let raw = parse_profile(&contents, path.display().to_string())?;
        Ok(Self {
            id: raw.id,
            source,
            location: Some(path.display().to_string()),
            contents,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn source(&self) -> &SourceKind {
        &self.source
    }

    pub fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }
}

/// A profile that participated in a successful load, in parent-to-child
/// order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedProfile {
    pub id: String,
    pub source: SourceKind,
    pub location: Option<String>,
}

/// The result of projecting and merging the bootstrap namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedBootstrap {
    pub(crate) config: BootstrapConfig,
    pub(crate) profile_chain: Vec<LoadedProfile>,
}

impl LoadedBootstrap {
    pub fn config(&self) -> &BootstrapConfig {
        &self.config
    }

    pub fn into_config(self) -> BootstrapConfig {
        self.config
    }

    pub fn profile_chain(&self) -> &[LoadedProfile] {
        &self.profile_chain
    }

    /// Build the static action plan after loading. This remains side-effect
    /// free; runtime input resolution belongs to the executor layer.
    pub fn build_plan(&self) -> Result<Plan, container_init_bootstrap_model::ModelError> {
        self.config.build_plan()
    }
}
