use std::error::Error;
use std::fmt;

use crate::provenance::SourceKind;
use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ModelError {
    Invalid {
        location: String,
        message: String,
    },
    DuplicateActionId(String),
    MissingDependency {
        action: String,
        dependency: String,
    },
    MissingIdentityResolve(String),
    DependencyCycle(Vec<String>),
    PhaseViolation {
        action: String,
        dependency: String,
    },
    TrustViolation {
        action: String,
        source: SourceKind,
        message: String,
    },
    WorkspaceOverlayDisabled(String),
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { location, message } => write!(formatter, "{location}: {message}"),
            Self::DuplicateActionId(id) => write!(formatter, "duplicate bootstrap action id {id:?}"),
            Self::MissingDependency { action, dependency } => write!(
                formatter,
                "bootstrap action {action:?} depends on missing action {dependency:?}"
            ),
            Self::MissingIdentityResolve(action) => write!(
                formatter,
                "bootstrap action {action:?} references identity but no identity.resolve action exists"
            ),
            Self::DependencyCycle(actions) => {
                write!(formatter, "bootstrap action dependency cycle: {}", actions.join(" -> "))
            }
            Self::PhaseViolation { action, dependency } => write!(
                formatter,
                "bootstrap action {action:?} depends on later-phase action {dependency:?}"
            ),
            Self::TrustViolation {
                action,
                source,
                message,
            } => write!(formatter, "bootstrap action {action:?} from {source:?}: {message}"),
            Self::WorkspaceOverlayDisabled(action) => write!(
                formatter,
                "bootstrap workspace overlay action {action:?} is disabled by policy"
            ),
        }
    }
}

impl Error for ModelError {}
