use std::path::PathBuf;

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
    pub uid: u32,
    pub gid: u32,
    pub user: String,
    pub home: PathBuf,
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
