//! The pure model for container-init's Bootstrap DSL.
//!
//! This crate deliberately has no process, filesystem, shell, or POSIX user
//! handling code. It describes trusted bootstrap configuration and builds a
//! deterministic, statically validated action plan for an executor.

mod action;
mod condition;
mod config;
mod input;
mod plan;

#[cfg(test)]
mod tests;

pub use action::{
    Action, ActionKind, FailurePolicy, Idempotency, Origin, RunAs, Sensitivity, SourceKind,
};
pub use condition::{Condition, ConditionValue, PathTemplate};
pub use config::{
    BootstrapConfig, BootstrapMode, BootstrapPolicy, HandoffConfig, IdentityConfig,
    NonInteractivePolicy, MAX_BOOTSTRAP_ACTIONS, MAX_HANDOFF_PREFIX_ITEMS,
    MAX_HANDOFF_PREFIX_ITEM_BYTES, MAX_HANDOFF_PREFIX_TOTAL_BYTES,
};
pub use error::ModelError;
pub use input::{BootstrapInput, InputNamespace, InputType, InputValue, ParsedInput};
pub use plan::{Plan, PlanEffect, PlanPhase, PlannedAction};

mod error {
    use serde::Serialize;
    use std::{error::Error, fmt};

    use crate::action::SourceKind;

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
                Self::DuplicateActionId(id) => {
                    write!(formatter, "duplicate bootstrap action id {id:?}")
                }
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
}

pub const BOOTSTRAP_SCHEMA_V1: u32 = 1;
