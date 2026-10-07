use crate::error::PosixError;
use crate::filesystem::atomic_replace;
use crate::system::PosixSystem;
use crate::types::{ActionChange, PosixIdentity};
use std::fs;
use std::io;
use std::path::Path;

pub(crate) fn map_user(
    system: &PosixSystem,
    identity: &PosixIdentity,
) -> Result<ActionChange, PosixError> {
    validate_user_name(&identity.user)?;
    validate_account_field("home", &identity.home.to_string_lossy())?;
    if !identity.home.is_absolute() {
        return Err(PosixError::invalid("home path must be absolute"));
    }
    if identity.uid == 0 && identity.user != "root" {
        return Err(PosixError::invalid(
            "UID 0 may only be reconciled as the root account",
        ));
    }
    if identity.user == "root" && identity.uid != 0 {
        return Err(PosixError::invalid(
            "the root account may not be mapped to a non-zero UID",
        ));
    }
    // Root is a stable service account. The normal identity plan skips this
    // action for UID 0; keeping the direct backend call a no-op prevents an
    // explicitly supplied action from rewriting root's home or GID.
    if identity.uid == 0 && identity.user == "root" {
        return Ok(ActionChange::new("root POSIX account left unchanged"));
    }

    let passwd = fs::read_to_string(&system.passwd_path).map_err(PosixError::io)?;
    let (passwd_output, passwd_changed, target_found) = reconcile_passwd(&passwd, identity)?;
    if !target_found {
        return Err(PosixError::invalid(
            "internal passwd reconciliation did not produce a target account",
        ));
    }

    let group = match fs::read_to_string(&system.group_path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(PosixError::io(source)),
    };
    let (group_output, group_changed) = reconcile_group(&group, identity)?;

    // Build and validate both complete files before making either visible.
    // passwd and group cannot be committed transactionally, so preserve the
    // original passwd contents and restore it if the second rename fails.
    if passwd_changed {
        atomic_replace(&system.passwd_path, passwd_output.as_bytes()).map_err(|source| {
            PosixError::invalid(format!(
                "account reconcile failed replacing passwd: {source}"
            ))
        })?;
    }
    if group_changed {
        if let Err(source) = atomic_replace(&system.group_path, group_output.as_bytes()) {
            let rollback = if passwd_changed {
                atomic_replace(&system.passwd_path, passwd.as_bytes()).err()
            } else {
                None
            };
            let message = match rollback {
                Some(rollback) => format!(
                    "account reconcile failed replacing group: {source}; passwd rollback also failed: {rollback}"
                ),
                None => format!("account reconcile failed replacing group: {source}"),
            };
            return Err(PosixError::invalid(message));
        }
    }

    let mapped = system
        .lookup_user_by_name(&identity.user)
        .map_err(PosixError::io)?
        .ok_or_else(|| {
            PosixError::invalid(format!(
                "account reconcile completed but user {:?} cannot be read back",
                identity.user
            ))
        })?;
    if mapped.uid != identity.uid || mapped.gid != identity.gid || mapped.home != identity.home {
        return Err(PosixError::invalid(format!(
            "account reconcile verification failed for {:?}",
            identity.user
        )));
    }

    Ok(ActionChange::new(if passwd_changed {
        "mapped POSIX passwd entry"
    } else {
        "POSIX passwd entry already mapped"
    }))
}

pub(crate) fn set_user_shell(
    system: &PosixSystem,
    user: &str,
    shell: &Path,
) -> Result<ActionChange, PosixError> {
    validate_user_name(user)?;
    if user == "root" {
        return Ok(ActionChange::new("root login shell left unchanged"));
    }
    let shell_value = shell.to_string_lossy();
    validate_account_field("shell", &shell_value)?;
    if !shell.is_absolute() {
        return Err(PosixError::invalid("login shell path must be absolute"));
    }
    let contents = fs::read_to_string(&system.passwd_path).map_err(PosixError::io)?;
    let mut found = false;
    let mut changed = false;
    let mut lines = Vec::new();
    for line in contents.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            lines.push(line.to_owned());
            continue;
        }
        let mut fields = line.split(':').map(str::to_owned).collect::<Vec<_>>();
        if fields.first().map(String::as_str) == Some(user) {
            found = true;
            if fields.len() < 7 {
                return Err(PosixError::invalid(
                    "target passwd entry has fewer than seven fields",
                ));
            }
            if fields[6] != shell_value {
                fields[6] = shell_value.to_string();
                changed = true;
            }
            lines.push(fields.join(":"));
        } else {
            lines.push(line.to_owned());
        }
    }
    if !found {
        return Err(PosixError::invalid(format!(
            "POSIX user {user:?} does not exist"
        )));
    }
    if changed {
        let mut output = lines.join("\n");
        if contents.ends_with('\n') {
            output.push('\n');
        }
        atomic_replace(&system.passwd_path, output.as_bytes()).map_err(PosixError::io)?;
    }
    Ok(ActionChange::new(if changed {
        "updated POSIX login shell"
    } else {
        "POSIX login shell already set"
    }))
}

fn reconcile_passwd(
    contents: &str,
    identity: &PosixIdentity,
) -> Result<(String, bool, bool), PosixError> {
    let mut found = false;
    let mut changed = false;
    let mut lines = Vec::new();
    for (line_number, line) in contents.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            lines.push(line.to_owned());
            continue;
        }
        let mut fields = line.split(':').map(str::to_owned).collect::<Vec<_>>();
        validate_passwd_fields(&fields, line_number + 1)?;
        let entry_uid = fields[2].parse::<u32>().map_err(|_| {
            PosixError::invalid(format!("malformed passwd UID on line {}", line_number + 1))
        })?;
        if entry_uid == identity.uid && fields[0] != identity.user {
            return Err(PosixError::invalid(format!(
                "target UID {} is already occupied by passwd user {:?}",
                identity.uid, fields[0]
            )));
        }
        if fields[0] == identity.user {
            if found {
                return Err(PosixError::invalid(format!(
                    "passwd contains duplicate user {:?}",
                    identity.user
                )));
            }
            found = true;
            for (index, value) in [
                (2, identity.uid.to_string()),
                (3, identity.gid.to_string()),
                (5, identity.home.to_string_lossy().into_owned()),
            ] {
                if fields[index] != value {
                    fields[index] = value;
                    changed = true;
                }
            }
        }
        lines.push(fields.join(":"));
    }
    if !found {
        lines.push(format!(
            "{}:x:{}:{}::{}:/bin/sh",
            identity.user,
            identity.uid,
            identity.gid,
            identity.home.display()
        ));
        changed = true;
        found = true;
    }
    let mut output = lines.join("\n");
    if contents.ends_with('\n') {
        output.push('\n');
    }
    Ok((output, changed, found))
}

fn reconcile_group(contents: &str, identity: &PosixIdentity) -> Result<(String, bool), PosixError> {
    let mut matched = false;
    let mut changed = false;
    let mut lines = Vec::new();
    for (line_number, line) in contents.lines().enumerate() {
        if line.starts_with('#') || line.trim().is_empty() {
            lines.push(line.to_owned());
            continue;
        }
        let mut fields = line.split(':').map(str::to_owned).collect::<Vec<_>>();
        validate_group_fields(&fields, line_number + 1)?;
        let gid = fields[2].parse::<u32>().map_err(|_| {
            PosixError::invalid(format!("malformed group GID on line {}", line_number + 1))
        })?;
        if gid == identity.gid {
            matched = true;
            let members = fields[3]
                .split(',')
                .filter(|member| !member.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let mut reconciled = Vec::with_capacity(members.len() + 1);
            let mut target_seen = false;
            for member in members {
                validate_user_name(&member)?;
                if member == identity.user {
                    if target_seen {
                        changed = true;
                        continue;
                    }
                    target_seen = true;
                }
                reconciled.push(member);
            }
            if !target_seen {
                reconciled.push(identity.user.clone());
                changed = true;
            }
            let value = reconciled.join(",");
            if fields[3] != value {
                fields[3] = value;
                changed = true;
            }
        }
        lines.push(fields.join(":"));
    }
    if !matched {
        if !lines.is_empty() && !contents.ends_with('\n') {
            // The original content has no newline, so separate the new entry.
            // `lines` is joined below and remains byte-for-byte intact otherwise.
        }
        lines.push(format!("{}:x:{}:", identity.user, identity.gid));
        changed = true;
    }
    let mut output = lines.join("\n");
    if contents.ends_with('\n') || changed {
        output.push('\n');
    }
    Ok((output, changed))
}

fn validate_passwd_fields(fields: &[String], line_number: usize) -> Result<(), PosixError> {
    if fields.len() != 7 {
        return Err(PosixError::invalid(format!(
            "malformed passwd entry on line {line_number}: expected seven fields"
        )));
    }
    validate_user_name(&fields[0])?;
    for (field, value) in [("uid", &fields[2]), ("gid", &fields[3])] {
        value.parse::<u32>().map_err(|_| {
            PosixError::invalid(format!("malformed passwd {field} on line {line_number}"))
        })?;
    }
    validate_account_field("home", &fields[5])?;
    validate_account_field("shell", &fields[6])?;
    Ok(())
}

fn validate_group_fields(fields: &[String], line_number: usize) -> Result<(), PosixError> {
    if fields.len() != 4 {
        return Err(PosixError::invalid(format!(
            "malformed group entry on line {line_number}: expected four fields"
        )));
    }
    validate_user_name(&fields[0])?;
    fields[2]
        .parse::<u32>()
        .map_err(|_| PosixError::invalid(format!("malformed group GID on line {line_number}")))?;
    validate_account_field("group members", &fields[3])?;
    for member in fields[3].split(',').filter(|member| !member.is_empty()) {
        validate_user_name(member)?;
    }
    Ok(())
}

fn validate_user_name(user: &str) -> Result<(), PosixError> {
    if user.is_empty()
        || user.len() > 32
        || !user
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(PosixError::invalid(format!(
            "invalid POSIX user name {user:?}"
        )));
    }
    Ok(())
}

fn validate_account_field(field: &str, value: &str) -> Result<(), PosixError> {
    if value
        .chars()
        .any(|character| matches!(character, ':' | '\0' | '\n' | '\r'))
    {
        return Err(PosixError::invalid(format!(
            "POSIX account {field} contains a prohibited character"
        )));
    }
    Ok(())
}
