use crate::command::CommandLineError;
use dev_env_model::ModelError;
use serde_json::Error as JsonError;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

/// Errors raised while turning a validated shell declaration into argv.
#[derive(Debug)]
pub enum ShellBuildError {
    Model { source: ModelError },
    MissingCommandArgument { shell: String },
    CommandLine { source: CommandLineError },
}

impl fmt::Display for ShellBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model { source } => write!(formatter, "shell configuration is invalid: {source}"),
            Self::MissingCommandArgument { shell } => write!(
                formatter,
                "shell {shell:?} has no command_arg configured for command execution"
            ),
            Self::CommandLine { source } => {
                write!(formatter, "shell command line is invalid: {source}")
            }
        }
    }
}

impl Error for ShellBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Model { source } => Some(source),
            Self::CommandLine { source } => Some(source),
            Self::MissingCommandArgument { .. } => None,
        }
    }
}

/// Errors raised while formatting a materialized environment for display.
#[derive(Debug)]
pub enum EnvironmentFormatError {
    Model { source: ModelError },
    Json { source: JsonError },
}

impl fmt::Display for EnvironmentFormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model { source } => {
                write!(formatter, "materialized environment is invalid: {source}")
            }
            Self::Json { source } => {
                write!(formatter, "could not encode environment as JSON: {source}")
            }
        }
    }
}

impl Error for EnvironmentFormatError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Model { source } => Some(source),
            Self::Json { source } => Some(source),
        }
    }
}

/// Errors raised while selecting one of the configured shell invocation
/// modes.
#[derive(Debug)]
pub enum ShellInvocationError {
    Build { source: ShellBuildError },
}

impl fmt::Display for ShellInvocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build { source } => {
                write!(formatter, "could not build shell invocation: {source}")
            }
        }
    }
}

impl Error for ShellInvocationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Build { source } => Some(source),
        }
    }
}

/// Why a shim's real executable cannot be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShimPathError {
    Empty,
    Relative,
    Nul,
}

impl fmt::Display for ShimPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "shim real executable may not be empty",
            Self::Relative => "shim real executable must be an absolute path",
            Self::Nul => "shim real executable may not contain NUL",
        })
    }
}

impl Error for ShimPathError {}

/// Errors raised while validating or building a shell shim command line.
#[derive(Debug)]
pub enum ShimError {
    InvalidRealPath {
        path: PathBuf,
        reason: ShimPathError,
    },
    Recursive {
        real: PathBuf,
        configured: PathBuf,
    },
    CommandLine {
        source: CommandLineError,
    },
}

impl fmt::Display for ShimError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRealPath { path, reason } => {
                write!(
                    formatter,
                    "shim real executable {}: {reason}",
                    path.display()
                )
            }
            Self::Recursive { real, configured } => write!(
                formatter,
                "shim real executable {} points at the configured shim command {}",
                real.display(),
                configured.display()
            ),
            Self::CommandLine { source } => {
                write!(formatter, "shim command line is invalid: {source}")
            }
        }
    }
}

impl Error for ShimError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidRealPath { reason, .. } => Some(reason),
            Self::CommandLine { source } => Some(source),
            Self::Recursive { .. } => None,
        }
    }
}
