//! Runtime materialization for the `dev-env` environment DSL.
//!
//! This crate is intentionally the boundary between a resolved, declarative
//! configuration and a process environment.  It does not load TOML, launch a
//! shell, or know about a particular provider such as mise or Devbox.  Those
//! responsibilities stay in the loader, shell, and provider crates.

mod config_tree;
mod context;
mod environment;
mod error;
mod fingerprint;
mod materializer;
mod provider;

pub use config_tree::ConfigTreeError;
pub use context::{ContextError, ContextPathError, MaterializeContext, RuntimeContext};
pub use error::{CoreError, CoreErrorKind};
pub use fingerprint::FingerprintError;
pub use materializer::{Materialization, MaterializationDiagnostic, Materializer};
pub use provider::ProviderRuntime;

pub use dev_env_model::{EnvValue, MaterializedEnv, ProviderReceipt, ResolvedConfig};
pub use dev_env_provider::{ProviderDiagnostic, ProviderRunResult, ProviderRuntimeError};
