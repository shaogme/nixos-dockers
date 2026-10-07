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
mod identity;
mod materializer;
mod provider;
mod service;

pub use config_tree::ConfigTreeError;
pub use context::{ContextError, ContextPathError, MaterializeContext, RuntimeContext};
pub use dev_env_provider::{ProviderJob, ProviderSupervisor};
pub use error::{CoreError, CoreErrorKind};
pub use fingerprint::FingerprintError;
pub use identity::{IdentityBroker, IdentityBrokerError};
pub use materializer::MaterializationMetadata;
pub use materializer::{Materialization, MaterializationDiagnostic, Materializer};
pub use provider::{LegacyProviderSupervisor, ProviderRuntime};
pub use service::{
    MaterializationService, MaterializationServiceError, MaterializationServiceResult,
};

pub use dev_env_model::{
    BackendError, BackendRequest, BackendRequestMessage, BackendResponse, BackendResponseMessage,
    ConfigSnapshot, ConfigSnapshotRef, EffectiveIdentity, Generation, IdentityPeer,
    IdentityRequest, IdentitySnapshot, MaterializationKey, PeerCredentials, PrepareRequest,
    RequestContext, RequestMode, RequestedIdentity, DEVENV_BACKEND_PROTOCOL_VERSION,
};
pub use dev_env_model::{EnvValue, MaterializedEnv, ProviderReceipt, ResolvedConfig};
pub use dev_env_provider::{ProviderDiagnostic, ProviderRunResult, ProviderRuntimeError};
