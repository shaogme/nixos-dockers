//! Runtime execution for the Bootstrap DSL.
//!
//! This crate intentionally knows about POSIX primitives, but not about any
//! image, provider, shell environment, or development tool. Configuration is
//! supplied by `container-init-bootstrap-model`; loading and trust-aware merging remain in
//! `container-init-bootstrap-loader`.

mod cgroup;
mod condition;
mod context;
mod error;
mod executor;
mod filesystem;
mod handoff;
mod identity;
mod report;
mod ssh;

pub use condition::ConditionContext;
pub use container_init_posix::{PosixSystem, WorkspaceObservation};
pub use context::RuntimeContext;
pub use error::{CoreError, ErrorClass};
pub use executor::{ActionExecutor, PlanExecutor};
pub use handoff::{HandoffCommand, HandoffContext};
pub use identity::{
    IdentityResolver, IdentitySource, ParsedRuntimeInput, ResolvedIdentity, ResolvedInputs,
    RuntimeInput, WorkspaceStatus,
};
pub use report::{ActionOutcome, ActionStatus, ExecutionOptions, ExecutionReport, PreparedExec};
pub use ssh::SshCapability;
