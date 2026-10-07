use crate::adapter::ShellAdapter;
use crate::command::CommandLine;
use crate::error::{ShellBuildError, ShellInvocationError};
use dev_env_model::ShellConfig;
use std::ffi::OsString;

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
}

pub fn build_invocation<A: ShellAdapter + ?Sized>(
    adapter: &A,
    config: &ShellConfig,
    invocation: &ShellInvocation,
) -> Result<CommandLine, ShellInvocationError> {
    match invocation {
        ShellInvocation::Interactive { args } => adapter
            .build_interactive(config, args)
            .map_err(|source| ShellInvocationError::Build { source }),
        ShellInvocation::Login { args } => adapter
            .build_login(config, args)
            .map_err(|source| ShellInvocationError::Build { source }),
        ShellInvocation::Command { script } => adapter
            .build_command(config, script)
            .map_err(|source| ShellInvocationError::Build { source }),
    }
}

impl From<ShellBuildError> for ShellInvocationError {
    fn from(source: ShellBuildError) -> Self {
        Self::Build { source }
    }
}
