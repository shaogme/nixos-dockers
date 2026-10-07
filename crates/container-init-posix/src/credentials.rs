use crate::system::PosixSystem;
use crate::types::PosixUser;
use std::ffi::{CStr, CString};
use std::fs;
use std::io;
use std::path::Path;

pub(crate) fn current_ids() -> (u32, u32) {
    unsafe { (libc::geteuid(), libc::getegid()) }
}

pub(crate) fn supplementary_groups_contain(gid: u32) -> Result<bool, crate::error::PosixError> {
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count < 0 {
        return Err(crate::error::PosixError::io(io::Error::last_os_error()));
    }
    let mut groups = vec![0 as libc::gid_t; count as usize];
    let result = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
    if result < 0 {
        return Err(crate::error::PosixError::io(io::Error::last_os_error()));
    }
    Ok(groups[..result as usize].contains(&(gid as libc::gid_t)))
}

pub(crate) fn lookup_user_by_name(
    system: &PosixSystem,
    name: &str,
) -> io::Result<Option<PosixUser>> {
    if let Some(user) = lookup_file_user(&system.passwd_path, |fields| {
        fields.first().map(String::as_str) == Some(name)
    })? {
        return Ok(Some(user));
    }
    if system.passwd_path != Path::new("/etc/passwd") {
        return Ok(None);
    }
    let name = CString::new(name).map_err(|_| invalid_c_string("user name"))?;
    // libc's passwd result points into process-global storage. Copy every
    // field before returning so later lookups cannot invalidate it.
    let result = unsafe { libc::getpwnam(name.as_ptr()) };
    Ok(unsafe { result.as_ref().map(copy_user) })
}

pub(crate) fn lookup_user_by_uid(system: &PosixSystem, uid: u32) -> io::Result<Option<PosixUser>> {
    if let Some(user) = lookup_file_user(&system.passwd_path, |fields| {
        fields.get(2).and_then(|value| value.parse::<u32>().ok()) == Some(uid)
    })? {
        return Ok(Some(user));
    }
    if system.passwd_path != Path::new("/etc/passwd") {
        return Ok(None);
    }
    let result = unsafe { libc::getpwuid(uid as libc::uid_t) };
    Ok(unsafe { result.as_ref().map(copy_user) })
}

fn copy_user(entry: &libc::passwd) -> PosixUser {
    PosixUser {
        name: c_string(entry.pw_name),
        uid: entry.pw_uid,
        gid: entry.pw_gid,
        home: c_string(entry.pw_dir).into(),
        shell: c_string(entry.pw_shell).into(),
    }
}

fn invalid_c_string(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("{what} contains NUL"))
}

fn c_string(value: *const libc::c_char) -> String {
    if value.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    }
}

fn lookup_file_user(
    path: &Path,
    predicate: impl Fn(&[String]) -> bool,
) -> io::Result<Option<PosixUser>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source),
    };
    for (line_number, line) in contents.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let fields = line.split(':').map(str::to_owned).collect::<Vec<_>>();
        if fields.len() != 7 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("malformed passwd entry on line {}", line_number + 1),
            ));
        }
        if fields[0].is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "passwd entry on line {} has an empty user name",
                    line_number + 1
                ),
            ));
        }
        let uid = fields[2].parse::<u32>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("malformed passwd UID on line {}", line_number + 1),
            )
        })?;
        let gid = fields[3].parse::<u32>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("malformed passwd GID on line {}", line_number + 1),
            )
        })?;
        if predicate(&fields) {
            return Ok(Some(PosixUser {
                name: fields[0].clone(),
                uid,
                gid,
                home: fields[5].clone().into(),
                shell: fields[6].clone().into(),
            }));
        }
    }
    Ok(None)
}
