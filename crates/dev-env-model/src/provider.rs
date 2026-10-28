use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, error::Error as StdError, fmt};

use crate::{
    condition::Condition, validation::StringValidation, ModelError, PathMode, Sensitivity,
};

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

impl StdError for ProviderError {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareStep {
    argv: Vec<String>,
    when: Option<String>,
    #[serde(default)]
    failure: FailurePolicy,
    timeout_ms: Option<u64>,
    #[serde(default)]
    sensitivity: Sensitivity,
}

impl PrepareStep {
    pub fn new(
        argv: Vec<String>,
        when: Option<String>,
        failure: FailurePolicy,
        timeout_ms: Option<u64>,
        sensitivity: Sensitivity,
    ) -> Self {
        Self {
            argv,
            when,
            failure,
            timeout_ms,
            sensitivity,
        }
    }

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
            argument
                .validate_command_arg(&format!("providers.{provider}.prepare[{index}].argv"))
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
    argv: Vec<String>,
    format: ShellEnvFormat,
    #[serde(default)]
    failure: FailurePolicy,
    #[serde(default)]
    path_mode: PathMode,
    timeout_ms: Option<u64>,
}

impl ShellEnvConfig {
    pub fn new(
        argv: Vec<String>,
        format: ShellEnvFormat,
        failure: FailurePolicy,
        path_mode: PathMode,
        timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            argv,
            format,
            failure,
            path_mode,
            timeout_ms,
        }
    }

    pub fn with_format(mut self, format: ShellEnvFormat) -> Self {
        self.format = format;
        self
    }

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
            argument
                .validate_command_arg(&format!("providers.{provider}.shellenv.argv"))
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
    executable: String,
    #[serde(default)]
    detect_files: Vec<String>,
    #[serde(default)]
    missing: MissingProviderPolicy,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    prepare: Vec<PrepareStep>,
    shellenv: Option<ShellEnvConfig>,
    #[serde(default)]
    sensitivity: Sensitivity,
}

impl ProviderConfig {
    pub fn new(
        executable: impl Into<String>,
        detect_files: Vec<String>,
        missing: MissingProviderPolicy,
        depends_on: Vec<String>,
        prepare: Vec<PrepareStep>,
    ) -> Self {
        Self {
            executable: executable.into(),
            detect_files,
            missing,
            depends_on,
            prepare,
            shellenv: None,
            sensitivity: Sensitivity::Public,
        }
    }

    pub fn with_shellenv(mut self, shellenv: ShellEnvConfig) -> Self {
        self.shellenv = Some(shellenv);
        self
    }

    pub fn with_sensitivity(mut self, sensitivity: Sensitivity) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    pub fn with_depends_on(mut self, depends_on: Vec<String>) -> Self {
        self.depends_on = depends_on;
        self
    }

    pub fn with_detect_files(mut self, detect_files: Vec<String>) -> Self {
        self.detect_files = detect_files;
        self
    }

    pub fn with_missing(mut self, missing: MissingProviderPolicy) -> Self {
        self.missing = missing;
        self
    }

    pub fn validate(&self, id: &str) -> Result<(), ModelError> {
        id.validate_id(&format!("providers.{id}"))
            .map_err(|_| ModelError::InvalidProvider {
                provider: id.to_owned(),
                reason: ProviderError::InvalidId,
            })?;
        self.executable
            .validate_nonempty_text(&format!("providers.{id}.executable"), false)
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
            glob.validate_glob(&format!("providers.{id}.detect_files[{index}]"))
                .map_err(|_| ModelError::InvalidProvider {
                    provider: id.to_owned(),
                    reason: ProviderError::InvalidGlob,
                })?;
        }
        let mut dependencies = BTreeSet::new();
        for dependency in &self.depends_on {
            dependency
                .validate_id(&format!("providers.{id}.depends_on"))
                .map_err(|_| ModelError::InvalidProvider {
                    provider: id.to_owned(),
                    reason: ProviderError::InvalidId,
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

impl PrepareStep {
    pub fn argv(&self) -> &Vec<String> {
        &self.argv
    }

    pub fn when(&self) -> Option<&str> {
        self.when.as_deref()
    }

    pub fn failure(&self) -> FailurePolicy {
        self.failure
    }

    pub fn timeout_ms(&self) -> Option<u64> {
        self.timeout_ms
    }

    pub fn sensitivity(&self) -> Sensitivity {
        self.sensitivity
    }
}

impl ShellEnvConfig {
    pub fn argv(&self) -> &Vec<String> {
        &self.argv
    }

    pub fn format(&self) -> ShellEnvFormat {
        self.format
    }

    pub fn failure(&self) -> FailurePolicy {
        self.failure
    }

    pub fn path_mode(&self) -> PathMode {
        self.path_mode
    }

    pub fn timeout_ms(&self) -> Option<u64> {
        self.timeout_ms
    }
}

impl ProviderConfig {
    pub fn executable(&self) -> &str {
        &self.executable
    }

    pub fn detect_files(&self) -> &Vec<String> {
        &self.detect_files
    }

    pub fn missing(&self) -> MissingProviderPolicy {
        self.missing
    }

    pub fn depends_on(&self) -> &Vec<String> {
        &self.depends_on
    }

    pub fn prepare(&self) -> &Vec<PrepareStep> {
        &self.prepare
    }

    pub fn shellenv(&self) -> &Option<ShellEnvConfig> {
        &self.shellenv
    }

    pub fn sensitivity(&self) -> Sensitivity {
        self.sensitivity
    }
}
