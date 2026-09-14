use dev_env_model::{ModelError, Origin, OverrideOperation};
use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

/// Errors which can occur while reading, graph-resolving, or merging sources.
///
/// Error causes remain typed all the way to callers. In particular, TOML and
/// model errors are kept as their original values instead of being rendered
/// into a `String` and losing their structure.
#[derive(Debug)]
pub enum LoaderError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Parse {
        location: String,
        source: toml::de::Error,
    },
    Serialize {
        location: String,
        source: toml::ser::Error,
    },
    Model {
        location: Option<String>,
        source: ModelError,
    },
    InvalidSourceId {
        id: String,
    },
    ProfileIdMismatch {
        source_id: String,
        document_id: String,
    },
    MissingProfile {
        id: String,
    },
    InheritanceCycle {
        profiles: Vec<String>,
    },
    DuplicateProfile {
        id: String,
        first: Option<String>,
        second: Option<String>,
    },
    Conflict {
        path: String,
        previous: Box<Origin>,
        incoming: Box<Origin>,
        remedy: ConflictRemedy,
    },
    TrustViolation {
        profile: String,
        source: crate::SourceKind,
        path: String,
        reason: TrustViolationReason,
    },
    MissingRequired {
        path: String,
    },
    OverrideTypeMismatch {
        path: String,
        operation: OverrideOperation,
        actual: ValueKind,
    },
    UnrepresentableOverride {
        path: String,
    },
    DuplicateInputName {
        name: String,
        first: String,
        second: String,
    },
    InputTargetMissing {
        name: String,
        target: String,
    },
    RuntimeInputConflict {
        input: String,
        first_name: String,
        second_name: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConflictRemedy {
    ExplicitOverride { path: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrustViolationReason {
    OnlyTrustedSourcesMayChangePolicy,
    OnlyTrustedSourcesMayDeclareInputs,
    WorkspaceOverrideNotAllowed,
    CliOverrideNotAllowed,
    ProviderNotDeclared,
    UntrustedSourceMayNotOverride,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueKind {
    Missing,
    Scalar,
    Array,
    Table,
}

impl fmt::Display for ValueKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Missing => "missing",
            Self::Scalar => "scalar",
            Self::Array => "array",
            Self::Table => "table",
        })
    }
}

impl fmt::Display for ConflictRemedy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExplicitOverride { path } => write!(
                formatter,
                "add [override.\"{path}\"] with an explicit operation and a non-empty reason"
            ),
        }
    }
}

impl fmt::Display for TrustViolationReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OnlyTrustedSourcesMayChangePolicy => {
                "only image and admin sources may change policy"
            }
            Self::OnlyTrustedSourcesMayDeclareInputs => {
                "only image and admin sources may declare inputs"
            }
            Self::WorkspaceOverrideNotAllowed => {
                "the profile policy does not allow this workspace override"
            }
            Self::CliOverrideNotAllowed => "the profile policy does not allow this CLI override",
            Self::ProviderNotDeclared => "an untrusted source may not introduce a provider",
            Self::UntrustedSourceMayNotOverride => {
                "an untrusted source may not replace an inherited declaration"
            }
        })
    }
}

impl fmt::Display for LoaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Parse { location, source } => write!(formatter, "{location}: {source}"),
            Self::Serialize { location, source } => write!(formatter, "{location}: {source}"),
            Self::Model { location, source } => match location {
                Some(location) => write!(formatter, "{location}: {source}"),
                None => write!(formatter, "environment model validation failed: {source}"),
            },
            Self::InvalidSourceId { id } => write!(formatter, "invalid profile source id {id:?}"),
            Self::ProfileIdMismatch {
                source_id,
                document_id,
            } => write!(
                formatter,
                "profile source id {source_id:?} does not match document id {document_id:?}"
            ),
            Self::MissingProfile { id } => write!(formatter, "missing parent profile {id:?}"),
            Self::InheritanceCycle { profiles } => {
                write!(formatter, "profile inheritance cycle: {}", profiles.join(" -> "))
            }
            Self::DuplicateProfile { id, first, second } => write!(
                formatter,
                "profile id {id:?} is declared by both {} and {}",
                first.as_deref().unwrap_or("<unknown>"),
                second.as_deref().unwrap_or("<unknown>")
            ),
            Self::Conflict {
                path,
                previous,
                incoming,
                remedy,
            } => write!(
                formatter,
                "DEVENV-E-CONFLICT: {path}\n  parent: {previous:?}\n  child : {incoming:?}\n  fix  : {remedy}"
            ),
            Self::TrustViolation {
                profile,
                source,
                path,
                reason,
            } => write!(
                formatter,
                "trust violation in profile {profile:?} ({source:?}) at {path:?}: {reason}"
            ),
            Self::MissingRequired { path } => {
                write!(formatter, "required configuration value {path:?} is missing")
            }
            Self::OverrideTypeMismatch {
                path,
                operation,
                actual,
            } => write!(
                formatter,
                "override {path:?} with operation {operation:?} requires an array, found {actual}"
            ),
            Self::UnrepresentableOverride { path } => write!(
                formatter,
                "override {path:?} contains a null value which TOML cannot represent"
            ),
            Self::DuplicateInputName {
                name,
                first,
                second,
            } => write!(
                formatter,
                "input name or export name {name:?} is declared by both {first:?} and {second:?}"
            ),
            Self::InputTargetMissing { name, target } => write!(
                formatter,
                "input {name:?} targets missing configuration path {target:?}"
            ),
            Self::RuntimeInputConflict {
                input,
                first_name,
                second_name,
            } => write!(
                formatter,
                "runtime input {input:?} was supplied by conflicting variables {first_name:?} and {second_name:?}"
            ),
        }
    }
}

impl Error for LoaderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::Serialize { source, .. } => Some(source),
            Self::Model { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<ModelError> for LoaderError {
    fn from(source: ModelError) -> Self {
        Self::Model {
            location: None,
            source,
        }
    }
}
