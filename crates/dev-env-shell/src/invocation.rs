use crate::{adapter::ShellAdapter, command::CommandLine, error::ShellBuildError};
use dev_env_model::ShellConfig;
use std::{error::Error, ffi::OsString, fmt};

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

/// One of the shell entry points supported by `dev-env shell` and
/// `dev-env login-shell`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShellInvocation {
    Interactive { args: Vec<OsString> },
    Login { args: Vec<OsString> },
    Command { script: OsString },
}

impl ShellInvocation {
    pub fn interactive(args: impl IntoIterator<Item = OsString>) -> Self {
        Self::Interactive {
            args: args.into_iter().collect(),
        }
    }

    pub fn login(args: impl IntoIterator<Item = OsString>) -> Self {
        Self::Login {
            args: args.into_iter().collect(),
        }
    }

    pub fn command(script: impl Into<OsString>) -> Self {
        Self::Command {
            script: script.into(),
        }
    }

    pub const fn is_command(&self) -> bool {
        matches!(self, Self::Command { .. })
    }

    pub fn build<A: ShellAdapter + ?Sized>(
        &self,
        adapter: &A,
        config: &ShellConfig,
    ) -> Result<CommandLine, ShellInvocationError> {
        match self {
            Self::Interactive { args } => adapter
                .build_interactive(config, args)
                .map_err(|source| ShellInvocationError::Build { source }),
            Self::Login { args } => adapter
                .build_login(config, args)
                .map_err(|source| ShellInvocationError::Build { source }),
            Self::Command { script } => adapter
                .build_command(config, script)
                .map_err(|source| ShellInvocationError::Build { source }),
        }
    }
}

impl From<ShellBuildError> for ShellInvocationError {
    fn from(source: ShellBuildError) -> Self {
        Self::Build { source }
    }
}
