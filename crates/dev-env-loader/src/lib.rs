//! Parsing and merging for the environment half of the `dev-env` DSL.
//!
//! The loader is deliberately side-effect free. It reads profile sources and
//! produces a validated [`dev_env_model::ResolvedConfig`]; provider execution,
//! process inspection, and shell launching belong to later crates.

mod error;
mod loader;
mod merge;
mod raw;
mod runtime;
mod source;

pub use error::{ConflictRemedy, LoaderError, TrustViolationReason, ValueKind};
pub use loader::{DevEnvLoader, ProfileLoader};
pub use source::{CliPatch, LoadedConfig, LoadedProfile, ProfileSource, SourceKind};
