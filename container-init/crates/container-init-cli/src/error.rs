use bootstrap_loader::LoaderError;
use bootstrap_model::ModelError;
use container_init_core::CoreError;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum CliError {
    Arguments(String),
    Loader(LoaderError),
    Model(ModelError),
    Configuration(String),
    Core(CoreError),
    Io {
        operation: String,
        path: Option<PathBuf>,
        source: io::Error,
    },
    Output(serde_json::Error),
    DoctorFailed {
        check: String,
        source: Option<Box<CoreError>>,
    },
}

impl CliError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Arguments(_) => 64,
            Self::Loader(error) => match error {
                LoaderError::TrustViolation { .. } => 66,
                LoaderError::Model(ModelError::TrustViolation { .. })
                | LoaderError::Model(ModelError::WorkspaceOverlayDisabled(_)) => 66,
                _ => 65,
            },
            Self::Model(_) | Self::Configuration(_) | Self::Output(_) => 65,
            Self::DoctorFailed { source, .. } => {
                source.as_deref().map(CoreError::exit_code).unwrap_or(65)
            }
            Self::Core(error) => error.exit_code(),
            Self::Io { .. } => 65,
        }
    }

    pub(crate) fn io(
        operation: impl Into<String>,
        path: Option<PathBuf>,
        source: io::Error,
    ) -> Self {
        Self::Io {
            operation: operation.into(),
            path,
            source,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arguments(message) => write!(formatter, "{message}"),
            Self::Loader(error) => error.fmt(formatter),
            Self::Configuration(message) => write!(formatter, "configuration error: {message}"),
            Self::Core(error) => error.fmt(formatter),
            Self::Io {
                operation,
                path,
                source,
            } => match path {
                Some(path) => write!(formatter, "{operation} {} failed: {source}", path.display()),
                None => write!(formatter, "{operation} failed: {source}"),
            },
            Self::Model(error) => write!(formatter, "bootstrap model validation failed: {error}"),
            Self::Output(error) => write!(formatter, "could not serialize output: {error}"),
            Self::DoctorFailed {
                check,
                source: Some(source),
            } => write!(
                formatter,
                "doctor found an unhealthy configuration: {check}: {source}"
            ),
            Self::DoctorFailed {
                check,
                source: None,
            } => write!(
                formatter,
                "doctor found an unhealthy configuration: {check}"
            ),
        }
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Loader(error) => Some(error),
            Self::Model(error) => Some(error),
            Self::Core(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::Output(error) => Some(error),
            Self::DoctorFailed { source, .. } => source
                .as_deref()
                .map(|error| error as &(dyn Error + 'static)),
            Self::Arguments(_) | Self::Configuration(_) => None,
        }
    }
}

impl From<LoaderError> for CliError {
    fn from(error: LoaderError) -> Self {
        Self::Loader(error)
    }
}

impl From<ModelError> for CliError {
    fn from(error: ModelError) -> Self {
        Self::Model(error)
    }
}

impl From<CoreError> for CliError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}
