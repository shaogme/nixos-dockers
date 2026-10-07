use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The identity a backend client is asking the container-init identity broker
/// to resolve.  A client never supplies an arbitrary uid/gid pair; the broker
/// maps one of these requests using the container's bootstrap policy.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum IdentityRequest {
    /// Resolve the identity represented by the authenticated socket peer.
    #[default]
    Peer,
    /// Request the root identity.  The broker still applies its policy.
    Root,
    /// Request a named account.  UID/GID namespace mapping remains broker
    /// owned and is deliberately absent from this type.
    User { name: String },
}

pub type RequestedIdentity = IdentityRequest;

impl IdentityRequest {
    pub fn validate(&self) -> Result<(), IdentityRequestError> {
        match self {
            Self::Peer | Self::Root => Ok(()),
            Self::User { name }
                if !name.is_empty()
                    && name.len() <= 32
                    && name.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                    }) =>
            {
                Ok(())
            }
            Self::User { .. } => Err(IdentityRequestError::InvalidUser),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum IdentityRequestError {
    InvalidUser,
}

impl std::fmt::Display for IdentityRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUser => formatter.write_str("identity user name is invalid"),
        }
    }
}

impl std::error::Error for IdentityRequestError {}

/// Credentials supplied by the Unix socket layer to the identity broker.
/// These values are observations, not client-controlled request fields.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityPeer {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

pub type PeerCredentials = IdentityPeer;

/// The broker's result for one request.  This is intentionally a data
/// contract shared by dev-env and container-init; resolution and namespace
/// mapping stay in container-init.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveIdentity {
    pub uid: u32,
    pub gid: u32,
    pub user: String,
    pub home: PathBuf,
    #[serde(default)]
    pub supplementary_groups: Vec<u32>,
    #[serde(default)]
    pub run_as_root: bool,
    #[serde(default)]
    pub source: IdentitySource,
    #[serde(default)]
    pub workspace: WorkspaceStatus,
}

pub type IdentitySnapshot = EffectiveIdentity;

impl EffectiveIdentity {
    pub fn root() -> Self {
        Self {
            uid: 0,
            gid: 0,
            user: "root".to_owned(),
            home: PathBuf::from("/root"),
            supplementary_groups: Vec::new(),
            run_as_root: true,
            source: IdentitySource::Root,
            workspace: WorkspaceStatus::Unavailable,
        }
    }

    pub fn validate(&self) -> Result<(), IdentityError> {
        if self.user.is_empty() || self.user.contains('\0') {
            return Err(IdentityError::InvalidUser);
        }
        if self.home.as_os_str().is_empty() || !self.home.is_absolute() {
            return Err(IdentityError::InvalidHome);
        }
        if self.uid == 0 && self.gid != 0 {
            return Err(IdentityError::RootGroupMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum IdentityError {
    InvalidUser,
    InvalidHome,
    RootGroupMismatch,
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUser => "resolved identity user is invalid",
            Self::InvalidHome => "resolved identity HOME must be an absolute path",
            Self::RootGroupMismatch => "root identity must use gid 0",
        })
    }
}

impl std::error::Error for IdentityError {}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    Root,
    Peer,
    NamedUser,
    #[default]
    Broker,
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Mounted,
    NotMounted,
    #[default]
    Unavailable,
}
