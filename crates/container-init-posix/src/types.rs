use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
};

/// Errors returned by the small POSIX boundary used by container-init.
///
/// The crate intentionally keeps this error independent from the bootstrap
/// model. The core executor can attach an action id and provenance while
/// callers that use this crate directly still get a useful classification.
#[derive(Debug)]
pub enum PosixError {
    Invalid(String),
    Namespace(String),
    Permission(String),
    Io(io::Error),
}

impl PosixError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    pub(crate) fn permission(message: impl Into<String>) -> Self {
        Self::Permission(message.into())
    }

    pub(crate) fn namespace(message: impl Into<String>) -> Self {
        Self::Namespace(message.into())
    }

    pub(crate) fn io(source: io::Error) -> Self {
        Self::Io(source)
    }
}

impl fmt::Display for PosixError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid POSIX operation: {message}"),
            Self::Namespace(message) => {
                write!(formatter, "invalid namespace observation: {message}")
            }
            Self::Permission(message) => write!(formatter, "POSIX permission denied: {message}"),
            Self::Io(source) => source.fmt(formatter),
        }
    }
}

impl Error for PosixError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::Invalid(_) | Self::Namespace(_) | Self::Permission(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceObservation {
    Mounted {
        uid: u32,
        gid: u32,
        mount_point: PathBuf,
        mount_id: u64,
    },
    NotMounted {
        reason: String,
    },
    Unavailable {
        reason: String,
    },
}

impl WorkspaceObservation {
    pub fn mounted(uid: u32, gid: u32, mount_point: impl Into<PathBuf>, mount_id: u64) -> Self {
        Self::Mounted {
            uid,
            gid,
            mount_point: mount_point.into(),
            mount_id,
        }
    }

    pub fn status(&self) -> &'static str {
        match self {
            Self::Mounted { .. } => "mounted",
            Self::NotMounted { .. } => "not_mounted",
            Self::Unavailable { .. } => "unavailable",
        }
    }
}

/// A copied POSIX passwd entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PosixUser {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    pub shell: PathBuf,
}

/// The identity values needed by account and privilege operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PosixIdentity {
    uid: u32,
    gid: u32,
    user: String,
    home: PathBuf,
}

impl PosixIdentity {
    pub fn new(uid: u32, gid: u32, user: impl Into<String>, home: impl Into<PathBuf>) -> Self {
        Self {
            uid,
            gid,
            user: user.into(),
            home: home.into(),
        }
    }

    pub fn uid(&self) -> u32 {
        self.uid
    }

    pub fn gid(&self) -> u32 {
        self.gid
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn home(&self) -> &Path {
        &self.home
    }
}

/// A description of a successful idempotent POSIX operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionChange {
    message: String,
}

impl ActionChange {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn into_message(self) -> String {
        self.message
    }
}

impl AsRef<str> for ActionChange {
    fn as_ref(&self) -> &str {
        self.message()
    }
}
