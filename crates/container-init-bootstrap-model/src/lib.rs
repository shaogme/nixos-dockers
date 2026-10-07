//! The pure model for container-init's Bootstrap DSL.
//!
//! This crate deliberately has no process, filesystem, shell, or POSIX user
//! handling code. It describes trusted bootstrap configuration and builds a
//! deterministic, statically validated action plan for an executor.

mod action;
mod condition;
mod config;
mod error;
mod input;
mod path;
mod plan;
mod provenance;
mod validation;

#[cfg(test)]
mod tests;

pub use action::{Action, ActionKind, FailurePolicy, Idempotency, RunAs, Sensitivity};
pub use condition::{Condition, ConditionValue};
pub use config::{
    BootstrapConfig, BootstrapMode, BootstrapPolicy, HandoffConfig, IdentityConfig,
    NonInteractivePolicy,
};
pub use error::ModelError;
pub use input::{BootstrapInput, InputNamespace, InputType, InputValue, ParsedInput};
pub use path::PathTemplate;
pub use plan::{Plan, PlanEffect, PlanPhase, PlannedAction};
pub use provenance::{Origin, SourceKind};

pub const BOOTSTRAP_SCHEMA_V1: u32 = 1;
