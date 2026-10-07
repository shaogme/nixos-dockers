use crate::error::LoaderError;
use crate::raw::parse_profile;
use dev_env_model::{Layer, Origin, SourceId};
use serde::Serialize;
use std::fs;
use std::path::Path;

/// Origin class used by the loader to enforce trust boundaries.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    ImageProfile,
    AdminProfile,
    UserOverlay,
    WorkspaceOverlay,
    Runtime,
    Cli,
    Unknown,
}

impl SourceKind {
    pub fn is_trusted(self) -> bool {
        matches!(self, Self::ImageProfile | Self::AdminProfile)
    }

    pub fn is_workspace(self) -> bool {
        matches!(self, Self::WorkspaceOverlay)
    }

    pub fn layer(self) -> Layer {
        match self {
            Self::ImageProfile => Layer::Profile,
            Self::AdminProfile => Layer::Admin,
            Self::UserOverlay => Layer::User,
            Self::WorkspaceOverlay => Layer::Workspace,
            Self::Runtime => Layer::Runtime,
            Self::Cli => Layer::Cli,
            Self::Unknown => Layer::Profile,
        }
    }
}

impl From<Layer> for SourceKind {
    fn from(layer: Layer) -> Self {
        match layer {
            Layer::Profile => Self::ImageProfile,
            Layer::Admin => Self::AdminProfile,
            Layer::User => Self::UserOverlay,
            Layer::Workspace => Self::WorkspaceOverlay,
            Layer::Runtime => Self::Runtime,
            Layer::Cli => Self::Cli,
        }
    }
}

impl From<SourceId> for SourceKind {
    fn from(source: SourceId) -> Self {
        match source {
            SourceId::Profile(_) => Self::ImageProfile,
            SourceId::File(_) => Self::UserOverlay,
            SourceId::Environment(_) => Self::Runtime,
            SourceId::Cli => Self::Cli,
            SourceId::Unknown => Self::Unknown,
        }
    }
}

/// A TOML source. The declared id is indexed separately from its filename;
/// filenames never participate in inheritance resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileSource {
    pub(crate) id: String,
    pub(crate) source: SourceKind,
    pub(crate) location: Option<String>,
    pub(crate) contents: String,
}

impl ProfileSource {
    pub fn new(
        id: impl Into<String>,
        source: impl Into<SourceKind>,
        contents: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            location: None,
            contents: contents.into(),
        }
    }

    pub fn with_location(
        id: impl Into<String>,
        source: impl Into<SourceKind>,
        location: impl Into<String>,
        contents: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            location: Some(location.into()),
            contents: contents.into(),
        }
    }

    pub fn from_file(
        path: impl AsRef<Path>,
        source: impl Into<SourceKind>,
    ) -> Result<Self, LoaderError> {
        let path = path.as_ref().to_path_buf();
        let contents = fs::read_to_string(&path).map_err(|source| LoaderError::Io {
            path: path.clone(),
            source,
        })?;
        let parsed = parse_profile(&contents, path.display().to_string())?;
        Ok(Self {
            id: parsed.document.id.clone(),
            source: source.into(),
            location: Some(path.display().to_string()),
            contents,
        })
    }

    /// Read a user/workspace overlay.  Overlay examples intentionally omit
    /// the profile-only `schema` and `id` header; the loader supplies those
    /// two metadata fields while keeping all actual configuration data
    /// unchanged.  Full profile documents continue to use [`from_file`].
    pub fn from_overlay_file(
        path: impl AsRef<Path>,
        source: impl Into<SourceKind>,
        id: impl Into<String>,
    ) -> Result<Self, LoaderError> {
        let path = path.as_ref().to_path_buf();
        let mut contents = fs::read_to_string(&path).map_err(|source| LoaderError::Io {
            path: path.clone(),
            source,
        })?;
        let value: toml::Value =
            toml::from_str(&contents).map_err(|source| LoaderError::Parse {
                location: path.display().to_string(),
                source,
            })?;
        let table = value.as_table().expect("a TOML document is always a table");
        let id = id.into();
        let mut header = String::new();
        if !table.contains_key("schema") {
            header.push_str("schema = 1\n");
        }
        if !table.contains_key("id") {
            let encoded_id = toml::Value::String(id.clone()).to_string();
            header.push_str("id = ");
            header.push_str(&encoded_id);
            header.push('\n');
        }
        if !header.is_empty() {
            header.push_str(&contents);
            contents = header;
        }
        let parsed = parse_profile(&contents, path.display().to_string())?;
        Ok(Self {
            id: parsed.document.id.clone(),
            source: source.into(),
            location: Some(path.display().to_string()),
            contents,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn source(&self) -> SourceKind {
        self.source
    }

    pub fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }

    pub(crate) fn origin(&self, profile: &str, reason: Option<String>) -> Origin {
        let source = match self.source {
            SourceKind::ImageProfile => SourceId::Profile(profile.to_owned()),
            SourceKind::AdminProfile | SourceKind::UserOverlay | SourceKind::WorkspaceOverlay => {
                SourceId::File(self.location.clone().unwrap_or_else(|| profile.to_owned()))
            }
            SourceKind::Runtime => {
                SourceId::Environment(self.location.clone().unwrap_or_else(|| profile.to_owned()))
            }
            SourceKind::Cli => SourceId::Cli,
            SourceKind::Unknown => SourceId::Unknown,
        };
        Origin {
            source,
            line: None,
            column: None,
            layer: self.source.layer(),
            reason,
        }
    }
}

/// A profile participating in a successful parent-to-child chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedProfile {
    pub id: String,
    pub source: SourceKind,
    pub location: Option<String>,
}

/// A validated environment configuration and the profile chain that produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedConfig {
    pub(crate) config: dev_env_model::ResolvedConfig,
    pub(crate) profile_chain: Vec<LoadedProfile>,
}

impl LoadedConfig {
    pub fn config(&self) -> &dev_env_model::ResolvedConfig {
        &self.config
    }

    pub fn into_config(self) -> dev_env_model::ResolvedConfig {
        self.config
    }

    pub fn profile_chain(&self) -> &[LoadedProfile] {
        &self.profile_chain
    }
}

/// A patch supplied by a CLI adapter. Runtime input handling is separate from
/// this type so both paths can be tested without process-global state.
#[derive(Clone, Debug, PartialEq)]
pub struct CliPatch {
    pub path: String,
    pub spec: dev_env_model::OverrideSpec,
}

impl CliPatch {
    pub fn new(path: impl Into<String>, spec: dev_env_model::OverrideSpec) -> Self {
        Self {
            path: path.into(),
            spec,
        }
    }
}
