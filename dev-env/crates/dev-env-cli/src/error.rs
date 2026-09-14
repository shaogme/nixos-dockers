use crate::args::ParseError;
use dev_env_core::{CoreError, CoreErrorKind};
use dev_env_loader::LoaderError;
use dev_env_model::ModelError;
use dev_env_shell::{CommandLineError, EnvironmentFormatError, ShellInvocationError, ShimError};
use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IoOperation {
    ReadCurrentDirectory,
    ReadProfileDirectory,
    ReadProfile,
    ReadDefaultProfile,
    ReadOverlay,
    ReadTrustFile,
    WriteTrustFile,
    HashFile,
}

impl fmt::Display for IoOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ReadCurrentDirectory => "read current directory",
            Self::ReadProfileDirectory => "read profile directory",
            Self::ReadProfile => "read profile",
            Self::ReadDefaultProfile => "read default profile",
            Self::ReadOverlay => "read configuration overlay",
            Self::ReadTrustFile => "read trust file",
            Self::WriteTrustFile => "write trust file",
            Self::HashFile => "hash file",
        })
    }
}

#[derive(Debug)]
pub enum OutputError {
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for OutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(formatter, "could not write output: {source}"),
            Self::Json(source) => write!(formatter, "could not serialize output: {source}"),
        }
    }
}

impl Error for OutputError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::Json(source) => Some(source),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigurationError {
    InvalidDefaultProfile {
        path: PathBuf,
        reason: DefaultProfileReason,
    },
    CwdOutsideWorkspace {
        cwd: PathBuf,
        workspace: PathBuf,
    },
    AmbiguousWorkspaceConfig {
        directory: PathBuf,
        conventional: PathBuf,
        directory_config: PathBuf,
    },
    MissingConfigPath {
        path: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefaultProfileReason {
    Empty,
    Whitespace,
    MultipleLines,
}

impl fmt::Display for DefaultProfileReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "file is empty",
            Self::Whitespace => "profile id contains whitespace",
            Self::MultipleLines => "file must contain exactly one profile id",
        })
    }
}

impl Error for DefaultProfileReason {}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDefaultProfile { path, reason } => {
                write!(formatter, "default profile {}: {reason}", path.display())
            }
            Self::CwdOutsideWorkspace { cwd, workspace } => write!(
                formatter,
                "current directory {} is outside workspace {}",
                cwd.display(),
                workspace.display()
            ),
            Self::AmbiguousWorkspaceConfig {
                directory,
                conventional,
                directory_config,
            } => write!(
                formatter,
                "workspace directory {} contains both {} and {}; use --config or DEVENV_CONFIG",
                directory.display(),
                conventional.display(),
                directory_config.display()
            ),
            Self::MissingConfigPath { path } => {
                write!(formatter, "configuration path {path:?} does not exist")
            }
        }
    }
}

impl Error for ConfigurationError {}

#[derive(Debug)]
pub enum TrustError {
    InvalidHash { value: PathBuf },
    HashFile { path: PathBuf, source: io::Error },
    ReadStore { path: PathBuf, source: io::Error },
    WriteStore { path: PathBuf, source: io::Error },
}

impl fmt::Display for TrustError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHash { value } => {
                write!(
                    formatter,
                    "trust target {} is neither a file nor a SHA-256 hash",
                    value.display()
                )
            }
            Self::HashFile { path, source } => {
                write!(formatter, "could not hash {}: {source}", path.display())
            }
            Self::ReadStore { path, source } => {
                write!(
                    formatter,
                    "could not read trust store {}: {source}",
                    path.display()
                )
            }
            Self::WriteStore { path, source } => {
                write!(
                    formatter,
                    "could not write trust store {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl Error for TrustError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::HashFile { source, .. }
            | Self::ReadStore { source, .. }
            | Self::WriteStore { source, .. } => Some(source),
            Self::InvalidHash { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum CliError {
    Arguments(ParseError),
    Loader(LoaderError),
    Configuration(ConfigurationError),
    Core(CoreError),
    Shell(ShellInvocationError),
    Shim(ShimError),
    CommandLine(CommandLineError),
    Format(EnvironmentFormatError),
    Io {
        operation: IoOperation,
        path: Option<PathBuf>,
        source: io::Error,
    },
    Output(OutputError),
    Trust(TrustError),
    Launch {
        program: PathBuf,
        args: Vec<std::ffi::OsString>,
        source: io::Error,
    },
    DoctorFailed {
        failed_checks: usize,
    },
}

impl CliError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Arguments(_) => 64,
            Self::Loader(error) => match error {
                LoaderError::TrustViolation { .. } => 66,
                LoaderError::Model {
                    source:
                        ModelError::WorkspaceOverrideNotAllowed { .. }
                        | ModelError::CliOverrideNotAllowed { .. }
                        | ModelError::PreferChildPolicyNotTrusted,
                    ..
                } => 66,
                _ => 65,
            },
            Self::Configuration(_) => 65,
            Self::Core(error) => match error.kind() {
                CoreErrorKind::Provider => 70,
                CoreErrorKind::Model
                | CoreErrorKind::Context
                | CoreErrorKind::ConfigTree
                | CoreErrorKind::Fingerprint
                | CoreErrorKind::PathJoin => 65,
            },
            Self::Shell(_) | Self::Shim(_) | Self::CommandLine(_) | Self::Format(_) => 65,
            Self::Io { .. } | Self::Output(_) => 74,
            Self::Trust(_) => 66,
            Self::Launch { .. } => 127,
            Self::DoctorFailed { .. } => 65,
        }
    }

    pub(crate) fn io(operation: IoOperation, path: Option<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path,
            source,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arguments(error) => error.fmt(formatter),
            Self::Loader(error) => write!(formatter, "DEVENV-E-CONFIG: {error}"),
            Self::Configuration(error) => write!(formatter, "DEVENV-E-CONFIG: {error}"),
            Self::Core(error) => write!(formatter, "{}: {error}", error.code()),
            Self::Shell(error) => write!(formatter, "DEVENV-E-SHELL: {error}"),
            Self::Shim(error) => write!(formatter, "DEVENV-E-SHIM: {error}"),
            Self::CommandLine(error) => write!(formatter, "DEVENV-E-COMMAND: {error}"),
            Self::Format(error) => write!(formatter, "DEVENV-E-FORMAT: {error}"),
            Self::Io {
                operation,
                path: Some(path),
                source,
            } => write!(
                formatter,
                "DEVENV-E-IO: {operation} {} failed: {source}",
                path.display()
            ),
            Self::Io {
                operation,
                path: None,
                source,
            } => write!(formatter, "DEVENV-E-IO: {operation} failed: {source}"),
            Self::Output(error) => write!(formatter, "DEVENV-E-OUTPUT: {error}"),
            Self::Trust(error) => write!(formatter, "DEVENV-E-TRUST: {error}"),
            Self::Launch {
                program, source, ..
            } => write!(
                formatter,
                "DEVENV-E-LAUNCH: could not execute {}: {source}",
                program.display()
            ),
            Self::DoctorFailed { failed_checks } => write!(
                formatter,
                "DEVENV-E-DOCTOR: {failed_checks} runtime check(s) failed"
            ),
        }
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Arguments(error) => Some(error),
            Self::Loader(error) => Some(error),
            Self::Configuration(error) => Some(error),
            Self::Core(error) => Some(error),
            Self::Shell(error) => Some(error),
            Self::Shim(error) => Some(error),
            Self::CommandLine(error) => Some(error),
            Self::Format(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::Output(error) => Some(error),
            Self::Trust(error) => Some(error),
            Self::Launch { source, .. } => Some(source),
            Self::DoctorFailed { .. } => None,
        }
    }
}

impl From<ParseError> for CliError {
    fn from(source: ParseError) -> Self {
        Self::Arguments(source)
    }
}

impl From<LoaderError> for CliError {
    fn from(source: LoaderError) -> Self {
        Self::Loader(source)
    }
}

impl From<ConfigurationError> for CliError {
    fn from(source: ConfigurationError) -> Self {
        Self::Configuration(source)
    }
}

impl From<CoreError> for CliError {
    fn from(source: CoreError) -> Self {
        Self::Core(source)
    }
}

impl From<ShellInvocationError> for CliError {
    fn from(source: ShellInvocationError) -> Self {
        Self::Shell(source)
    }
}

impl From<EnvironmentFormatError> for CliError {
    fn from(source: EnvironmentFormatError) -> Self {
        Self::Format(source)
    }
}

impl From<ShimError> for CliError {
    fn from(source: ShimError) -> Self {
        Self::Shim(source)
    }
}

impl From<CommandLineError> for CliError {
    fn from(source: CommandLineError) -> Self {
        Self::CommandLine(source)
    }
}

impl From<TrustError> for CliError {
    fn from(source: TrustError) -> Self {
        Self::Trust(source)
    }
}

impl From<OutputError> for CliError {
    fn from(source: OutputError) -> Self {
        Self::Output(source)
    }
}
