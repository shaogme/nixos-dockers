use crate::command::CommandLine;
use crate::environment::{format_environment, EnvironmentFormat, RenderOptions};
use crate::error::{EnvironmentFormatError, ShellBuildError};
use dev_env_model::{MaterializedEnv, ShellConfig};
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// Builds shell argv from profile data.  Implementations may choose a
/// display format, but the command construction defaults are intentionally
/// data-driven by [`ShellConfig`].
pub trait ShellAdapter {
    fn id(&self) -> &str;

    fn environment_format(&self) -> EnvironmentFormat {
        EnvironmentFormat::Shell
    }

    fn command<'a>(&self, config: &'a ShellConfig) -> &'a Path {
        Path::new(&config.command)
    }

    fn build_interactive(
        &self,
        config: &ShellConfig,
        args: &[OsString],
    ) -> Result<CommandLine, ShellBuildError> {
        config
            .validate(self.id())
            .map_err(|source| ShellBuildError::Model { source })?;
        build_with_configured_args(self.command(config), &config.interactive_args, args)
    }

    fn build_login(
        &self,
        config: &ShellConfig,
        args: &[OsString],
    ) -> Result<CommandLine, ShellBuildError> {
        config
            .validate(self.id())
            .map_err(|source| ShellBuildError::Model { source })?;
        // An argument-bearing login invocation (for example SSH's `-c`) must
        // retain its own mode.  With no arguments, select the profile's
        // default login + interactive flags together.
        let configured = if args.is_empty() {
            config
                .login_args
                .iter()
                .chain(config.interactive_args.iter())
                .cloned()
                .collect::<Vec<_>>()
        } else {
            config.login_args.clone()
        };
        build_with_configured_args(self.command(config), &configured, args)
    }

    fn build_command(
        &self,
        config: &ShellConfig,
        script: &OsStr,
    ) -> Result<CommandLine, ShellBuildError> {
        config
            .validate(self.id())
            .map_err(|source| ShellBuildError::Model { source })?;
        let command_arg = config.command_arg.as_deref().ok_or_else(|| {
            ShellBuildError::MissingCommandArgument {
                shell: self.id().to_owned(),
            }
        })?;
        let configured = [command_arg.to_owned()];
        build_with_configured_args(self.command(config), &configured, &[script.to_owned()])
    }

    fn format_env(&self, environment: &MaterializedEnv) -> Result<String, EnvironmentFormatError> {
        format_environment(
            self.environment_format(),
            environment,
            RenderOptions::default(),
        )
    }

    fn format_env_with_options(
        &self,
        environment: &MaterializedEnv,
        options: RenderOptions,
    ) -> Result<String, EnvironmentFormatError> {
        format_environment(self.environment_format(), environment, options)
    }
}

/// Generic adapter used by both POSIX and argv-oriented shells.  Shell
/// differences belong in the profile's argv fields; this type only supplies
/// the adapter identity and output format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredShellAdapter {
    id: String,
    format: EnvironmentFormat,
}

impl ConfiguredShellAdapter {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            format: EnvironmentFormat::Shell,
        }
    }

    pub fn with_environment_format(mut self, format: EnvironmentFormat) -> Self {
        self.format = format;
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

impl ShellAdapter for ConfiguredShellAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn environment_format(&self) -> EnvironmentFormat {
        self.format
    }
}

fn build_with_configured_args(
    program: &Path,
    configured: &[String],
    extra: &[OsString],
) -> Result<CommandLine, ShellBuildError> {
    let args = configured
        .iter()
        .map(OsString::from)
        .chain(extra.iter().cloned())
        .collect::<Vec<_>>();
    CommandLine::try_new(program.to_path_buf(), args)
        .map_err(|source| ShellBuildError::CommandLine { source })
}
