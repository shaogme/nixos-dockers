use serde::{Deserialize, Serialize};

/// A profile source used for provenance and trust checks.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
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
    pub fn is_trusted(&self) -> bool {
        matches!(self, Self::ImageProfile | Self::AdminProfile)
    }

    pub fn is_workspace(&self) -> bool {
        matches!(self, Self::WorkspaceOverlay)
    }
}

/// Provenance attached by the profile loader. It is not part of the profile
/// file itself, so actions can still be deserialized from the flat DSL shape.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Origin {
    pub profile: String,
    pub source: SourceKind,
    pub location: Option<String>,
}

impl Origin {
    pub fn image(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            source: SourceKind::ImageProfile,
            location: None,
        }
    }

    pub fn admin(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            source: SourceKind::AdminProfile,
            location: None,
        }
    }

    pub fn workspace(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            source: SourceKind::WorkspaceOverlay,
            location: None,
        }
    }

    pub fn unknown() -> Self {
        Self {
            profile: "<unknown>".to_owned(),
            source: SourceKind::Unknown,
            location: None,
        }
    }
}

impl Default for Origin {
    fn default() -> Self {
        Self::unknown()
    }
}
