use crate::error::PosixError;
use crate::types::ActionChange;
use std::ffi::{CString, OsStr};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Parse the octal mode representation used by the Bootstrap DSL.
pub fn parse_mode(value: &str) -> Result<u32, String> {
    if value.is_empty() || value.len() > 4 || !value.chars().all(|c| ('0'..='7').contains(&c)) {
        return Err("mode must be an octal string containing at most four digits".to_owned());
    }
    u32::from_str_radix(value, 8).map_err(|_| "mode is outside the supported range".to_owned())
}

/// Compatibility spelling for callers that use the model's short helper.
pub fn mode(value: &str) -> Result<u32, String> {
    parse_mode(value)
}

pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

pub(crate) fn chown(
    path: &Path,
    uid: u32,
    gid: u32,
    follow_symlink: bool,
) -> Result<ActionChange, PosixError> {
    let current = fs::symlink_metadata(path).map_err(PosixError::io)?;
    if current.uid() == uid && current.gid() == gid {
        return Ok(ActionChange::new("ownership already matches"));
    }
    chown_path(path, uid, gid, follow_symlink).map_err(PosixError::io)?;
    Ok(ActionChange::new("updated ownership"))
}

pub(crate) fn chown_path(path: &Path, uid: u32, gid: u32, follow_symlink: bool) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid_c_string("path"))?;
    let result = unsafe {
        if follow_symlink {
            libc::chown(path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t)
        } else {
            libc::lchown(path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t)
        }
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(crate) fn atomic_replace(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no valid file name")
    })?;
    let temporary = parent.join(format!(".{name}.container-init-{}", std::process::id()));
    let file_mode = fs::symlink_metadata(path)
        .map(|metadata| metadata.mode() & 0o7777)
        .unwrap_or(0o644);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(file_mode)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn invalid_c_string(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("{what} contains NUL"))
}
