use crate::command::{CommandError, CommandOutput};
use crate::detect::DetectionError;
use crate::environment::EnvironmentParseError;
use crate::lock::LockError;
use crate::receipt::FingerprintError;
use crate::template::TemplateError;
use dev_env_model::ModelError;
use std::error::Error;
use std::fmt;
use std::string::FromUtf8Error;

/// A lifecycle error remains typed so CLI and JSON adapters can make their
/// own diagnostic decisions without parsing a rendered message.
#[derive(Debug)]
pub enum ProviderRuntimeError {
    Model(ModelError),
    Detection {
        provider: String,
        source: DetectionError,
    },
    MissingExecutable {
        provider: String,
        executable: String,
        policy: dev_env_model::MissingProviderPolicy,
    },
    Template {
        provider: String,
        operation: String,
        source: TemplateError,
    },
    Command {
        provider: String,
        operation: String,
        source: CommandError,
    },
    CommandFailed {
        provider: String,
        operation: String,
        output: CommandOutput,
    },
    OutputUtf8 {
        provider: String,
        operation: String,
        source: FromUtf8Error,
    },
    OutputRejected {
        provider: String,
        operation: String,
        source: EnvironmentParseError,
    },
    Lock {
        provider: String,
        source: LockError,
    },
    Fingerprint {
        provider: String,
        source: FingerprintError,
    },
}

/// A compact classification useful to callers that need a stable exit code.
/// It is derived from [`ProviderRuntimeError`] without replacing the original
/// error value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderRuntimeErrorKind {
    Model,
    Detection,
    MissingExecutable,
    Template,
    Command,
    CommandFailed,
    OutputUtf8,
    OutputRejected,
    Lock,
    Fingerprint,
}

impl ProviderRuntimeError {
    pub fn kind(&self) -> ProviderRuntimeErrorKind {
        match self {
            Self::Model(_) => ProviderRuntimeErrorKind::Model,
            Self::Detection { .. } => ProviderRuntimeErrorKind::Detection,
            Self::MissingExecutable { .. } => ProviderRuntimeErrorKind::MissingExecutable,
            Self::Template { .. } => ProviderRuntimeErrorKind::Template,
            Self::Command { .. } => ProviderRuntimeErrorKind::Command,
            Self::CommandFailed { .. } => ProviderRuntimeErrorKind::CommandFailed,
            Self::OutputUtf8 { .. } => ProviderRuntimeErrorKind::OutputUtf8,
            Self::OutputRejected { .. } => ProviderRuntimeErrorKind::OutputRejected,
            Self::Lock { .. } => ProviderRuntimeErrorKind::Lock,
            Self::Fingerprint { .. } => ProviderRuntimeErrorKind::Fingerprint,
        }
    }
}

impl ProviderRuntimeErrorKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Model => "DEVENV-E-PROVIDER-MODEL",
            Self::Detection => "DEVENV-E-PROVIDER-DETECT",
            Self::MissingExecutable => "DEVENV-E-PROVIDER-MISSING",
            Self::Template => "DEVENV-E-PROVIDER-TEMPLATE",
            Self::Command => "DEVENV-E-PROVIDER-COMMAND",
            Self::CommandFailed => "DEVENV-E-PROVIDER-EXIT",
            Self::OutputUtf8 => "DEVENV-E-PROVIDER-UTF8",
            Self::OutputRejected => "DEVENV-E-PROVIDER-OUTPUT",
            Self::Lock => "DEVENV-E-PROVIDER-LOCK",
            Self::Fingerprint => "DEVENV-E-PROVIDER-FINGERPRINT",
        }
    }
}

impl fmt::Display for ProviderRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(source) => write!(formatter, "provider model validation failed: {source}"),
            Self::Detection { provider, source } => {
                write!(formatter, "provider {provider:?} detection failed: {source}")
            }
            Self::MissingExecutable {
                provider,
                executable,
                ..
            } => write!(formatter, "provider {provider:?} executable {executable:?} is missing"),
            Self::Template {
                provider,
                operation,
                source,
            } => write!(formatter, "provider {provider:?} {operation} template failed: {source}"),
            Self::Command {
                provider,
                operation,
                source,
            } => write!(formatter, "provider {provider:?} {operation} command failed: {source}"),
            Self::CommandFailed {
                provider,
                operation,
                output,
            } => write!(
                formatter,
                "provider {provider:?} {operation} exited unsuccessfully (status {:?}, timed_out: {}, stderr: {:?})",
                output.status,
                output.timed_out,
                String::from_utf8_lossy(&output.stderr)
            ),
            Self::OutputUtf8 {
                provider,
                operation,
                source,
            } => write!(formatter, "provider {provider:?} {operation} output is not UTF-8: {source}"),
            Self::OutputRejected {
                provider,
                operation,
                source,
            } => write!(formatter, "provider {provider:?} {operation} output was rejected: {source}"),
            Self::Lock { provider, source } => {
                write!(formatter, "provider {provider:?} lock failed: {source}")
            }
            Self::Fingerprint { provider, source } => {
                write!(formatter, "provider {provider:?} fingerprint failed: {source}")
            }
        }
    }
}

impl Error for ProviderRuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Model(source) => Some(source),
            Self::Detection { source, .. } => Some(source),
            Self::Template { source, .. } => Some(source),
            Self::Command { source, .. } => Some(source),
            Self::OutputUtf8 { source, .. } => Some(source),
            Self::OutputRejected { source, .. } => Some(source),
            Self::Lock { source, .. } => Some(source),
            Self::Fingerprint { source, .. } => Some(source),
            Self::MissingExecutable { .. } | Self::CommandFailed { .. } => None,
        }
    }
}

impl From<ModelError> for ProviderRuntimeError {
    fn from(source: ModelError) -> Self {
        Self::Model(source)
    }
}
