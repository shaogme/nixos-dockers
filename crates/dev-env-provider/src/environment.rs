use dev_env_model::{
    EnvValue, MaterializedEnv, Sensitivity, ShellEnvEntry, ShellEnvFormat, ShellEnvParseError,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DotenvParseError {
    InvalidLine {
        line: usize,
    },
    Shell {
        line: usize,
        source: ShellEnvParseError,
    },
}

impl fmt::Display for DotenvParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLine { line } => write!(formatter, "invalid dotenv line {line}"),
            Self::Shell { line, source } => write!(formatter, "dotenv line {line}: {source}"),
        }
    }
}

impl std::error::Error for DotenvParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Shell { source, .. } => Some(source),
            Self::InvalidLine { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum JsonEnvironmentError {
    Parse { source: serde_json::Error },
    RootNotObject,
    NonString { name: String },
    InvalidName { name: String },
}

impl fmt::Display for JsonEnvironmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse { source } => write!(formatter, "invalid JSON environment: {source}"),
            Self::RootNotObject => formatter.write_str("JSON environment must be an object"),
            Self::NonString { name } => {
                write!(
                    formatter,
                    "JSON environment value {name:?} must be a string or null"
                )
            }
            Self::InvalidName { name } => {
                write!(formatter, "invalid JSON environment name {name:?}")
            }
        }
    }
}

impl std::error::Error for JsonEnvironmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse { source } => Some(source),
            Self::RootNotObject | Self::NonString { .. } | Self::InvalidName { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum EnvironmentParseError {
    Shell { source: ShellEnvParseError },
    Dotenv { source: DotenvParseError },
    Json { source: JsonEnvironmentError },
}

impl fmt::Display for EnvironmentParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shell { source } => write!(formatter, "shellenv output: {source}"),
            Self::Dotenv { source } => write!(formatter, "dotenv output: {source}"),
            Self::Json { source } => write!(formatter, "JSON environment output: {source}"),
        }
    }
}

impl std::error::Error for EnvironmentParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Shell { source } => Some(source),
            Self::Dotenv { source } => Some(source),
            Self::Json { source } => Some(source),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnvironmentDelta {
    pub set: BTreeMap<String, String>,
    pub unset: BTreeSet<String>,
}

impl EnvironmentDelta {
    pub fn from_entries(entries: impl IntoIterator<Item = ShellEnvEntry>) -> Self {
        let mut delta = Self::default();
        delta.apply_entries(entries);
        delta
    }

    pub fn apply_entries(&mut self, entries: impl IntoIterator<Item = ShellEnvEntry>) {
        for entry in entries {
            match entry {
                ShellEnvEntry::Set { name, value } => {
                    self.unset.remove(&name);
                    self.set.insert(name, value);
                }
                ShellEnvEntry::Unset { name } => {
                    self.set.remove(&name);
                    self.unset.insert(name);
                }
            }
        }
    }

    pub fn apply_to(&self, environment: &mut BTreeMap<String, String>) {
        for name in &self.unset {
            environment.remove(name);
        }
        environment.extend(
            self.set
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
    }

    /// Apply provider metadata at the boundary where the generic delta enters
    /// the model's materialized environment.
    pub fn apply_to_materialized(
        &self,
        environment: &mut MaterializedEnv,
        provider: &str,
        sensitivity: Sensitivity,
    ) {
        for name in &self.unset {
            environment.values.remove(name);
        }
        for (name, value) in &self.set {
            environment.values.insert(
                name.clone(),
                EnvValue {
                    value: value.clone(),
                    origin: None,
                    sensitivity,
                    provider: Some(provider.to_owned()),
                },
            );
        }
    }
}

pub fn parse_output(
    format: ShellEnvFormat,
    output: &str,
) -> Result<Vec<ShellEnvEntry>, EnvironmentParseError> {
    match format {
        ShellEnvFormat::Shell => {
            ShellEnvEntry::parse(output).map_err(|source| EnvironmentParseError::Shell { source })
        }
        ShellEnvFormat::Dotenv => {
            parse_dotenv(output).map_err(|source| EnvironmentParseError::Dotenv { source })
        }
        ShellEnvFormat::Json => {
            parse_json(output).map_err(|source| EnvironmentParseError::Json { source })
        }
    }
}

fn parse_dotenv(output: &str) -> Result<Vec<ShellEnvEntry>, DotenvParseError> {
    output
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                None
            } else {
                Some((index + 1, line))
            }
        })
        .map(|(line, contents)| {
            ShellEnvEntry::parse_line(contents)
                .map_err(|source| DotenvParseError::Shell { line, source })?
                .ok_or(DotenvParseError::InvalidLine { line })
        })
        .collect()
}

fn parse_json(output: &str) -> Result<Vec<ShellEnvEntry>, JsonEnvironmentError> {
    let value: Value =
        serde_json::from_str(output).map_err(|source| JsonEnvironmentError::Parse { source })?;
    let Value::Object(values) = value else {
        return Err(JsonEnvironmentError::RootNotObject);
    };
    values
        .into_iter()
        .map(|(name, value)| {
            if !is_environment_name(&name) {
                return Err(JsonEnvironmentError::InvalidName { name });
            }
            match value {
                Value::String(value) => Ok(ShellEnvEntry::Set { name, value }),
                Value::Null => Ok(ShellEnvEntry::Unset { name }),
                _ => Err(JsonEnvironmentError::NonString { name }),
            }
        })
        .collect()
}

fn is_environment_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(character) if character.is_ascii_uppercase() || character == '_')
        && chars.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}
