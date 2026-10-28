mod accounts;
mod credentials;
mod filesystem;
mod locking;
mod namespace;
mod types;

use accounts::Accounts;
use credentials::Credentials;
use filesystem::Filesystem;
use namespace::NamespaceOps;
use std::{
    fs::{read_to_string, symlink_metadata},
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

pub use locking::{PosixLock, DEFAULT_LOCK_TIMEOUT, DEFAULT_POLL_INTERVAL};
pub use namespace::{IdMapEntry, MountInfoEntry, NamespaceMap};
pub use types::{ActionChange, PosixError, PosixIdentity, PosixUser, WorkspaceObservation};

/// Access to the host POSIX account database and process credentials.
///
/// Account file paths are configurable so tests can use an isolated passwd
/// and group fixture. The default always points at the real container files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PosixSystem {
    passwd_path: PathBuf,
    group_path: PathBuf,
    uid_map: Option<NamespaceMap>,
    gid_map: Option<NamespaceMap>,
    mountinfo: Option<String>,
    workspace_observation: Option<WorkspaceObservation>,
    current_ids_override: Option<(u32, u32)>,
}

impl Default for PosixSystem {
    fn default() -> Self {
        Self {
            passwd_path: PathBuf::from("/etc/passwd"),
            group_path: PathBuf::from("/etc/group"),
            uid_map: None,
            gid_map: None,
            mountinfo: None,
            workspace_observation: None,
            current_ids_override: None,
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
            ..Self::default()
        }
    }

    pub fn passwd_path(&self) -> &Path {
        &self.passwd_path
    }

    pub fn group_path(&self) -> &Path {
        &self.group_path
    }

    pub fn current_ids(&self) -> (u32, u32) {
        self.current_ids_override
            .unwrap_or_else(Credentials::current_ids)
    }

    /// Inject parsed namespace maps for deterministic rootless and boundary tests.
    pub fn with_namespace_maps(mut self, uid_map: NamespaceMap, gid_map: NamespaceMap) -> Self {
        self.uid_map = Some(uid_map);
        self.gid_map = Some(gid_map);
        self
    }

    /// Inject namespace map text for tests without changing the process user namespace.
    pub fn with_namespace_map_contents(
        self,
        uid_map: &str,
        gid_map: &str,
    ) -> Result<Self, PosixError> {
        Ok(self.with_namespace_maps(NamespaceMap::parse(uid_map)?, NamespaceMap::parse(gid_map)?))
    }

    pub fn map_uid_from_parent(&self, uid: u32) -> Result<u32, PosixError> {
        self.uid_map
            .as_ref()
            .map(|map| map.map_parent_id(uid))
            .unwrap_or_else(|| {
                NamespaceOps::read_namespace_map(Path::new("/proc/self/uid_map"))?
                    .map_parent_id(uid)
            })
    }

    pub fn map_gid_from_parent(&self, gid: u32) -> Result<u32, PosixError> {
        self.gid_map
            .as_ref()
            .map(|map| map.map_parent_id(gid))
            .unwrap_or_else(|| {
                NamespaceOps::read_namespace_map(Path::new("/proc/self/gid_map"))?
                    .map_parent_id(gid)
            })
    }

    pub fn with_mountinfo(mut self, contents: impl Into<String>) -> Self {
        self.mountinfo = Some(contents.into());
        self
    }

    pub fn with_workspace_observation(mut self, observation: WorkspaceObservation) -> Self {
        self.workspace_observation = Some(observation);
        self
    }

    pub fn observe_workspace(&self, path: &Path) -> WorkspaceObservation {
        if let Some(observation) = &self.workspace_observation {
            return observation.clone();
        }
        let path = match NamespaceOps::normalize_workspace_path(path) {
            Ok(path) => path,
            Err(error) => {
                return WorkspaceObservation::Unavailable {
                    reason: error.to_string(),
                }
            }
        };
        if let Err(error) = NamespaceOps::reject_symlink_components(&path) {
            return WorkspaceObservation::Unavailable {
                reason: error.to_string(),
            };
        }
        let contents = match &self.mountinfo {
            Some(contents) => contents.clone(),
            None => match read_to_string("/proc/self/mountinfo") {
                Ok(contents) => contents,
                Err(error) => {
                    return WorkspaceObservation::Unavailable {
                        reason: format!("cannot read /proc/self/mountinfo: {error}"),
                    }
                }
            },
        };
        let mounts = match NamespaceOps::parse_mountinfo(&contents) {
            Ok(mounts) => mounts,
            Err(error) => {
                return WorkspaceObservation::Unavailable {
                    reason: error.to_string(),
                }
            }
        };
        let mount = mounts
            .into_iter()
            .filter(|mount| {
                mount.mount_point != Path::new("/") && is_mount_parent(&mount.mount_point, &path)
            })
            .max_by_key(|mount| mount.mount_point.components().count());
        let Some(mount) = mount else {
            return WorkspaceObservation::NotMounted {
                reason: "workspace is only covered by the root filesystem".to_owned(),
            };
        };
        let metadata = match symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() => metadata,
            Ok(_) => {
                return WorkspaceObservation::NotMounted {
                    reason: "workspace is not a directory".to_owned(),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return WorkspaceObservation::NotMounted {
                    reason: "workspace does not exist".to_owned(),
                }
            }
            Err(error) => {
                return WorkspaceObservation::Unavailable {
                    reason: format!("workspace cannot be inspected: {error}"),
                }
            }
        };
        WorkspaceObservation::Mounted {
            uid: metadata.uid(),
            gid: metadata.gid(),
            mount_point: mount.mount_point,
            mount_id: mount.mount_id,
        }
    }

    pub fn with_current_ids(mut self, uid: u32, gid: u32) -> Self {
        self.current_ids_override = Some((uid, gid));
        self
    }

    pub fn lookup_user_by_name(&self, name: &str) -> io::Result<Option<PosixUser>> {
        Credentials::lookup_user_by_name(self, name)
    }

    pub fn lookup_user_by_uid(&self, uid: u32) -> io::Result<Option<PosixUser>> {
        Credentials::lookup_user_by_uid(self, uid)
    }

    pub fn map_user(&self, identity: &PosixIdentity) -> Result<ActionChange, PosixError> {
        Accounts::map_user(self, identity)
    }

    pub fn set_user_shell(&self, user: &str, shell: &Path) -> Result<ActionChange, PosixError> {
        Accounts::set_user_shell(self, user, shell)
    }

    pub fn chown(
        &self,
        path: &Path,
        uid: u32,
        gid: u32,
        follow_symlink: bool,
    ) -> Result<ActionChange, PosixError> {
        Filesystem::chown(path, uid, gid, follow_symlink)
    }

    pub fn drop_privileges(&self, identity: &PosixIdentity) -> Result<ActionChange, PosixError> {
        Credentials::drop_privileges(self, identity)
    }

    fn atomic_replace(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        Filesystem::atomic_replace(path, contents)
    }
}

fn is_mount_parent(mount_point: &Path, workspace: &Path) -> bool {
    workspace == mount_point || workspace.starts_with(mount_point)
}

/// Parse the octal mode representation used by the Bootstrap DSL.
pub fn parse_mode(value: &str) -> Result<u32, String> {
    Filesystem::parse_mode(value)
}

/// Compatibility spelling for callers that use the model's short helper.
pub fn mode(value: &str) -> Result<u32, String> {
    Filesystem::mode(value)
}

pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    Filesystem::set_mode(path, mode)
}

pub fn default_lock_timeout() -> Duration {
    PosixLock::default_lock_timeout()
}

pub fn default_poll_interval() -> Duration {
    PosixLock::default_poll_interval()
}

pub fn is_writable(path: &Path) -> bool {
    PosixLock::is_writable(path)
}

pub fn parse_mountinfo(contents: &str) -> Result<Vec<MountInfoEntry>, PosixError> {
    NamespaceOps::parse_mountinfo(contents)
}

pub fn normalize_workspace_path(path: &Path) -> Result<PathBuf, PosixError> {
    NamespaceOps::normalize_workspace_path(path)
}

pub fn reject_symlink_components(path: &Path) -> Result<(), PosixError> {
    NamespaceOps::reject_symlink_components(path)
}

pub fn read_namespace_map(path: &Path) -> Result<NamespaceMap, PosixError> {
    NamespaceOps::read_namespace_map(path)
}

pub fn unshare_user_and_mount_namespaces(uid: u32, gid: u32) -> Result<(), PosixError> {
    NamespaceOps::unshare_user_and_mount_namespaces(uid, gid)
}

pub fn mount_cgroup2(target: &Path) -> Result<(), PosixError> {
    NamespaceOps::mount_cgroup2(target)
}

pub fn bind_mount(source: &Path, target: &Path) -> Result<(), PosixError> {
    NamespaceOps::bind_mount(source, target)
}
