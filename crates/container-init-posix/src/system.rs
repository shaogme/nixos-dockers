use crate::accounts;
use crate::credentials;
use crate::error::PosixError;
use crate::filesystem;
use crate::namespace::{self, NamespaceMap};
use crate::privilege;
use crate::types::{ActionChange, PosixIdentity, PosixUser, WorkspaceObservation};
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
            .unwrap_or_else(credentials::current_ids)
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
    ) -> Result<Self, crate::error::PosixError> {
        Ok(self.with_namespace_maps(NamespaceMap::parse(uid_map)?, NamespaceMap::parse(gid_map)?))
    }

    pub fn map_uid_from_parent(&self, uid: u32) -> Result<u32, crate::error::PosixError> {
        self.uid_map
            .as_ref()
            .map(|map| map.map_parent_id(uid))
            .unwrap_or_else(|| {
                namespace::read_namespace_map(Path::new("/proc/self/uid_map"))?.map_parent_id(uid)
            })
    }

    pub fn map_gid_from_parent(&self, gid: u32) -> Result<u32, crate::error::PosixError> {
        self.gid_map
            .as_ref()
            .map(|map| map.map_parent_id(gid))
            .unwrap_or_else(|| {
                namespace::read_namespace_map(Path::new("/proc/self/gid_map"))?.map_parent_id(gid)
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
        let path = match namespace::normalize_workspace_path(path) {
            Ok(path) => path,
            Err(error) => {
                return WorkspaceObservation::Unavailable {
                    reason: error.to_string(),
                }
            }
        };
        if let Err(error) = namespace::reject_symlink_components(&path) {
            return WorkspaceObservation::Unavailable {
                reason: error.to_string(),
            };
        }
        let contents = match &self.mountinfo {
            Some(contents) => contents.clone(),
            None => match std::fs::read_to_string("/proc/self/mountinfo") {
                Ok(contents) => contents,
                Err(error) => {
                    return WorkspaceObservation::Unavailable {
                        reason: format!("cannot read /proc/self/mountinfo: {error}"),
                    }
                }
            },
        };
        let mounts = match namespace::parse_mountinfo(&contents) {
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
        let metadata = match std::fs::symlink_metadata(&path) {
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
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
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

fn is_mount_parent(mount_point: &Path, workspace: &Path) -> bool {
    workspace == mount_point || workspace.starts_with(mount_point)
}
