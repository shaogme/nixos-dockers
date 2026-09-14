use crate::{ModelError, ModelErrorReason};

pub(crate) fn is_env_name(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(
        characters.next(),
        Some(character) if character.is_ascii_uppercase() || character == '_'
    ) && characters.all(|character| {
        character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
    })
}

pub(crate) fn validate_env_name(location: &str, value: &str) -> Result<(), ModelError> {
    if !is_env_name(value) || value.contains('=') || value.contains('\0') {
        return Err(ModelError::InvalidEnvironmentName {
            location: location.to_owned(),
            name: value.to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}

pub(crate) fn is_config_path(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    let parts = value.split('.').collect::<Vec<_>>();
    !parts.iter().any(|part| part.is_empty() || *part == "..")
        && parts.iter().enumerate().all(|(index, part)| {
            (*part == "*" && index + 1 == parts.len())
                || part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                })
        })
}

pub(crate) fn validate_id(location: &str, value: &str) -> Result<(), ModelError> {
    if !is_identifier(value) {
        return Err(ModelError::InvalidValue {
            location: location.to_owned(),
            reason: ModelErrorReason::InvalidIdentifier,
        });
    }
    Ok(())
}

pub(crate) fn validate_config_path(location: &str, value: &str) -> Result<(), ModelError> {
    if !is_config_path(value) {
        return Err(ModelError::InvalidValue {
            location: location.to_owned(),
            reason: ModelErrorReason::InvalidIdentifier,
        });
    }
    Ok(())
}

pub(crate) fn validate_nonempty_text(
    location: &str,
    value: &str,
    allow_newlines: bool,
) -> Result<(), ModelError> {
    if value.is_empty() {
        return Err(ModelError::InvalidValue {
            location: location.to_owned(),
            reason: ModelErrorReason::Empty,
        });
    }
    if value.contains('\0') {
        return Err(ModelError::InvalidValue {
            location: location.to_owned(),
            reason: ModelErrorReason::Nul,
        });
    }
    if !allow_newlines && (value.contains('\n') || value.contains('\r')) {
        return Err(ModelError::InvalidValue {
            location: location.to_owned(),
            reason: ModelErrorReason::Newline,
        });
    }
    Ok(())
}

pub(crate) fn validate_command_arg(location: &str, value: &str) -> Result<(), ModelError> {
    validate_nonempty_text(location, value, false)?;
    // One value is passed as one argv element.  Spaces and shell
    // metacharacters are data here; NUL/newline have already been rejected.
    Ok(())
}

pub(crate) fn validate_path(location: &str, value: &str) -> Result<(), ModelError> {
    validate_nonempty_text(location, value, false)?;
    if !value.starts_with('/') && !value.starts_with("${") {
        return Err(ModelError::InvalidPath {
            location: location.to_owned(),
            reason: ModelErrorReason::AbsolutePathRequired,
        });
    }
    if value.split('/').any(|component| component == "..") {
        return Err(ModelError::InvalidPath {
            location: location.to_owned(),
            reason: ModelErrorReason::PathTraversal,
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
        return Err(ModelError::InvalidPath {
            location: location.to_owned(),
            reason: ModelErrorReason::ShellMetacharacter,
        });
    }
    if value.contains('$') && !value.contains("${") {
        return Err(ModelError::InvalidPath {
            location: location.to_owned(),
            reason: ModelErrorReason::ShellMetacharacter,
        });
    }
    Ok(())
}

pub(crate) fn validate_concrete_path(location: &str, value: &str) -> Result<(), ModelError> {
    validate_path(location, value)?;
    if value.contains("${") {
        return Err(ModelError::InvalidPath {
            location: location.to_owned(),
            reason: ModelErrorReason::InvalidPath,
        });
    }
    Ok(())
}

pub(crate) fn is_reference(value: &str) -> bool {
    let Some((namespace, name)) = value.split_once('.') else {
        return false;
    };
    match namespace {
        "workspace" => matches!(name, "root" | "cwd"),
        "context" => matches!(name, "cwd" | "os" | "arch"),
        "input" | "env" => is_env_name(name),
        "config" | "features" => is_config_path(name),
        _ => false,
    }
}

pub(crate) fn validate_glob(location: &str, value: &str) -> Result<(), ModelError> {
    validate_nonempty_text(location, value, false)?;
    if value.starts_with('/') || value.split('/').any(|component| component == "..") {
        return Err(ModelError::InvalidPath {
            location: location.to_owned(),
            reason: ModelErrorReason::InvalidPath,
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
        return Err(ModelError::InvalidPath {
            location: location.to_owned(),
            reason: ModelErrorReason::ShellMetacharacter,
        });
    }
    Ok(())
}
