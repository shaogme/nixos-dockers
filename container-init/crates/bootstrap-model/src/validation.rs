use crate::error::ModelError;

pub(crate) fn validate_path_template(location: &str, value: &str) -> Result<(), ModelError> {
    if value.is_empty() {
        return Err(ModelError::Invalid {
            location: location.to_owned(),
            message: "path may not be empty".to_owned(),
        });
    }
    if value.contains('\0') {
        return Err(ModelError::Invalid {
            location: location.to_owned(),
            message: "path may not contain NUL".to_owned(),
        });
    }
    if value.contains("$(")
        || value.contains('`')
        || value.contains(';')
        || value.contains('|')
        || value.contains('&')
        || value.contains('>')
        || value.contains('<')
    {
        return Err(ModelError::Invalid {
            location: location.to_owned(),
            message: "shell expressions are not allowed in paths".to_owned(),
        });
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(ModelError::Invalid {
            location: location.to_owned(),
            message: "path may not contain newlines".to_owned(),
        });
    }
    validate_interpolations(location, value)?;
    if !value.starts_with('/') && !value.starts_with("${") {
        return Err(ModelError::Invalid {
            location: location.to_owned(),
            message: "path must be absolute or start with a structured interpolation".to_owned(),
        });
    }
    for component in value.split('/') {
        if component == ".." {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "path traversal with '..' is not allowed".to_owned(),
            });
        }
    }
    Ok(())
}

pub(crate) fn validate_rendered_path(value: &str) -> Result<(), String> {
    if value.is_empty() || !value.starts_with('/') {
        return Err("rendered path must be absolute".to_owned());
    }
    if value.contains('\0') {
        return Err("rendered path may not contain NUL".to_owned());
    }
    if value.contains('\n') || value.contains('\r') {
        return Err("rendered path may not contain newlines".to_owned());
    }
    if value.contains("$(")
        || value.contains('`')
        || value.contains(';')
        || value.contains('|')
        || value.contains('&')
        || value.contains('>')
        || value.contains('<')
    {
        return Err("rendered path may not contain shell metacharacters".to_owned());
    }
    if value.split('/').any(|component| component == "..") {
        return Err("rendered path may not contain '..'".to_owned());
    }
    Ok(())
}

fn validate_interpolations(location: &str, value: &str) -> Result<(), ModelError> {
    let mut cursor = 0;
    while cursor < value.len() {
        let character = value[cursor..]
            .chars()
            .next()
            .expect("cursor is a character boundary");
        if character != '$' && character != '{' && character != '}' {
            cursor += character.len_utf8();
            continue;
        }
        if character != '$' || !value[cursor..].starts_with("${") {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "only ${namespace.reference} interpolation is allowed".to_owned(),
            });
        }
        let after_start = &value[cursor + 2..];
        let end = after_start.find('}').ok_or_else(|| ModelError::Invalid {
            location: location.to_owned(),
            message: "unterminated structured interpolation".to_owned(),
        })?;
        let reference = &after_start[..end];
        if !is_reference(reference) {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: format!("unsupported structured reference {reference:?}"),
            });
        }
        cursor += 2 + end + 1;
    }
    Ok(())
}

pub(crate) fn is_reference(value: &str) -> bool {
    let (namespace, name) = value.split_once('.').unwrap_or(("", ""));
    match namespace {
        "bootstrap" => name == "workspace_root",
        "identity" => matches!(name, "uid" | "gid" | "user" | "home" | "target"),
        "context" => matches!(name, "cwd" | "os" | "arch"),
        "input" => is_env_name(name),
        "config" => is_config_path(name),
        "env" => is_env_name(name),
        _ => false,
    }
}

pub(crate) fn is_config_path(value: &str) -> bool {
    !value.is_empty()
        && !value.split('.').any(|part| part.is_empty() || part == "..")
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

pub(crate) fn is_bootstrap_target(value: &str) -> bool {
    matches!(
        value,
        "identity.uid" | "identity.gid" | "identity.run_as_root" | "identity.home"
    )
}

pub(crate) fn is_env_name(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(characters.next(), Some(character) if character.is_ascii_uppercase() || character == '_')
        && characters.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

pub(crate) fn is_action_id(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

pub(crate) fn validate_executable(location: &str, value: &str) -> Result<(), ModelError> {
    validate_argv_value(location, value)?;
    if value.contains("${") || value.chars().any(char::is_whitespace) {
        return Err(ModelError::Invalid {
            location: location.to_owned(),
            message: "executable path must be a concrete single path".to_owned(),
        });
    }
    validate_path_template(location, value)?;
    if !value.starts_with('/') {
        return Err(ModelError::Invalid {
            location: location.to_owned(),
            message: "executable path must be absolute".to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn validate_argv_value(location: &str, value: &str) -> Result<(), ModelError> {
    if value.is_empty() || value.contains('\0') || value.contains('\n') || value.contains('\r') {
        return Err(ModelError::Invalid {
            location: location.to_owned(),
            message: "argv value must be non-empty and contain no NUL/newline".to_owned(),
        });
    }
    if value.contains("$(") || value.contains('`') || value.contains(';') || value.contains('|') {
        return Err(ModelError::Invalid {
            location: location.to_owned(),
            message: "shell expressions are not allowed in argv values".to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn validate_user_name(location: &str, value: &str) -> Result<(), ModelError> {
    if value.is_empty()
        || value.len() > 32
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(ModelError::Invalid {
            location: location.to_owned(),
            message: format!("invalid POSIX user name {value:?}"),
        });
    }
    Ok(())
}

pub(crate) fn validate_owner(location: &str, value: &str) -> Result<(), ModelError> {
    if value == "root" || value == "identity.target" {
        return Ok(());
    }
    let Some((uid, gid)) = value.split_once(':') else {
        return Err(ModelError::Invalid {
            location: location.to_owned(),
            message: "owner must be root, identity.target, or uid:gid".to_owned(),
        });
    };
    parse_id("uid", uid).map_err(|_| ModelError::Invalid {
        location: location.to_owned(),
        message: "owner uid:gid contains an invalid uid".to_owned(),
    })?;
    parse_id("gid", gid).map_err(|_| ModelError::Invalid {
        location: location.to_owned(),
        message: "owner uid:gid contains an invalid gid".to_owned(),
    })?;
    Ok(())
}

pub(crate) fn parse_id(kind: &str, value: &str) -> Result<u32, ModelError> {
    if value.is_empty() || !value.chars().all(|character| character.is_ascii_digit()) {
        return Err(ModelError::Invalid {
            location: format!("bootstrap input {kind}"),
            message: format!("{value:?} is not a valid {kind}"),
        });
    }
    value.parse::<u32>().map_err(|_| ModelError::Invalid {
        location: format!("bootstrap input {kind}"),
        message: format!("{value:?} is outside the supported {kind} range"),
    })
}

pub(crate) fn parse_mode(value: &str) -> Result<u32, String> {
    if value.is_empty()
        || value.len() > 4
        || !value
            .chars()
            .all(|character| ('0'..='7').contains(&character))
    {
        return Err("mode must be an octal string containing at most four digits".to_owned());
    }
    u32::from_str_radix(value, 8).map_err(|_| "mode is outside the supported range".to_owned())
}
