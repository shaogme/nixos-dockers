use crate::credentials;
use crate::error::PosixError;
use crate::system::PosixSystem;
use crate::types::{ActionChange, PosixIdentity};
use std::ffi::CString;
use std::io;

pub(crate) fn drop_privileges(
    system: &PosixSystem,
    identity: &PosixIdentity,
) -> Result<ActionChange, PosixError> {
    let (current_uid, current_gid) = credentials::current_ids();
    if current_uid == identity.uid
        && current_gid == identity.gid
        && credentials::supplementary_groups_contain(identity.gid)?
    {
        return Ok(ActionChange::new("already running as target identity"));
    }
    if current_uid != 0 {
        return Err(PosixError::permission(
            "dropping to another identity requires effective root",
        ));
    }

    let name = CString::new(identity.user.as_str())
        .map_err(|_| PosixError::invalid("user name contains NUL"))?;
    if system
        .lookup_user_by_name(&identity.user)
        .map_err(PosixError::io)?
        .is_some()
    {
        let result = unsafe { libc::initgroups(name.as_ptr(), identity.gid as libc::gid_t) };
        if result != 0 {
            return Err(PosixError::io(io::Error::last_os_error()));
        }
    } else {
        let groups = [identity.gid as libc::gid_t];
        if unsafe { libc::setgroups(groups.len(), groups.as_ptr()) } != 0 {
            return Err(PosixError::io(io::Error::last_os_error()));
        }
    }
    if unsafe { libc::setgid(identity.gid as libc::gid_t) } != 0 {
        return Err(PosixError::io(io::Error::last_os_error()));
    }
    if unsafe { libc::setuid(identity.uid as libc::uid_t) } != 0 {
        return Err(PosixError::io(io::Error::last_os_error()));
    }
    let (effective_uid, effective_gid) = credentials::current_ids();
    if effective_uid != identity.uid || effective_gid != identity.gid {
        return Err(PosixError::permission(format!(
            "credential verification failed: expected {}:{}, got {}:{}",
            identity.uid, identity.gid, effective_uid, effective_gid
        )));
    }
    if !credentials::supplementary_groups_contain(identity.gid)? {
        return Err(PosixError::permission(
            "credential verification failed: target GID is absent from supplementary groups",
        ));
    }
    Ok(ActionChange::new("dropped privileges to target identity"))
}
