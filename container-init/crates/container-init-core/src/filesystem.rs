use crate::error::CoreError;
use crate::identity::ResolvedIdentity;
use bootstrap_model::PathTemplate;
use container_init_posix::{self as posix, ActionChange, PosixSystem};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn render_path(
    action: &str,
    field: &str,
    value: &str,
    values: &BTreeMap<String, String>,
) -> Result<PathBuf, CoreError> {
    let template = PathTemplate::new(value).map_err(CoreError::Model)?;
    template
        .render(values)
        .map(PathBuf::from)
        .map_err(|message| CoreError::Invalid {
            location: format!("bootstrap.actions.{action}.{field}"),
            message,
        })
}

pub(crate) fn ensure_dir(
    action: &str,
    path: &Path,
    mode: Option<&str>,
    owner: Option<&str>,
    identity: &ResolvedIdentity,
    posix_system: &PosixSystem,
) -> Result<ActionChange, CoreError> {
    validate_safe_path(path, false, action)?;
    let existed = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(CoreError::action(
                    action,
                    Some(path.to_path_buf()),
                    "path exists but is not a directory",
                ));
            }
            true
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .map_err(|source| CoreError::io(Some(action), Some(path.to_path_buf()), source))?;
            false
        }
        Err(source) => {
            return Err(CoreError::io(
                Some(action),
                Some(path.to_path_buf()),
                source,
            ))
        }
    };
    if let Some(mode) = mode {
        posix::set_mode(path, parse_mode(mode)?)
            .map_err(|source| CoreError::io(Some(action), Some(path.to_path_buf()), source))?;
    }
    if let Some(owner) = owner {
        chown_with_owner(action, path, owner, identity, posix_system, true)?;
    }
    Ok(ActionChange::new(if existed {
        "directory already exists and was reconciled"
    } else {
        "created directory"
    }))
}

pub(crate) fn ensure_file(
    action: &str,
    path: &Path,
    content: Option<&str>,
    mode: Option<&str>,
    owner: Option<&str>,
    identity: &ResolvedIdentity,
    posix_system: &PosixSystem,
) -> Result<ActionChange, CoreError> {
    validate_safe_path(path, false, action)?;
    if let Some(parent) = path.parent() {
        validate_safe_path(parent, false, action)?;
        fs::create_dir_all(parent)
            .map_err(|source| CoreError::io(Some(action), Some(parent.to_path_buf()), source))?;
    }
    let existed = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(CoreError::action(
                    action,
                    Some(path.to_path_buf()),
                    "path exists but is not a regular file",
                ));
            }
            true
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => false,
        Err(source) => {
            return Err(CoreError::io(
                Some(action),
                Some(path.to_path_buf()),
                source,
            ))
        }
    };

    if existed {
        if let Some(expected) = content {
            let mut actual = String::new();
            fs::File::open(path)
                .and_then(|mut file| file.read_to_string(&mut actual))
                .map_err(|source| CoreError::io(Some(action), Some(path.to_path_buf()), source))?;
            if actual != expected {
                return Err(CoreError::action(
                    action,
                    Some(path.to_path_buf()),
                    "existing file content differs; refusing to overwrite it",
                ));
            }
        }
    } else {
        let content = content.unwrap_or_default().as_bytes().to_owned();
        let temporary = temporary_path(path);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode.map(parse_mode).transpose()?.unwrap_or(0o644))
            .open(&temporary)
            .map_err(|source| CoreError::io(Some(action), Some(temporary.clone()), source))?;
        let result = (|| {
            file.write_all(&content)?;
            file.sync_all()?;
            fs::rename(&temporary, path)
        })();
        if let Err(source) = result {
            let _ = fs::remove_file(&temporary);
            return Err(CoreError::io(
                Some(action),
                Some(path.to_path_buf()),
                source,
            ));
        }
        if let Some(parent) = path.parent() {
            if let Ok(directory) = fs::File::open(parent) {
                let _ = directory.sync_all();
            }
        }
    }
    if let Some(mode) = mode {
        posix::set_mode(path, parse_mode(mode)?)
            .map_err(|source| CoreError::io(Some(action), Some(path.to_path_buf()), source))?;
    }
    if let Some(owner) = owner {
        chown_with_owner(action, path, owner, identity, posix_system, true)?;
    }
    Ok(ActionChange::new(if existed {
        "file already exists and was reconciled"
    } else {
        "created file atomically"
    }))
}

pub(crate) fn ensure_symlink(
    action: &str,
    link: &Path,
    target: &Path,
    parent_mode: Option<&str>,
    owner: Option<&str>,
    identity: &ResolvedIdentity,
    posix_system: &PosixSystem,
) -> Result<ActionChange, CoreError> {
    validate_safe_path(link, true, action)?;
    if let Some(parent) = link.parent() {
        validate_safe_path(parent, false, action)?;
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|source| {
                CoreError::io(Some(action), Some(parent.to_path_buf()), source)
            })?;
        }
        if let Some(mode) = parent_mode {
            posix::set_mode(parent, parse_mode(mode)?).map_err(|source| {
                CoreError::io(Some(action), Some(parent.to_path_buf()), source)
            })?;
        }
    }

    match fs::symlink_metadata(link) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let actual = fs::read_link(link)
                .map_err(|source| CoreError::io(Some(action), Some(link.to_path_buf()), source))?;
            if actual != target {
                return Err(CoreError::action(
                    action,
                    Some(link.to_path_buf()),
                    format!(
                        "existing symlink targets {}, expected {}; refusing replacement",
                        actual.display(),
                        target.display()
                    ),
                ));
            }
        }
        Ok(metadata) => {
            let reason = if metadata.is_dir() && is_mount_point(link) {
                "existing path is a mount point"
            } else {
                "existing path is a real file or directory"
            };
            return Err(CoreError::action(action, Some(link.to_path_buf()), reason));
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            std::os::unix::fs::symlink(target, link)
                .map_err(|source| CoreError::io(Some(action), Some(link.to_path_buf()), source))?;
        }
        Err(source) => {
            return Err(CoreError::io(
                Some(action),
                Some(link.to_path_buf()),
                source,
            ))
        }
    }

    if let Some(owner) = owner {
        // lchown is intentional: changing a link's owner must never modify
        // the target mounted or shared by another action.
        chown_with_owner(action, link, owner, identity, posix_system, false)?;
    }
    Ok(ActionChange::new("symlink already matched or was created"))
}

pub(crate) fn chown(
    action: &str,
    path: &Path,
    owner: &str,
    recursive: bool,
    identity: &ResolvedIdentity,
    posix_system: &PosixSystem,
) -> Result<ActionChange, CoreError> {
    validate_recursive_path(action, path, recursive)?;
    validate_safe_path(path, false, action)?;
    chown_with_owner(action, path, owner, identity, posix_system, false)?;
    if recursive {
        walk_no_symlinks(path, &mut |child| {
            chown_with_owner(action, child, owner, identity, posix_system, false).map(|_| ())
        })?;
    }
    Ok(ActionChange::new(if recursive {
        "updated ownership recursively without following symlinks"
    } else {
        "updated ownership"
    }))
}

pub(crate) fn chmod(
    action: &str,
    path: &Path,
    mode: &str,
    recursive: bool,
) -> Result<ActionChange, CoreError> {
    validate_recursive_path(action, path, recursive)?;
    validate_safe_path(path, false, action)?;
    set_mode_for_action(action, path, mode)?;
    if recursive {
        walk_no_symlinks(path, &mut |child| set_mode_for_action(action, child, mode))?;
    }
    Ok(ActionChange::new(if recursive {
        "updated mode recursively without following symlinks"
    } else {
        "updated mode"
    }))
}

fn set_mode_for_action(action: &str, path: &Path, mode: &str) -> Result<(), CoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| CoreError::io(Some(action), Some(path.to_path_buf()), source))?;
    if metadata.file_type().is_symlink() {
        return Err(CoreError::action(
            action,
            Some(path.to_path_buf()),
            "refusing to chmod a symbolic link",
        ));
    }
    posix::set_mode(path, parse_mode(mode)?)
        .map_err(|source| CoreError::io(Some(action), Some(path.to_path_buf()), source))
}

fn validate_recursive_path(action: &str, path: &Path, recursive: bool) -> Result<(), CoreError> {
    let value = path.as_os_str().to_string_lossy();
    if value
        .chars()
        .any(|character| matches!(character, '*' | '?' | '[' | ']'))
    {
        return Err(CoreError::action(
            action,
            Some(path.to_path_buf()),
            "wildcards are not accepted in filesystem action paths",
        ));
    }
    if recursive && (path == Path::new("/") || path.parent().is_none()) {
        return Err(CoreError::action(
            action,
            Some(path.to_path_buf()),
            "recursive filesystem actions may not target the filesystem root",
        ));
    }
    Ok(())
}

pub(crate) fn validate_safe_path(
    path: &Path,
    allow_final_symlink: bool,
    action: &str,
) -> Result<(), CoreError> {
    if !path.is_absolute() {
        return Err(CoreError::action(
            action,
            Some(path.to_path_buf()),
            "resolved path must be absolute",
        ));
    }
    let mut current = PathBuf::from("/");
    let components = path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            continue;
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let final_component = index + 1 == components.len();
                if !(allow_final_symlink && final_component) {
                    return Err(CoreError::action(
                        action,
                        Some(current),
                        "path contains an unexpected symbolic link",
                    ));
                }
            }
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => break,
            Err(source) => return Err(CoreError::io(Some(action), Some(current), source)),
        }
    }
    Ok(())
}

fn walk_no_symlinks(
    path: &Path,
    callback: &mut impl FnMut(&Path) -> Result<(), CoreError>,
) -> Result<(), CoreError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| CoreError::io(None, Some(path.to_path_buf()), source))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if !metadata.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(path)
        .map_err(|source| CoreError::io(None, Some(path.to_path_buf()), source))?
    {
        let child = entry
            .map_err(|source| CoreError::io(None, Some(path.to_path_buf()), source))?
            .path();
        callback(&child)?;
        walk_no_symlinks(&child, callback)?;
    }
    Ok(())
}

fn parse_mode(value: &str) -> Result<u32, CoreError> {
    posix::mode(value).map_err(|message| CoreError::Invalid {
        location: "filesystem mode".to_owned(),
        message,
    })
}

fn owner_ids(owner: &str, identity: &ResolvedIdentity) -> Result<(u32, u32), String> {
    match owner {
        "root" => Ok((0, 0)),
        "identity.target" => Ok((identity.uid, identity.gid)),
        numeric => {
            let (uid, gid) = numeric
                .split_once(':')
                .ok_or_else(|| "owner must be root, identity.target, or uid:gid".to_owned())?;
            let uid = uid
                .parse::<u32>()
                .map_err(|_| "owner uid is invalid".to_owned())?;
            let gid = gid
                .parse::<u32>()
                .map_err(|_| "owner gid is invalid".to_owned())?;
            Ok((uid, gid))
        }
    }
}

fn chown_with_owner(
    action: &str,
    path: &Path,
    owner: &str,
    identity: &ResolvedIdentity,
    posix_system: &PosixSystem,
    follow_symlink: bool,
) -> Result<ActionChange, CoreError> {
    let (uid, gid) = owner_ids(owner, identity)
        .map_err(|message| CoreError::action(action, Some(path.to_path_buf()), message))?;
    posix_system
        .chown(path, uid, gid, follow_symlink)
        .map_err(|error| CoreError::from_posix(action, Some(path.to_path_buf()), error))
}

fn temporary_path(path: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    path.parent()
        .unwrap_or_else(|| Path::new("/"))
        .join(format!(
            ".{name}.container-init-{}-{stamp}",
            std::process::id()
        ))
}

fn is_mount_point(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return true;
    };
    match (fs::metadata(path), fs::metadata(parent)) {
        (Ok(path), Ok(parent)) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                path.dev() != parent.dev()
            }
            #[cfg(not(unix))]
            {
                let _ = (path, parent);
                false
            }
        }
        _ => false,
    }
}
