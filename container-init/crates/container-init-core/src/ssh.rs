use crate::error::CoreError;
use crate::filesystem;
use crate::identity::ResolvedIdentity;
use bootstrap_model::Action;
use container_init_posix::{self as posix, ActionChange, PosixSystem};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_HOST_KEY_TYPES: &[&str] = &["rsa", "ed25519"];

/// The deliberately small, optional OpenSSH capability used by
/// `service.ssh.prepare`. The action still owns all filesystem paths; this
/// capability only supplies the trusted key-generation executable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshCapability {
    keygen: PathBuf,
}

impl Default for SshCapability {
    fn default() -> Self {
        Self {
            keygen: PathBuf::from("/usr/bin/ssh-keygen"),
        }
    }
}

impl SshCapability {
    pub fn new(keygen: impl Into<PathBuf>) -> Self {
        Self {
            keygen: keygen.into(),
        }
    }

    pub fn keygen(&self) -> &Path {
        &self.keygen
    }

    pub fn available(&self) -> bool {
        executable_file(&self.keygen)
    }
}

pub(crate) fn prepare(
    action: &Action,
    identity: &ResolvedIdentity,
    values: &std::collections::BTreeMap<String, String>,
    posix_system: &PosixSystem,
    capability: &SshCapability,
) -> Result<ActionChange, CoreError> {
    let action_id = action.id.as_str();
    let render = |field: &str, value: Option<&str>| {
        let value = value.ok_or_else(|| CoreError::Invalid {
            location: format!("bootstrap.actions.{action_id}.{field}"),
            message: "field is required".to_owned(),
        })?;
        filesystem::render_path(action_id, field, value, values)
    };

    let host_key_dir = render("host_key_dir", action.ssh_host_key_dir())?;
    let authorized_keys_dir = render("authorized_keys_dir", action.ssh_authorized_keys_dir())?;
    let runtime_dir = render("runtime_dir", action.ssh_runtime_dir())?;

    filesystem::ensure_dir(
        action_id,
        &host_key_dir,
        Some("0755"),
        Some("root"),
        identity,
        posix_system,
    )?;
    filesystem::ensure_dir(
        action_id,
        &authorized_keys_dir,
        Some("0755"),
        Some("root"),
        identity,
        posix_system,
    )?;
    filesystem::ensure_dir(
        action_id,
        &runtime_dir,
        Some("0755"),
        Some("root"),
        identity,
        posix_system,
    )?;

    let key_types = if action.ssh_host_key_types().is_empty() {
        DEFAULT_HOST_KEY_TYPES
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>()
    } else {
        action.ssh_host_key_types().to_vec()
    };
    let mut generated = 0usize;
    for key_type in &key_types {
        if ensure_host_key(action_id, &host_key_dir, key_type, capability, posix_system)? {
            generated += 1;
        }
    }

    let authorized_key_path = authorized_keys_dir.join(&identity.user);
    filesystem::validate_safe_path(&authorized_key_path, false, action_id)?;
    let authorized_keys = if let Some(content) = action.content.as_deref() {
        if content.contains('\0') {
            return Err(CoreError::action(
                action_id,
                Some(authorized_key_path.clone()),
                "authorized keys may not contain NUL",
            ));
        }
        filesystem::ensure_file(
            action_id,
            &authorized_key_path,
            Some(content),
            Some("0644"),
            Some("root"),
            identity,
            posix_system,
        )?;
        true
    } else if let Some(source) = action.authorized_keys_source.as_deref() {
        let source = filesystem::render_path(action_id, "authorized_keys_source", source, values)?;
        match read_authorized_keys(action_id, &source)? {
            Some(content) => {
                filesystem::ensure_file(
                    action_id,
                    &authorized_key_path,
                    Some(&content),
                    Some("0644"),
                    Some("root"),
                    identity,
                    posix_system,
                )?;
                true
            }
            None => false,
        }
    } else {
        false
    };

    Ok(ActionChange::new(format!(
        "prepared SSH service ({generated} host key(s) generated, authorized keys {})",
        if authorized_keys {
            "reconciled"
        } else {
            "not provided"
        }
    )))
}

fn ensure_host_key(
    action: &str,
    host_key_dir: &Path,
    key_type: &str,
    capability: &SshCapability,
    posix_system: &PosixSystem,
) -> Result<bool, CoreError> {
    let key_path = host_key_dir.join(format!("ssh_host_{key_type}_key"));
    let public_path = PathBuf::from(format!("{}.pub", key_path.display()));
    for path in [&key_path, &public_path] {
        filesystem::validate_safe_path(path, false, action)?;
    }

    let private_exists = regular_file_state(action, &key_path)?;
    let public_exists = regular_file_state(action, &public_path)?;
    match (private_exists, public_exists) {
        (true, true) => {
            reconcile_key_file(action, &key_path, 0o600, posix_system)?;
            reconcile_key_file(action, &public_path, 0o644, posix_system)?;
            Ok(false)
        }
        (true, false) | (false, true) => Err(CoreError::action(
            action,
            Some(key_path),
            format!(
                "SSH {key_type} host key is incomplete; private and public files must be present together"
            ),
        )),
        (false, false) => {
            let temporary = temporary_key_path(host_key_dir, key_type);
            let temporary_public = PathBuf::from(format!("{}.pub", temporary.display()));
            let result = generate_key(action, &temporary, key_type, capability, posix_system)
                .and_then(|()| install_key_pair(action, &temporary, &temporary_public, &key_path, &public_path, posix_system));
            if result.is_err() {
                remove_if_regular(&temporary);
                remove_if_regular(&temporary_public);
            }
            result.map(|()| true)
        }
    }
}

fn regular_file_state(action: &str, path: &Path) -> Result<bool, CoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(CoreError::action(
            action,
            Some(path.to_path_buf()),
            "SSH key path may not be a symbolic link",
        )),
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(CoreError::action(
            action,
            Some(path.to_path_buf()),
            "SSH key path exists but is not a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(CoreError::io(Some(action), Some(path.to_path_buf()), error)),
    }
}

fn reconcile_key_file(
    action: &str,
    path: &Path,
    mode: u32,
    posix_system: &PosixSystem,
) -> Result<(), CoreError> {
    posix::set_mode(path, mode)
        .map_err(|source| CoreError::io(Some(action), Some(path.to_path_buf()), source))?;
    posix_system
        .chown(path, 0, 0, true)
        .map_err(|error| CoreError::from_posix(action, Some(path.to_path_buf()), error))?;
    Ok(())
}

fn generate_key(
    action: &str,
    path: &Path,
    key_type: &str,
    capability: &SshCapability,
    posix_system: &PosixSystem,
) -> Result<(), CoreError> {
    let output = Command::new(capability.keygen())
        .args(["-q", "-t", key_type, "-N", "", "-f"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|source| {
            CoreError::io(
                Some(action),
                Some(capability.keygen().to_path_buf()),
                source,
            )
        })?;
    if !output.success() {
        return Err(CoreError::action(
            action,
            Some(path.to_path_buf()),
            format!("ssh-keygen failed for host key type {key_type:?} with status {output}"),
        ));
    }
    let public = PathBuf::from(format!("{}.pub", path.display()));
    if !regular_file_state(action, path)? || !regular_file_state(action, &public)? {
        return Err(CoreError::action(
            action,
            Some(path.to_path_buf()),
            "ssh-keygen did not produce a private/public key pair",
        ));
    }
    reconcile_key_file(action, path, 0o600, posix_system)?;
    reconcile_key_file(action, &public, 0o644, posix_system)?;
    Ok(())
}

fn install_key_pair(
    action: &str,
    temporary: &Path,
    temporary_public: &Path,
    private: &Path,
    public: &Path,
    posix_system: &PosixSystem,
) -> Result<(), CoreError> {
    if regular_file_state(action, private)? || regular_file_state(action, public)? {
        return Err(CoreError::action(
            action,
            Some(private.to_path_buf()),
            "SSH host key appeared while it was being generated; refusing replacement",
        ));
    }
    fs::hard_link(temporary, private)
        .map_err(|source| CoreError::io(Some(action), Some(private.to_path_buf()), source))?;
    if let Err(source) = fs::hard_link(temporary_public, public) {
        let _ = fs::remove_file(private);
        return Err(CoreError::io(
            Some(action),
            Some(public.to_path_buf()),
            source,
        ));
    }
    // The hard links make installation non-overwriting. Removing the
    // temporary names leaves the generated files in their declared paths.
    fs::remove_file(temporary)
        .map_err(|source| CoreError::io(Some(action), Some(temporary.to_path_buf()), source))?;
    fs::remove_file(temporary_public).map_err(|source| {
        CoreError::io(Some(action), Some(temporary_public.to_path_buf()), source)
    })?;
    reconcile_key_file(action, private, 0o600, posix_system)?;
    reconcile_key_file(action, public, 0o644, posix_system)?;
    Ok(())
}

fn read_authorized_keys(action: &str, path: &Path) -> Result<Option<String>, CoreError> {
    filesystem::validate_safe_path(path, false, action)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(CoreError::action(
            action,
            Some(path.to_path_buf()),
            "authorized key source may not be a symbolic link",
        )),
        Ok(metadata) if !metadata.is_file() => Err(CoreError::action(
            action,
            Some(path.to_path_buf()),
            "authorized key source is not a regular file",
        )),
        Ok(_) => {
            let content = fs::read_to_string(path)
                .map_err(|source| CoreError::io(Some(action), Some(path.to_path_buf()), source))?;
            if content.contains('\0') {
                return Err(CoreError::action(
                    action,
                    Some(path.to_path_buf()),
                    "authorized keys may not contain NUL",
                ));
            }
            Ok(Some(content))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CoreError::io(Some(action), Some(path.to_path_buf()), error)),
    }
}

fn temporary_key_path(directory: &Path, key_type: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    directory.join(format!(
        ".container-init-ssh-{}-{stamp}-{key_type}",
        std::process::id()
    ))
}

fn remove_if_regular(path: &Path) {
    if fs::symlink_metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
    {
        let _ = fs::remove_file(path);
    }
}

fn executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| {
            metadata.is_file() && {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode() & 0o111 != 0
                }
                #[cfg(not(unix))]
                {
                    true
                }
            }
        })
        .unwrap_or(false)
}
