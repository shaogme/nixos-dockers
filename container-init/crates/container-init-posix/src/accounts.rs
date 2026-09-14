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
    let passwd = fs::read_to_string(&system.passwd_path).map_err(PosixError::io)?;
    let mut found = false;
    let mut changed = false;
    let mut lines = Vec::new();
    for line in passwd.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            lines.push(line.to_owned());
            continue;
        }
        let mut fields = line.split(':').map(str::to_owned).collect::<Vec<_>>();
        if fields.first().map(String::as_str) == Some(identity.user.as_str()) {
            found = true;
            if fields.len() < 7 {
                return Err(PosixError::invalid(
                    "target passwd entry has fewer than seven fields",
                ));
            }
            if fields[2] != identity.uid.to_string() {
                fields[2] = identity.uid.to_string();
                changed = true;
            }
            if fields[3] != identity.gid.to_string() {
                fields[3] = identity.gid.to_string();
                changed = true;
            }
            lines.push(fields.join(":"));
        } else {
            lines.push(line.to_owned());
        }
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
    }
    let mut output = lines.join("\n");
    if passwd.ends_with('\n') {
        output.push('\n');
    }
    if changed {
        atomic_replace(&system.passwd_path, output.as_bytes()).map_err(PosixError::io)?;
    }

    ensure_group(system, identity)?;
    Ok(ActionChange::new(if changed {
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

fn ensure_group(system: &PosixSystem, identity: &PosixIdentity) -> Result<(), PosixError> {
    let contents = match fs::read_to_string(&system.group_path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(PosixError::io(source)),
    };
    let has_gid = contents
        .lines()
        .filter_map(|line| line.split(':').nth(2))
        .any(|gid| gid.parse::<u32>().ok() == Some(identity.gid));
    if has_gid {
        return Ok(());
    }
    let mut output = contents;
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(&format!("{}:x:{}:\n", identity.user, identity.gid));
    atomic_replace(&system.group_path, output.as_bytes()).map_err(PosixError::io)
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
            "POSIX passwd {field} contains a prohibited character"
        )));
    }
    Ok(())
}
