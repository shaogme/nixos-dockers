use serde::{Deserialize, Serialize};

use crate::{condition::PathTemplate, error::ModelError};

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedInput {
    UidPair { uid: u32, gid: Option<u32> },
    Gid(u32),
    Bool(bool),
    Path(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BootstrapInput {
    target: String,
    #[serde(rename = "type")]
    input_type: InputType,
    #[serde(default)]
    aliases: Vec<String>,
    runtime: bool,
    format: Option<String>,
    /// The namespace in which a UID/GID runtime value is expressed.
    ///
    /// This is deliberately optional at the Rust representation boundary so
    /// that the model can produce a useful validation error for legacy
    /// profiles which omitted it. It is required for every UID/GID input.
    #[serde(default)]
    namespace: Option<InputNamespace>,
    default: Option<InputValue>,
    #[serde(default)]
    allow_outside_workspace: bool,
}

impl BootstrapInput {
    pub fn new(target: impl Into<String>, input_type: InputType) -> Self {
        Self {
            target: target.into(),
            input_type,
            aliases: Vec::new(),
            runtime: false,
            format: None,
            namespace: None,
            default: None,
            allow_outside_workspace: false,
        }
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn set_target(&mut self, target: impl Into<String>) {
        self.target = target.into();
    }

    pub fn input_type(&self) -> InputType {
        self.input_type
    }

    pub fn set_input_type(&mut self, input_type: InputType) {
        self.input_type = input_type;
    }

    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }

    pub fn aliases_mut(&mut self) -> &mut Vec<String> {
        &mut self.aliases
    }

    pub fn set_aliases(&mut self, aliases: Vec<String>) {
        self.aliases = aliases;
    }

    pub fn runtime(&self) -> bool {
        self.runtime
    }

    pub fn set_runtime(&mut self, runtime: bool) {
        self.runtime = runtime;
    }

    pub fn format(&self) -> &Option<String> {
        &self.format
    }

    pub fn set_format(&mut self, format: Option<String>) {
        self.format = format;
    }

    pub fn namespace(&self) -> Option<InputNamespace> {
        self.namespace
    }

    pub fn set_namespace(&mut self, namespace: Option<InputNamespace>) {
        self.namespace = namespace;
    }

    pub fn default(&self) -> &Option<InputValue> {
        &self.default
    }

    pub fn set_default(&mut self, default: Option<InputValue>) {
        self.default = default;
    }

    pub fn allow_outside_workspace(&self) -> bool {
        self.allow_outside_workspace
    }

    pub fn set_allow_outside_workspace(&mut self, allow: bool) {
        self.allow_outside_workspace = allow;
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

    pub(crate) fn validate(&self, name: &str) -> Result<(), ModelError> {
        if !Self::is_bootstrap_target(&self.target) {
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
            _ => {
                return Err(ModelError::Invalid {
                    location: format!("bootstrap.inputs.{name}.target"),
                    message: format!("unsupported bootstrap target {:?}", self.target),
                });
            }
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
            if !Self::is_env_name(alias) {
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
                let uid = Self::parse_id("uid", parts.next().unwrap_or_default())?;
                let gid = match parts.next() {
                    Some(value) => Some(Self::parse_id("gid", value)?),
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
            InputType::Gid => Ok(ParsedInput::Gid(Self::parse_id("gid", raw)?)),
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
                PathTemplate::validate_template("bootstrap input", raw)?;
                Ok(ParsedInput::Path(raw.to_owned()))
            }
        }
    }
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
