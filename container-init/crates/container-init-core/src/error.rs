use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

use bootstrap_model::{ModelError, Origin};
use container_init_posix::PosixError;
use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

/// Stable error families for the container-init CLI layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum ErrorClass {
    InvalidArguments,
    Configuration,
    Trust,
    Identity,
    Action,
    Handoff,
    Lock,
}

impl ErrorClass {
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::InvalidArguments => 64,
            Self::Configuration => 65,
            Self::Trust => 66,
            Self::Identity => 67,
            Self::Action => 68,
            Self::Handoff => 69,
            Self::Lock => 70,
        }
    }
}

#[derive(Debug)]
pub enum CoreError {
    Model(ModelError),
    Invalid {
        location: String,
        message: String,
    },
    Identity {
        message: String,
    },
    Permission {
        action: Option<String>,
        message: String,
    },
    Action {
        action: String,
        path: Option<PathBuf>,
        message: String,
    },
    Io {
        action: Option<String>,
        path: Option<PathBuf>,
        source: io::Error,
    },
    Lock {
        path: PathBuf,
        operation: LockOperation,
        source: io::Error,
    },
    Serialization {
        operation: String,
        source: serde_json::Error,
    },
    Handoff {
        program: PathBuf,
        args: Vec<String>,
        source: io::Error,
    },
    Annotated {
        action: String,
        origin: Origin,
        source: Box<Self>,
    },
}

impl CoreError {
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Model(_) | Self::Invalid { .. } => ErrorClass::Configuration,
            Self::Identity { .. } => ErrorClass::Identity,
            Self::Permission { .. } => ErrorClass::Trust,
            Self::Action { .. } | Self::Io { .. } => ErrorClass::Action,
            Self::Lock { .. } => ErrorClass::Lock,
            Self::Serialization { .. } => ErrorClass::Action,
            Self::Handoff { .. } => ErrorClass::Handoff,
            Self::Annotated { source, .. } => source.class(),
        }
    }

    pub fn exit_code(&self) -> i32 {
        self.class().exit_code()
    }

    pub(crate) fn action(action: &str, path: Option<PathBuf>, message: impl Into<String>) -> Self {
        Self::Action {
            action: action.to_owned(),
            path,
            message: message.into(),
        }
    }

    pub(crate) fn io(action: Option<&str>, path: Option<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            action: action.map(str::to_owned),
            path,
            source,
        }
    }

    pub(crate) fn from_posix(action: &str, path: Option<PathBuf>, error: PosixError) -> Self {
        match error {
            PosixError::Invalid(message) => Self::action(action, path, message),
            PosixError::Permission(message) => Self::Permission {
                action: Some(action.to_owned()),
                message,
            },
            PosixError::Io(source) => Self::io(Some(action), path, source),
        }
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(error) => {
                write!(formatter, "bootstrap model validation failed: {error}")
            }
            Self::Invalid { location, message } => write!(formatter, "{location}: {message}"),
            Self::Identity { message } => {
                write!(formatter, "identity resolution failed: {message}")
            }
            Self::Permission { action, message } => match action {
                Some(action) => write!(
                    formatter,
                    "permission denied for action {action:?}: {message}"
                ),
                None => write!(formatter, "permission denied: {message}"),
            },
            Self::Action {
                action,
                path,
                message,
            } => match path {
                Some(path) => write!(
                    formatter,
                    "bootstrap action {action:?} at {} failed: {message}",
                    path.display()
                ),
                None => write!(formatter, "bootstrap action {action:?} failed: {message}"),
            },
            Self::Io {
                action,
                path,
                source,
            } => {
                let action = action.as_deref().unwrap_or("bootstrap operation");
                match path {
                    Some(path) => {
                        write!(formatter, "{action} at {} failed: {source}", path.display())
                    }
                    None => write!(formatter, "{action} failed: {source}"),
                }
            }
            Self::Lock {
                path,
                operation,
                source,
            } => {
                if *operation == LockOperation::Acquire
                    && source.kind() == io::ErrorKind::WouldBlock
                {
                    write!(
                        formatter,
                        "bootstrap lock at {} failed: another container-init process owns the lock",
                        path.display()
                    )
                } else {
                    write!(
                        formatter,
                        "bootstrap lock at {} failed during {operation}: {source}",
                        path.display()
                    )
                }
            }
            Self::Serialization { operation, source } => {
                write!(formatter, "could not {operation}: {source}")
            }
            Self::Handoff {
                program,
                args,
                source,
            } => write!(
                formatter,
                "handoff exec {} {:?} failed: {source}",
                program.display(),
                args
            ),
            Self::Annotated {
                action,
                origin,
                source,
            } => write!(
                formatter,
                "bootstrap action {action:?} from profile {:?} ({:?}) failed: {source}",
                origin.profile, origin.source
            ),
        }
    }
}

impl Error for CoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Model(error) => Some(error),
            Self::Io { source, .. } | Self::Handoff { source, .. } => Some(source),
            Self::Lock { source, .. } => Some(source),
            Self::Serialization { source, .. } => Some(source),
            Self::Annotated { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl Serialize for CoreError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut value = serializer.serialize_struct("CoreError", 4)?;
        value.serialize_field("class", &self.class())?;
        match self {
            Self::Model(error) => {
                value.serialize_field("kind", "model")?;
                value.serialize_field("error", error)?;
            }
            Self::Invalid { location, message } => {
                value.serialize_field("kind", "invalid")?;
                value.serialize_field("location", location)?;
                value.serialize_field("message", message)?;
            }
            Self::Identity { message } => {
                value.serialize_field("kind", "identity")?;
                value.serialize_field("message", message)?;
            }
            Self::Permission { action, message } => {
                value.serialize_field("kind", "permission")?;
                value.serialize_field("action", action)?;
                value.serialize_field("message", message)?;
            }
            Self::Action {
                action,
                path,
                message,
            } => {
                value.serialize_field("kind", "action")?;
                value.serialize_field("action", action)?;
                value.serialize_field("path", path)?;
                value.serialize_field("message", message)?;
            }
            Self::Io {
                action,
                path,
                source,
            } => {
                value.serialize_field("kind", "io")?;
                value.serialize_field("action", action)?;
                value.serialize_field("path", path)?;
                value.serialize_field("io_kind", &format_args!("{:?}", source.kind()))?;
                value.serialize_field("raw_os_error", &source.raw_os_error())?;
            }
            Self::Lock {
                path,
                operation,
                source,
            } => {
                value.serialize_field("kind", "lock")?;
                value.serialize_field("path", path)?;
                value.serialize_field("operation", operation)?;
                value.serialize_field("io_kind", &format_args!("{:?}", source.kind()))?;
                value.serialize_field("raw_os_error", &source.raw_os_error())?;
            }
            Self::Serialization { operation, source } => {
                value.serialize_field("kind", "serialization")?;
                value.serialize_field("operation", operation)?;
                value.serialize_field("line", &source.line())?;
                value.serialize_field("column", &source.column())?;
            }
            Self::Handoff {
                program,
                args,
                source,
            } => {
                value.serialize_field("kind", "handoff")?;
                value.serialize_field("program", program)?;
                value.serialize_field("args", args)?;
                value.serialize_field("io_kind", &format_args!("{:?}", source.kind()))?;
                value.serialize_field("raw_os_error", &source.raw_os_error())?;
            }
            Self::Annotated {
                action,
                origin,
                source,
            } => {
                value.serialize_field("kind", "annotated")?;
                value.serialize_field("action", action)?;
                value.serialize_field("origin", origin)?;
                value.serialize_field("source", source)?;
            }
        }
        value.end()
    }
}

use crate::lock::LockOperation;
