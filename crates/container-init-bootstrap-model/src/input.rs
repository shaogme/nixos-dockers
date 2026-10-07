use serde::{Deserialize, Serialize};

use crate::error::ModelError;
use crate::validation::{is_bootstrap_target, is_env_name, parse_id, validate_path_template};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BootstrapInput {
    pub target: String,
    #[serde(rename = "type")]
    pub input_type: InputType,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub runtime: bool,
    pub format: Option<String>,
    /// The namespace in which a UID/GID runtime value is expressed.
    ///
    /// This is deliberately optional at the Rust representation boundary so
    /// that the model can produce a useful validation error for legacy
    /// profiles which omitted it. It is required for every UID/GID input.
    #[serde(default)]
    pub namespace: Option<InputNamespace>,
    pub default: Option<InputValue>,
    #[serde(default)]
    pub allow_outside_workspace: bool,
}

impl BootstrapInput {
    pub(crate) fn validate(&self, name: &str) -> Result<(), ModelError> {
        if !is_bootstrap_target(&self.target) {
            return Err(ModelError::Invalid {
                location: format!("bootstrap.inputs.{name}.target"),
                message: format!("unsupported bootstrap target {:?}", self.target),
            });
        }
        let expected_type = match self.target.as_str() {
            "identity.uid" => InputType::UidPair,
            "identity.gid" => InputType::Gid,
            "identity.run_as_root" => InputType::Bool,
            "identity.home" => InputType::Path,
            _ => unreachable!("target was validated above"),
        };
        if self.input_type != expected_type {
            return Err(ModelError::Invalid {
                location: format!("bootstrap.inputs.{name}.type"),
                message: format!(
                    "input target {:?} requires type {:?}",
                    self.target, expected_type
                ),
            });
        }
        if matches!(self.input_type, InputType::UidPair | InputType::Gid)
            && self.namespace.is_none()
        {
            return Err(ModelError::Invalid {
                location: format!("bootstrap.inputs.{name}.namespace"),
                message:
                    "UID/GID inputs must explicitly declare namespace = \"host\" or \"container\""
                        .to_owned(),
            });
        }
        if !matches!(self.input_type, InputType::UidPair | InputType::Gid)
            && self.namespace.is_some()
        {
            return Err(ModelError::Invalid {
                location: format!("bootstrap.inputs.{name}.namespace"),
                message: "namespace is only valid for uid_pair and gid inputs".to_owned(),
            });
        }
        for alias in &self.aliases {
            if !is_env_name(alias) {
                return Err(ModelError::Invalid {
                    location: format!("bootstrap.inputs.{name}.aliases"),
                    message: format!("{alias:?} is not a valid environment name"),
                });
            }
        }
        if let Some(default) = &self.default {
            self.parse_value(&default.to_string_value())?;
        }
        Ok(())
    }

    /// Parse a declared runtime input using only the input's declared type.
    pub fn parse_value(&self, raw: &str) -> Result<ParsedInput, ModelError> {
        match self.input_type {
            InputType::UidPair => {
                let mut parts = raw.split(':');
                let uid = parse_id("uid", parts.next().unwrap_or_default())?;
                let gid = match parts.next() {
                    Some(value) => Some(parse_id("gid", value)?),
                    None => None,
                };
                if parts.next().is_some() {
                    return Err(ModelError::Invalid {
                        location: "bootstrap input".to_owned(),
                        message: "uid_pair must have the form uid[:gid]".to_owned(),
                    });
                }
                Ok(ParsedInput::UidPair { uid, gid })
            }
            InputType::Gid => Ok(ParsedInput::Gid(parse_id("gid", raw)?)),
            InputType::Bool => match raw {
                "1" | "true" | "TRUE" | "True" => Ok(ParsedInput::Bool(true)),
                "0" | "false" | "FALSE" | "False" => Ok(ParsedInput::Bool(false)),
                _ => Err(ModelError::Invalid {
                    location: "bootstrap input".to_owned(),
                    message: format!("{raw:?} is not a boolean"),
                }),
            },
            InputType::Path => {
                if raw.contains("${") {
                    return Err(ModelError::Invalid {
                        location: "bootstrap input".to_owned(),
                        message: "runtime path inputs must be concrete paths".to_owned(),
                    });
                }
                validate_path_template("bootstrap input", raw)?;
                Ok(ParsedInput::Path(raw.to_owned()))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    UidPair,
    Gid,
    Bool,
    Path,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputNamespace {
    /// The parent user namespace of the current process.
    Host,
    /// The current process user namespace.
    Container,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(untagged)]
pub enum InputValue {
    Bool(bool),
    Integer(u64),
    String(String),
}

impl InputValue {
    fn to_string_value(&self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Integer(value) => value.to_string(),
            Self::String(value) => value.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedInput {
    UidPair { uid: u32, gid: Option<u32> },
    Gid(u32),
    Bool(bool),
    Path(String),
}
