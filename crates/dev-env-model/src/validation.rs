use crate::{ModelError, ModelErrorReason};

pub(crate) trait StringValidation {
    fn is_env_name(&self) -> bool;

    fn is_posix_env_name(&self) -> bool;

    fn validate_env_name(&self, location: &str) -> Result<(), ModelError>;

    fn is_identifier(&self) -> bool;

    fn is_config_path(&self) -> bool;

    fn validate_id(&self, location: &str) -> Result<(), ModelError>;

    fn validate_config_path(&self, location: &str) -> Result<(), ModelError>;

    fn validate_nonempty_text(
        &self,
        location: &str,
        allow_newlines: bool,
    ) -> Result<(), ModelError>;

    fn validate_command_arg(&self, location: &str) -> Result<(), ModelError>;

    fn validate_path(&self, location: &str) -> Result<(), ModelError>;

    fn validate_concrete_path(&self, location: &str) -> Result<(), ModelError>;

    fn is_reference(&self) -> bool;

    fn validate_glob(&self, location: &str) -> Result<(), ModelError>;
}

impl StringValidation for str {
    fn is_env_name(&self) -> bool {
        let mut characters = self.chars();
        matches!(
            characters.next(),
            Some(character) if character.is_ascii_uppercase() || character == '_'
        ) && characters.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
    }

    fn is_posix_env_name(&self) -> bool {
        let mut characters = self.chars();
        matches!(
            characters.next(),
            Some(character) if character.is_ascii_alphabetic() || character == '_'
        ) && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
    }

    fn validate_env_name(&self, location: &str) -> Result<(), ModelError> {
        if !self.is_env_name() || self.contains('=') || self.contains('\0') {
            return Err(ModelError::InvalidEnvironmentName {
                location: location.to_owned(),
                name: self.to_owned(),
            });
        }
        Ok(())
    }

    fn is_identifier(&self) -> bool {
        !self.is_empty()
            && self.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
            })
    }

    fn is_config_path(&self) -> bool {
        if self.is_empty() {
            return false;
        }
        let parts = self.split('.').collect::<Vec<_>>();
        !parts.iter().any(|part| part.is_empty() || *part == "..")
            && parts.iter().enumerate().all(|(index, part)| {
                (*part == "*" && index + 1 == parts.len())
                    || part.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                    })
            })
    }

    fn validate_id(&self, location: &str) -> Result<(), ModelError> {
        if !self.is_identifier() {
            return Err(ModelError::InvalidValue {
                location: location.to_owned(),
                reason: ModelErrorReason::InvalidIdentifier,
            });
        }
        Ok(())
    }

    fn validate_config_path(&self, location: &str) -> Result<(), ModelError> {
        if !self.is_config_path() {
            return Err(ModelError::InvalidValue {
                location: location.to_owned(),
                reason: ModelErrorReason::InvalidIdentifier,
            });
        }
        Ok(())
    }

    fn validate_nonempty_text(
        &self,
        location: &str,
        allow_newlines: bool,
    ) -> Result<(), ModelError> {
        if self.is_empty() {
            return Err(ModelError::InvalidValue {
                location: location.to_owned(),
                reason: ModelErrorReason::Empty,
            });
        }
        if self.contains('\0') {
            return Err(ModelError::InvalidValue {
                location: location.to_owned(),
                reason: ModelErrorReason::Nul,
            });
        }
        if !allow_newlines && (self.contains('\n') || self.contains('\r')) {
            return Err(ModelError::InvalidValue {
                location: location.to_owned(),
                reason: ModelErrorReason::Newline,
            });
        }
        Ok(())
    }

    fn validate_command_arg(&self, location: &str) -> Result<(), ModelError> {
        self.validate_nonempty_text(location, false)
    }

    fn validate_path(&self, location: &str) -> Result<(), ModelError> {
        self.validate_nonempty_text(location, false)?;
        if !self.starts_with('/') && !self.starts_with("${") {
            return Err(ModelError::InvalidPath {
                location: location.to_owned(),
                reason: ModelErrorReason::AbsolutePathRequired,
            });
        }
        if self.split('/').any(|component| component == "..") {
            return Err(ModelError::InvalidPath {
                location: location.to_owned(),
                reason: ModelErrorReason::PathTraversal,
            });
        }
        if self.contains("$(")
            || self.contains('`')
            || self.contains(';')
            || self.contains('|')
            || self.contains('&')
            || self.contains('>')
            || self.contains('<')
        {
            return Err(ModelError::InvalidPath {
                location: location.to_owned(),
                reason: ModelErrorReason::ShellMetacharacter,
            });
        }
        if self.contains('$') && !self.contains("${") {
            return Err(ModelError::InvalidPath {
                location: location.to_owned(),
                reason: ModelErrorReason::ShellMetacharacter,
            });
        }
        Ok(())
    }

    fn validate_concrete_path(&self, location: &str) -> Result<(), ModelError> {
        self.validate_path(location)?;
        if self.contains("${") {
            return Err(ModelError::InvalidPath {
                location: location.to_owned(),
                reason: ModelErrorReason::InvalidPath,
            });
        }
        Ok(())
    }

    fn is_reference(&self) -> bool {
        let Some((namespace, name)) = self.split_once('.') else {
            return false;
        };
        match namespace {
            "workspace" => matches!(name, "root" | "cwd"),
            "context" => matches!(name, "cwd" | "os" | "arch"),
            "input" | "env" => name.is_env_name(),
            "config" | "features" => name.is_config_path(),
            _ => false,
        }
    }

    fn validate_glob(&self, location: &str) -> Result<(), ModelError> {
        self.validate_nonempty_text(location, false)?;
        if self.starts_with('/') || self.split('/').any(|component| component == "..") {
            return Err(ModelError::InvalidPath {
                location: location.to_owned(),
                reason: ModelErrorReason::InvalidPath,
            });
        }
        if self.contains("$(")
            || self.contains('`')
            || self.contains(';')
            || self.contains('|')
            || self.contains('&')
            || self.contains('>')
            || self.contains('<')
        {
            return Err(ModelError::InvalidPath {
                location: location.to_owned(),
                reason: ModelErrorReason::ShellMetacharacter,
            });
        }
        Ok(())
    }
}
