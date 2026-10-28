use serde::{Deserialize, Serialize};
use std::{error::Error as StdError, fmt, path::PathBuf};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum IdentityRequestError {
    InvalidUser,
}

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

impl fmt::Display for IdentityRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUser => formatter.write_str("identity user name is invalid"),
        }
    }
}

impl StdError for IdentityRequestError {}

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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum IdentityError {
    InvalidUser,
    InvalidHome,
    RootGroupMismatch,
}

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

/// The broker's result for one request.  This is intentionally a data
/// contract shared by dev-env and container-init; resolution and namespace
/// mapping stay in container-init.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveIdentity {
    uid: u32,
    gid: u32,
    user: String,
    home: PathBuf,
    #[serde(default)]
    supplementary_groups: Vec<u32>,
    #[serde(default)]
    run_as_root: bool,
    #[serde(default)]
    source: IdentitySource,
    #[serde(default)]
    workspace: WorkspaceStatus,
}

pub type IdentitySnapshot = EffectiveIdentity;

impl EffectiveIdentity {
    pub fn new(
        uid: u32,
        gid: u32,
        user: impl Into<String>,
        home: impl Into<PathBuf>,
        source: IdentitySource,
    ) -> Self {
        Self {
            uid,
            gid,
            user: user.into(),
            home: home.into(),
            supplementary_groups: Vec::new(),
            run_as_root: false,
            source,
            workspace: WorkspaceStatus::Unavailable,
        }
    }

    pub fn with_supplementary_groups(mut self, groups: Vec<u32>) -> Self {
        self.supplementary_groups = groups;
        self
    }

    pub fn with_run_as_root(mut self, run_as_root: bool) -> Self {
        self.run_as_root = run_as_root;
        self
    }

    pub fn with_workspace(mut self, workspace: WorkspaceStatus) -> Self {
        self.workspace = workspace;
        self
    }

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

impl fmt::Display for IdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUser => "resolved identity user is invalid",
            Self::InvalidHome => "resolved identity HOME must be an absolute path",
            Self::RootGroupMismatch => "root identity must use gid 0",
        })
    }
}

impl StdError for IdentityError {}

impl EffectiveIdentity {
    pub fn uid(&self) -> u32 {
        self.uid
    }

    pub fn gid(&self) -> u32 {
        self.gid
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn home(&self) -> &PathBuf {
        &self.home
    }

    pub fn supplementary_groups(&self) -> &Vec<u32> {
        &self.supplementary_groups
    }

    pub fn run_as_root(&self) -> bool {
        self.run_as_root
    }

    pub fn source(&self) -> IdentitySource {
        self.source
    }

    pub fn workspace(&self) -> WorkspaceStatus {
        self.workspace
    }
}
