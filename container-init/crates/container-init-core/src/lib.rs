//! Runtime execution for the Bootstrap DSL.
//!
//! This crate intentionally knows about POSIX primitives, but not about any
//! image, provider, shell environment, or development tool. Configuration is
//! supplied by `bootstrap-model`; loading and trust-aware merging remain in
//! `bootstrap-loader`.

mod condition;
mod context;
mod error;
mod executor;
mod filesystem;
mod handoff;
mod identity;
mod lock;
mod receipt;
mod ssh;

pub use condition::ConditionContext;
pub use container_init_posix::PosixSystem;
pub use container_init_posix::WorkspaceObservation;
pub use context::RuntimeContext;
pub use error::{CoreError, ErrorClass};
pub use executor::{
    ActionExecutor, ActionOutcome, ActionStatus, ExecutionOptions, ExecutionReport, PlanExecutor,
};
pub use handoff::HandoffCommand;
pub use identity::{
    IdentityResolver, IdentitySource, ParsedRuntimeInput, ResolvedIdentity, ResolvedInputs,
    RuntimeInput, WorkspaceStatus,
};
pub use lock::{BootstrapLock, LockOperation};
pub use ssh::SshCapability;
