use crate::accounts;
use crate::credentials;
use crate::error::PosixError;
use crate::filesystem;
use crate::privilege;
use crate::types::{ActionChange, PosixIdentity, PosixUser};
use std::io;
use std::path::{Path, PathBuf};

/// Access to the host POSIX account database and process credentials.
///
/// Account file paths are configurable so tests can use an isolated passwd
/// and group fixture. The default always points at the real container files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PosixSystem {
    pub(crate) passwd_path: PathBuf,
    pub(crate) group_path: PathBuf,
}

impl Default for PosixSystem {
    fn default() -> Self {
        Self {
            passwd_path: PathBuf::from("/etc/passwd"),
            group_path: PathBuf::from("/etc/group"),
        }
    }
}

impl PosixSystem {
    pub fn new() -> Self {
        Self::default()
    }

    /// Use alternate account databases in unit and container fixture tests.
    /// The same files are used for both identity lookup and account edits.
    pub fn with_account_files(
        passwd_path: impl Into<PathBuf>,
        group_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            passwd_path: passwd_path.into(),
            group_path: group_path.into(),
        }
    }

    pub fn passwd_path(&self) -> &Path {
        &self.passwd_path
    }

    pub fn group_path(&self) -> &Path {
        &self.group_path
    }

    pub fn current_ids(&self) -> (u32, u32) {
        credentials::current_ids()
    }

    pub fn lookup_user_by_name(&self, name: &str) -> io::Result<Option<PosixUser>> {
        credentials::lookup_user_by_name(self, name)
    }

    pub fn lookup_user_by_uid(&self, uid: u32) -> io::Result<Option<PosixUser>> {
        credentials::lookup_user_by_uid(self, uid)
    }

    pub fn map_user(&self, identity: &PosixIdentity) -> Result<ActionChange, PosixError> {
        accounts::map_user(self, identity)
    }

    pub fn set_user_shell(&self, user: &str, shell: &Path) -> Result<ActionChange, PosixError> {
        accounts::set_user_shell(self, user, shell)
    }

    pub fn chown(
        &self,
        path: &Path,
        uid: u32,
        gid: u32,
        follow_symlink: bool,
    ) -> Result<ActionChange, PosixError> {
        filesystem::chown(path, uid, gid, follow_symlink)
    }

    pub fn drop_privileges(&self, identity: &PosixIdentity) -> Result<ActionChange, PosixError> {
        privilege::drop_privileges(self, identity)
    }
}
