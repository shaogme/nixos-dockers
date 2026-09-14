use crate::args::CliOptions;
use crate::error::CliError;
use container_init_core::{PosixSystem, ResolvedIdentity};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub fn path(
    options: &CliOptions,
    profile: &str,
    workspace: &Path,
    identity: &ResolvedIdentity,
) -> PathBuf {
    if let Some(path) = &options.lock_path {
        return path.clone();
    }
    if let Some(path) = env::var_os("CONTAINER_INIT_LOCK_PATH") {
        return PathBuf::from(path);
    }

    let base = if let Some(runtime) = env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime).join("container-init")
    } else {
        #[cfg(unix)]
        if PosixSystem::new().current_ids().0 == 0 {
            PathBuf::from("/run/container-init")
        } else {
            workspace.join(".container-init")
        }
        #[cfg(not(unix))]
        {
            workspace.join(".container-init")
        }
    };
    base.join(format!(
        "bootstrap-{}.lock",
        stable_key(profile, workspace, identity)
    ))
}

pub fn ensure_parent(path: &Path) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| {
            CliError::io(
                "create bootstrap lock directory",
                Some(parent.to_path_buf()),
                source,
            )
        })?;
    }
    Ok(())
}

fn stable_key(profile: &str, workspace: &Path, identity: &ResolvedIdentity) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    let canonical_workspace =
        fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    for part in [
        profile.as_bytes(),
        canonical_workspace.as_os_str().as_encoded_bytes(),
        identity.user.as_bytes(),
        identity.uid.to_string().as_bytes(),
        identity.gid.to_string().as_bytes(),
    ] {
        for byte in part {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
