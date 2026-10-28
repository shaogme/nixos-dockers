use crate::{ActionChange, PosixError, PosixIdentity, PosixSystem, PosixUser};
use libc::{
    c_char, getegid, geteuid, getgroups, getpwnam, getpwuid, gid_t, initgroups, passwd, setgid,
    setgroups, setuid, uid_t,
};
use std::{
    ffi::{CStr, CString},
    fs,
    io::{self, Error, ErrorKind},
    path::Path,
    ptr::null_mut,
};

pub(crate) struct Credentials;

impl Credentials {
    pub(crate) fn current_ids() -> (u32, u32) {
        unsafe { (geteuid(), getegid()) }
    }

    pub(crate) fn supplementary_groups_contain(gid: u32) -> Result<bool, PosixError> {
        let count = unsafe { getgroups(0, null_mut()) };
        if count < 0 {
            return Err(PosixError::io(Error::last_os_error()));
        }
        let mut groups = vec![0 as gid_t; count as usize];
        let result = unsafe { getgroups(count, groups.as_mut_ptr()) };
        if result < 0 {
            return Err(PosixError::io(Error::last_os_error()));
        }
        Ok(groups[..result as usize].contains(&(gid as gid_t)))
    }

    pub(crate) fn lookup_user_by_name(
        system: &PosixSystem,
        name: &str,
    ) -> io::Result<Option<PosixUser>> {
        if let Some(user) = Self::lookup_file_user(system.passwd_path(), |fields| {
            fields.first().map(String::as_str) == Some(name)
        })? {
            return Ok(Some(user));
        }
        if system.passwd_path() != Path::new("/etc/passwd") {
            return Ok(None);
        }
        let name = CString::new(name).map_err(|_| Self::invalid_c_string("user name"))?;
        // libc's passwd result points into process-global storage. Copy every
        // field before returning so later lookups cannot invalidate it.
        let result = unsafe { getpwnam(name.as_ptr()) };
        Ok(unsafe { result.as_ref().map(Self::copy_user) })
    }

    pub(crate) fn lookup_user_by_uid(
        system: &PosixSystem,
        uid: u32,
    ) -> io::Result<Option<PosixUser>> {
        if let Some(user) = Self::lookup_file_user(system.passwd_path(), |fields| {
            fields.get(2).and_then(|value| value.parse::<u32>().ok()) == Some(uid)
        })? {
            return Ok(Some(user));
        }
        if system.passwd_path() != Path::new("/etc/passwd") {
            return Ok(None);
        }
        let result = unsafe { getpwuid(uid as uid_t) };
        Ok(unsafe { result.as_ref().map(Self::copy_user) })
    }

    pub(crate) fn drop_privileges(
        system: &PosixSystem,
        identity: &PosixIdentity,
    ) -> Result<ActionChange, PosixError> {
        let (current_uid, current_gid) = Self::current_ids();
        if current_uid == identity.uid()
            && current_gid == identity.gid()
            && Self::supplementary_groups_contain(identity.gid())?
        {
            return Ok(ActionChange::new("already running as target identity"));
        }
        if current_uid != 0 {
            return Err(PosixError::permission(
                "dropping to another identity requires effective root",
            ));
        }

        let name = CString::new(identity.user())
            .map_err(|_| PosixError::invalid("user name contains NUL"))?;
        if Self::lookup_user_by_name(system, identity.user())
            .map_err(PosixError::io)?
            .is_some()
        {
            let result = unsafe { initgroups(name.as_ptr(), identity.gid() as gid_t) };
            if result != 0 {
                return Err(PosixError::io(Error::last_os_error()));
            }
        } else {
            let groups = [identity.gid() as gid_t];
            if unsafe { setgroups(groups.len(), groups.as_ptr()) } != 0 {
                return Err(PosixError::io(Error::last_os_error()));
            }
        }
        if unsafe { setgid(identity.gid() as gid_t) } != 0 {
            return Err(PosixError::io(Error::last_os_error()));
        }
        if unsafe { setuid(identity.uid() as uid_t) } != 0 {
            return Err(PosixError::io(Error::last_os_error()));
        }
        let (effective_uid, effective_gid) = Self::current_ids();
        if effective_uid != identity.uid() || effective_gid != identity.gid() {
            return Err(PosixError::permission(format!(
                "credential verification failed: expected {}:{}, got {}:{}",
                identity.uid(),
                identity.gid(),
                effective_uid,
                effective_gid
            )));
        }
        if !Self::supplementary_groups_contain(identity.gid())? {
            return Err(PosixError::permission(
                "credential verification failed: target GID is absent from supplementary groups",
            ));
        }
        Ok(ActionChange::new("dropped privileges to target identity"))
    }

    fn copy_user(entry: &passwd) -> PosixUser {
        PosixUser {
            name: Self::c_string(entry.pw_name),
            uid: entry.pw_uid,
            gid: entry.pw_gid,
            home: Self::c_string(entry.pw_dir).into(),
            shell: Self::c_string(entry.pw_shell).into(),
        }
    }

    fn invalid_c_string(what: &str) -> io::Error {
        io::Error::new(ErrorKind::InvalidInput, format!("{what} contains NUL"))
    }

    fn c_string(value: *const c_char) -> String {
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
            Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(source),
        };
        for (line_number, line) in contents.lines().enumerate() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let fields = line.split(':').map(str::to_owned).collect::<Vec<_>>();
            if fields.len() != 7 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!("malformed passwd entry on line {}", line_number + 1),
                ));
            }
            if fields[0].is_empty() {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "passwd entry on line {} has an empty user name",
                        line_number + 1
                    ),
                ));
            }
            let uid = fields[2].parse::<u32>().map_err(|_| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("malformed passwd UID on line {}", line_number + 1),
                )
            })?;
            let gid = fields[3].parse::<u32>().map_err(|_| {
                Error::new(
                    ErrorKind::InvalidData,
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
}
