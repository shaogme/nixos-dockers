use std::error::Error;
use std::fmt;
use std::io;

/// Errors returned by the small POSIX boundary used by container-init.
///
/// The crate intentionally keeps this error independent from the bootstrap
/// model. The core executor can attach an action id and provenance while
/// callers that use this crate directly still get a useful classification.
#[derive(Debug)]
pub enum PosixError {
    Invalid(String),
    Permission(String),
    Io(io::Error),
}

impl PosixError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    pub(crate) fn permission(message: impl Into<String>) -> Self {
        Self::Permission(message.into())
    }

    pub(crate) fn io(source: io::Error) -> Self {
        Self::Io(source)
    }
}

impl fmt::Display for PosixError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "invalid POSIX operation: {message}"),
            Self::Permission(message) => write!(formatter, "POSIX permission denied: {message}"),
            Self::Io(source) => source.fmt(formatter),
        }
    }
}

impl Error for PosixError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
            Self::Invalid(_) | Self::Permission(_) => None,
        }
    }
}
