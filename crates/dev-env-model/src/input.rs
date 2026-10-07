use serde::{Deserialize, Serialize};
use std::fmt;

use crate::validation::{
    is_env_name, validate_concrete_path, validate_config_path, validate_env_name,
};
use crate::{ModelError, Sensitivity, ValueTree};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    Bool,
    Enum,
    Integer,
    Path,
    String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum InputValue {
    Bool(bool),
    Integer(i64),
    String(String),
}

impl InputValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    fn as_raw(&self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Integer(value) => value.to_string(),
            Self::String(value) => value.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ParsedInput {
    Bool(bool),
    Enum(String),
    Integer(i64),
    Path(String),
    String(String),
}

impl ParsedInput {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Enum(value) | Self::Path(value) | Self::String(value) => Some(value),
            Self::Bool(_) | Self::Integer(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum InputError {
    InvalidName,
    InvalidTarget,
    InvalidEnumValue { value: String },
    InvalidAlias { alias: String },
    AliasTargetMissing { alias: String, target: String },
    WrongDefaultType,
    InvalidBoolean { value: String },
    InvalidInteger { value: String },
    InvalidPath,
    InvalidString,
    NotRuntime,
}

impl fmt::Display for InputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => {
                formatter.write_str("input name must be an uppercase environment name")
            }
            Self::InvalidTarget => formatter.write_str("input target must be a valid config path"),
            Self::InvalidEnumValue { value } => write!(formatter, "invalid enum value {value:?}"),
            Self::InvalidAlias { alias } => write!(formatter, "invalid alias {alias:?}"),
            Self::AliasTargetMissing { alias, target } => {
                write!(
                    formatter,
                    "alias {alias:?} maps to undeclared enum value {target:?}"
                )
            }
            Self::WrongDefaultType => formatter.write_str("default has the wrong type"),
            Self::InvalidBoolean { value } => write!(formatter, "invalid boolean {value:?}"),
            Self::InvalidInteger { value } => write!(formatter, "invalid integer {value:?}"),
            Self::InvalidPath => formatter.write_str("input is not a safe absolute path"),
            Self::InvalidString => formatter.write_str("input string contains NUL"),
            Self::NotRuntime => formatter.write_str("input is not enabled at runtime"),
        }
    }
}

impl std::error::Error for InputError {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputSpec {
    pub target: String,
    #[serde(rename = "type")]
    pub input_type: InputType,
    #[serde(default)]
    pub values: Vec<String>,
    #[serde(default)]
    pub aliases: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub runtime: bool,
    pub export_as: Option<String>,
    pub default: Option<InputValue>,
    #[serde(default)]
    pub sensitivity: Sensitivity,
}

impl InputSpec {
    pub fn validate(&self, name: &str) -> Result<(), ModelError> {
        validate_env_name(&format!("inputs.{name}"), name).map_err(|_| {
            ModelError::InvalidInput {
                name: name.to_owned(),
                reason: InputError::InvalidName,
            }
        })?;
        validate_config_path(&format!("inputs.{name}.target"), &self.target).map_err(|_| {
            ModelError::InvalidInput {
                name: name.to_owned(),
                reason: InputError::InvalidTarget,
            }
        })?;
        if self.target.contains('*') {
            return Err(ModelError::InvalidInput {
                name: name.to_owned(),
                reason: InputError::InvalidTarget,
            });
        }
        if let Some(export_as) = &self.export_as {
            if !is_env_name(export_as) {
                return Err(ModelError::InvalidInput {
                    name: name.to_owned(),
                    reason: InputError::InvalidName,
                });
            }
        }
        if self.input_type == InputType::Enum {
            if self.values.is_empty() {
                return Err(ModelError::InvalidInput {
                    name: name.to_owned(),
                    reason: InputError::InvalidEnumValue {
                        value: "<empty enum>".to_owned(),
                    },
                });
            }
            for value in &self.values {
                if value.is_empty()
                    || value.contains('\0')
                    || self.values.iter().filter(|item| *item == value).count() > 1
                {
                    return Err(ModelError::InvalidInput {
                        name: name.to_owned(),
                        reason: InputError::InvalidEnumValue {
                            value: value.clone(),
                        },
                    });
                }
            }
            for (alias, target) in &self.aliases {
                if alias.is_empty() || alias.contains('\0') {
                    return Err(ModelError::InvalidInput {
                        name: name.to_owned(),
                        reason: InputError::InvalidAlias {
                            alias: alias.clone(),
                        },
                    });
                }
                if !self.values.iter().any(|value| value == target) {
                    return Err(ModelError::InvalidInput {
                        name: name.to_owned(),
                        reason: InputError::AliasTargetMissing {
                            alias: alias.clone(),
                            target: target.clone(),
                        },
                    });
                }
            }
        } else if !self.values.is_empty() || !self.aliases.is_empty() {
            return Err(ModelError::InvalidInput {
                name: name.to_owned(),
                reason: InputError::InvalidEnumValue {
                    value: "values and aliases are only valid for enum inputs".to_owned(),
                },
            });
        }
        if let Some(default) = &self.default {
            self.validate_default(default)
                .map_err(|reason| ModelError::InvalidInput {
                    name: name.to_owned(),
                    reason,
                })?;
        }
        Ok(())
    }

    fn validate_default(&self, default: &InputValue) -> Result<(), InputError> {
        match self.input_type {
            InputType::Bool if matches!(default, InputValue::Bool(_)) => Ok(()),
            InputType::Integer if matches!(default, InputValue::Integer(_)) => Ok(()),
            InputType::String if matches!(default, InputValue::String(_)) => {
                if default.as_raw().contains('\0') {
                    Err(InputError::InvalidString)
                } else {
                    Ok(())
                }
            }
            InputType::Path if matches!(default, InputValue::String(_)) => {
                validate_concrete_path("input.default", &default.as_raw())
                    .map_err(|_| InputError::InvalidPath)
            }
            InputType::Enum if matches!(default, InputValue::String(_)) => {
                let value = default.as_raw();
                self.parse_value(&value)
                    .map(|_| ())
                    .map_err(|_| InputError::InvalidEnumValue { value })
            }
            _ => Err(InputError::WrongDefaultType),
        }
    }

    /// Parse a value using only the type declared by this input.
    pub fn parse_value(&self, raw: &str) -> Result<ParsedInput, ModelError> {
        self.parse_value_named(&self.target, raw)
    }

    /// Parse a value while retaining the source environment variable name in
    /// the structured diagnostic.
    pub fn parse_value_named(&self, name: &str, raw: &str) -> Result<ParsedInput, ModelError> {
        let result = match self.input_type {
            InputType::Bool => match raw {
                "1" | "true" | "TRUE" | "True" => Ok(ParsedInput::Bool(true)),
                "0" | "false" | "FALSE" | "False" => Ok(ParsedInput::Bool(false)),
                _ => Err(InputError::InvalidBoolean {
                    value: raw.to_owned(),
                }),
            },
            InputType::Enum => {
                let value = self.aliases.get(raw).map(String::as_str).unwrap_or(raw);
                if self.values.iter().any(|candidate| candidate == value) {
                    Ok(ParsedInput::Enum(value.to_owned()))
                } else {
                    Err(InputError::InvalidEnumValue {
                        value: raw.to_owned(),
                    })
                }
            }
            InputType::Integer => raw.parse::<i64>().map(ParsedInput::Integer).map_err(|_| {
                InputError::InvalidInteger {
                    value: raw.to_owned(),
                }
            }),
            InputType::Path => match validate_concrete_path("input", raw) {
                Ok(()) => Ok(ParsedInput::Path(raw.to_owned())),
                Err(_) => Err(InputError::InvalidPath),
            },
            InputType::String => {
                if raw.contains('\0') {
                    Err(InputError::InvalidString)
                } else {
                    Ok(ParsedInput::String(raw.to_owned()))
                }
            }
        };
        result.map_err(|reason| ModelError::InvalidInput {
            name: name.to_owned(),
            reason,
        })
    }

    pub fn parse_runtime_value(&self, raw: &str) -> Result<ParsedInput, ModelError> {
        self.parse_runtime_value_named(&self.target, raw)
    }

    pub fn parse_runtime_value_named(
        &self,
        name: &str,
        raw: &str,
    ) -> Result<ParsedInput, ModelError> {
        if !self.runtime {
            return Err(ModelError::InvalidInput {
                name: name.to_owned(),
                reason: InputError::NotRuntime,
            });
        }
        self.parse_value_named(name, raw)
    }

    pub fn parse_declared_value(&self, value: &ValueTree) -> Result<ParsedInput, ModelError> {
        match value {
            ValueTree::Bool(value) => self.parse_value(&value.to_string()),
            ValueTree::Integer(value) => self.parse_value(&value.to_string()),
            ValueTree::String(value) => self.parse_value(value),
            _ => Err(ModelError::InvalidInput {
                name: self.target.clone(),
                reason: InputError::WrongDefaultType,
            }),
        }
    }
}
