use dev_env_model::MaterializedEnv;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// An executable and its argv, kept separate from process execution.
///
/// Keeping this as data makes it possible to test shell argument handling
/// without spawning a shell.  Every argument remains one argv element; no
/// string is reparsed as shell source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandLine {
    program: PathBuf,
    args: Vec<OsString>,
}

impl CommandLine {
    pub fn new<I, A>(program: impl Into<PathBuf>, args: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    pub fn try_new<I, A>(program: impl Into<PathBuf>, args: I) -> Result<Self, CommandLineError>
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        let command = Self::new(program, args);
        command.validate()?;
        Ok(command)
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    pub fn into_parts(self) -> (PathBuf, Vec<OsString>) {
        (self.program, self.args)
    }

    pub fn validate(&self) -> Result<(), CommandLineError> {
        if self.program.as_os_str().is_empty() {
            return Err(CommandLineError::EmptyProgram);
        }
        if contains_nul(self.program.as_os_str()) {
            return Err(CommandLineError::NulProgram);
        }
        for (index, argument) in self.args.iter().enumerate() {
            if contains_nul(argument) {
                return Err(CommandLineError::NulArgument { index });
            }
        }
        Ok(())
    }

    /// Build a process command and inject the materialized environment.
    ///
    /// The command receives exactly the materialized values.  A higher-level
    /// materializer decides whether ambient values belong in that map; this
    /// layer does not leak the launcher's unrelated process environment.
    pub fn command_with_environment(
        &self,
        environment: &MaterializedEnv,
    ) -> Result<Command, CommandLineError> {
        self.validate()?;
        environment
            .validate()
            .map_err(|source| CommandLineError::Environment { source })?;
        let mut command = Command::new(&self.program);
        command.env_clear().args(&self.args);
        command.envs(
            environment
                .values
                .iter()
                .map(|(name, value)| (name.as_str(), value.value.as_str())),
        );
        Ok(command)
    }
}

#[derive(Debug)]
pub enum CommandLineError {
    EmptyProgram,
    NulProgram,
    NulArgument { index: usize },
    Environment { source: dev_env_model::ModelError },
}

impl std::fmt::Display for CommandLineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyProgram => formatter.write_str("command program may not be empty"),
            Self::NulProgram => formatter.write_str("command program may not contain NUL"),
            Self::NulArgument { index } => {
                write!(formatter, "command argument {index} may not contain NUL")
            }
            Self::Environment { source } => {
                write!(
                    formatter,
                    "environment injection failed validation: {source}"
                )
            }
        }
    }
}

impl std::error::Error for CommandLineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Environment { source } => Some(source),
            Self::EmptyProgram | Self::NulProgram | Self::NulArgument { .. } => None,
        }
    }
}

fn contains_nul(value: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().contains(&0)
    }
    #[cfg(not(unix))]
    {
        value.to_string_lossy().contains('\0')
    }
}
