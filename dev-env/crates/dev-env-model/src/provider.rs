use serde::{Deserialize, Serialize};
use std::fmt;

use crate::condition::Condition;
use crate::validation::{validate_command_arg, validate_glob, validate_id, validate_nonempty_text};
use crate::{ModelError, Sensitivity};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    #[default]
    Error,
    Warn,
    Ignore,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingProviderPolicy {
    #[default]
    Error,
    Warn,
    Ignore,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellEnvFormat {
    Shell,
    Dotenv,
    Json,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ProviderError {
    InvalidId,
    EmptyExecutable,
    InvalidExecutable,
    EmptyArg,
    InvalidArg,
    InvalidGlob,
    EmptyPrepareArgv,
    InvalidCondition,
    InvalidShellenv,
    InvalidTimeout,
    DuplicateDependency,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId => formatter.write_str("provider id is invalid"),
            Self::EmptyExecutable => formatter.write_str("provider executable may not be empty"),
            Self::InvalidExecutable => formatter.write_str("provider executable is invalid"),
            Self::EmptyArg => formatter.write_str("provider argv values may not be empty"),
            Self::InvalidArg => formatter.write_str("provider argv value is invalid"),
            Self::InvalidGlob => formatter.write_str("provider detect glob is invalid"),
            Self::EmptyPrepareArgv => formatter.write_str("prepare argv may not be empty"),
            Self::InvalidCondition => formatter.write_str("provider condition is invalid"),
            Self::InvalidShellenv => formatter.write_str("shellenv configuration is invalid"),
            Self::InvalidTimeout => formatter.write_str("timeout must be greater than zero"),
            Self::DuplicateDependency => {
                formatter.write_str("provider dependencies must be unique")
            }
        }
    }
}

impl std::error::Error for ProviderError {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareStep {
    pub argv: Vec<String>,
    pub when: Option<String>,
    #[serde(default)]
    pub failure: FailurePolicy,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub sensitivity: Sensitivity,
}

impl PrepareStep {
    pub fn validate(&self, provider: &str, index: usize) -> Result<(), ModelError> {
        if self.argv.is_empty() {
            return Err(ModelError::InvalidProvider {
                provider: provider.to_owned(),
                reason: ProviderError::EmptyPrepareArgv,
            });
        }
        for argument in &self.argv {
            if argument.is_empty() {
                return Err(ModelError::InvalidProvider {
                    provider: provider.to_owned(),
                    reason: ProviderError::EmptyArg,
                });
            }
            validate_command_arg(
                &format!("providers.{provider}.prepare[{index}].argv"),
                argument,
            )
            .map_err(|_| ModelError::InvalidProvider {
                provider: provider.to_owned(),
                reason: ProviderError::InvalidArg,
            })?;
        }
        if let Some(condition) = &self.when {
            Condition::parse(condition).map_err(|_| ModelError::InvalidProvider {
                provider: provider.to_owned(),
                reason: ProviderError::InvalidCondition,
            })?;
        }
        if matches!(self.timeout_ms, Some(0)) {
            return Err(ModelError::InvalidProvider {
                provider: provider.to_owned(),
                reason: ProviderError::InvalidTimeout,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShellEnvConfig {
    pub argv: Vec<String>,
    pub format: ShellEnvFormat,
    #[serde(default)]
    pub failure: FailurePolicy,
    #[serde(default)]
    pub path_mode: crate::PathMode,
    pub timeout_ms: Option<u64>,
}

impl ShellEnvConfig {
    pub fn validate(&self, provider: &str) -> Result<(), ModelError> {
        if self.argv.is_empty() {
            return Err(ModelError::InvalidProvider {
                provider: provider.to_owned(),
                reason: ProviderError::InvalidShellenv,
            });
        }
        for argument in &self.argv {
            if argument.is_empty() {
                return Err(ModelError::InvalidProvider {
                    provider: provider.to_owned(),
                    reason: ProviderError::EmptyArg,
                });
            }
            validate_command_arg(&format!("providers.{provider}.shellenv.argv"), argument)
                .map_err(|_| ModelError::InvalidProvider {
                    provider: provider.to_owned(),
                    reason: ProviderError::InvalidArg,
                })?;
        }
        if matches!(self.timeout_ms, Some(0)) {
            return Err(ModelError::InvalidProvider {
                provider: provider.to_owned(),
                reason: ProviderError::InvalidTimeout,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub executable: String,
    #[serde(default)]
    pub detect_files: Vec<String>,
    #[serde(default)]
    pub missing: MissingProviderPolicy,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub prepare: Vec<PrepareStep>,
    pub shellenv: Option<ShellEnvConfig>,
    #[serde(default)]
    pub sensitivity: Sensitivity,
}

impl ProviderConfig {
    pub fn validate(&self, id: &str) -> Result<(), ModelError> {
        validate_id(&format!("providers.{id}"), id).map_err(|_| ModelError::InvalidProvider {
            provider: id.to_owned(),
            reason: ProviderError::InvalidId,
        })?;
        validate_nonempty_text(
            &format!("providers.{id}.executable"),
            &self.executable,
            false,
        )
        .map_err(|_| ModelError::InvalidProvider {
            provider: id.to_owned(),
            reason: if self.executable.is_empty() {
                ProviderError::EmptyExecutable
            } else {
                ProviderError::InvalidExecutable
            },
        })?;
        if self.executable.chars().any(char::is_whitespace)
            || self.executable.contains("$(")
            || self.executable.contains('`')
            || self.executable.contains(';')
            || self.executable.contains('|')
            || self.executable.contains('&')
            || self.executable.contains('>')
            || self.executable.contains('<')
        {
            return Err(ModelError::InvalidProvider {
                provider: id.to_owned(),
                reason: ProviderError::InvalidExecutable,
            });
        }
        for (index, glob) in self.detect_files.iter().enumerate() {
            validate_glob(&format!("providers.{id}.detect_files[{index}]"), glob).map_err(
                |_| ModelError::InvalidProvider {
                    provider: id.to_owned(),
                    reason: ProviderError::InvalidGlob,
                },
            )?;
        }
        let mut dependencies = std::collections::BTreeSet::new();
        for dependency in &self.depends_on {
            validate_id(&format!("providers.{id}.depends_on"), dependency).map_err(|_| {
                ModelError::InvalidProvider {
                    provider: id.to_owned(),
                    reason: ProviderError::InvalidId,
                }
            })?;
            if !dependencies.insert(dependency) {
                return Err(ModelError::InvalidProvider {
                    provider: id.to_owned(),
                    reason: ProviderError::DuplicateDependency,
                });
            }
        }
        for (index, step) in self.prepare.iter().enumerate() {
            step.validate(id, index)?;
        }
        if let Some(shellenv) = &self.shellenv {
            shellenv.validate(id)?;
        }
        Ok(())
    }
}
