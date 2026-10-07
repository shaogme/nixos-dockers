use crate::context::ContextError;
use crate::fingerprint::FingerprintError;
use crate::ConfigTreeError;
use dev_env_model::ModelError;
use dev_env_provider::ProviderRuntimeError;
use std::error::Error;
use std::fmt;

/// Structured failures raised by the materializer.
///
/// A nested source is retained for every operation that can fail at runtime.
/// In particular, provider errors are never rendered and stored as strings;
/// callers can still inspect their status, stderr, policy, and typed cause.
#[derive(Debug)]
pub enum CoreError {
    Model(ModelError),
    Context(ContextError),
    ConfigTree(ConfigTreeError),
    Fingerprint(FingerprintError),
    Provider {
        provider: String,
        source: Box<ProviderRuntimeError>,
    },
    PathJoin(std::env::JoinPathsError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreErrorKind {
    Model,
    Context,
    ConfigTree,
    Fingerprint,
    Provider,
    PathJoin,
}

impl CoreError {
    pub fn kind(&self) -> CoreErrorKind {
        match self {
            Self::Model(_) => CoreErrorKind::Model,
            Self::Context(_) => CoreErrorKind::Context,
            Self::ConfigTree(_) => CoreErrorKind::ConfigTree,
            Self::Fingerprint(_) => CoreErrorKind::Fingerprint,
            Self::Provider { .. } => CoreErrorKind::Provider,
            Self::PathJoin(_) => CoreErrorKind::PathJoin,
        }
    }

    pub const fn code(&self) -> &'static str {
        match self {
            Self::Model(_) => "DEVENV-E-MODEL",
            Self::Context(_) => "DEVENV-E-CONTEXT",
            Self::ConfigTree(_) => "DEVENV-E-CONFIG-TREE",
            Self::Fingerprint(_) => "DEVENV-E-FINGERPRINT",
            Self::Provider { .. } => "DEVENV-E-PROVIDER",
            Self::PathJoin(_) => "DEVENV-E-PATH",
        }
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(source) => write!(formatter, "{source}"),
            Self::Context(source) => write!(formatter, "{source}"),
            Self::ConfigTree(source) => write!(formatter, "configuration context failed: {source}"),
            Self::Fingerprint(source) => {
                write!(formatter, "configuration fingerprint failed: {source}")
            }
            Self::Provider { provider, source } => {
                write!(formatter, "provider {provider:?} failed: {source}")
            }
            Self::PathJoin(source) => write!(formatter, "could not construct PATH: {source}"),
        }
    }
}

impl Error for CoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Model(source) => Some(source),
            Self::Context(source) => Some(source),
            Self::ConfigTree(source) => Some(source),
            Self::Fingerprint(source) => Some(source),
            Self::Provider { source, .. } => Some(source.as_ref()),
            Self::PathJoin(source) => Some(source),
        }
    }
}

impl From<ModelError> for CoreError {
    fn from(source: ModelError) -> Self {
        Self::Model(source)
    }
}

impl From<ContextError> for CoreError {
    fn from(source: ContextError) -> Self {
        Self::Context(source)
    }
}

impl From<ConfigTreeError> for CoreError {
    fn from(source: ConfigTreeError) -> Self {
        Self::ConfigTree(source)
    }
}

impl From<FingerprintError> for CoreError {
    fn from(source: FingerprintError) -> Self {
        Self::Fingerprint(source)
    }
}
