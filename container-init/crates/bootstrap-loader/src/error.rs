use bootstrap_model::{ModelError, Origin, SourceKind};
use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;
use toml::de::Error as TomlError;

#[derive(Debug)]
pub enum LoaderError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Parse {
        location: String,
        source: TomlError,
    },
    Invalid {
        location: String,
        message: String,
    },
    Model(ModelError),
    MissingProfile(String),
    InheritanceCycle(Vec<String>),
    DuplicateProfile {
        id: String,
        first: Option<String>,
        second: Option<String>,
    },
    DuplicateInput {
        profile: String,
        name: String,
    },
    DuplicateActionId {
        profile: String,
        action: String,
    },
    Conflict {
        path: String,
        previous: Box<Origin>,
        incoming: Box<Origin>,
        remedy: String,
    },
    TrustViolation {
        profile: String,
        source: SourceKind,
        path: String,
        message: String,
    },
}

impl fmt::Display for LoaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Parse { location, source } => write!(formatter, "{location}: {source}"),
            Self::Invalid { location, message } => write!(formatter, "{location}: {message}"),
            Self::Model(error) => write!(formatter, "bootstrap model validation failed: {error}"),
            Self::MissingProfile(id) => write!(formatter, "missing parent profile {id:?}"),
            Self::InheritanceCycle(cycle) => {
                write!(formatter, "profile inheritance cycle: {}", cycle.join(" -> "))
            }
            Self::DuplicateProfile { id, first, second } => write!(
                formatter,
                "profile id {id:?} is declared by both {} and {}",
                first.as_deref().unwrap_or("<unknown>"),
                second.as_deref().unwrap_or("<unknown>")
            ),
            Self::DuplicateInput { profile, name } => {
                write!(formatter, "profile {profile:?} declares input {name:?} more than once")
            }
            Self::DuplicateActionId { profile, action } => write!(
                formatter,
                "profile {profile:?} declares bootstrap action {action:?} more than once"
            ),
            Self::Conflict {
                path,
                previous,
                incoming,
                remedy,
            } => write!(
                formatter,
                "bootstrap conflict at {path:?}: {:?} ({:?}) conflicts with {:?} ({:?}); {remedy}",
                previous.profile,
                previous.source,
                incoming.profile,
                incoming.source
            ),
            Self::TrustViolation {
                profile,
                source,
                path,
                message,
            } => write!(
                formatter,
                "bootstrap trust violation in profile {profile:?} ({source:?}) at {path:?}: {message}"
            ),
        }
    }
}

impl Error for LoaderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Model(error) => Some(error),
            Self::Parse { source, .. } => Some(source),
            _ => None,
        }
    }
}
