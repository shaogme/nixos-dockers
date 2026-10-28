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
mod materializer;
mod service;

pub use config_tree::ConfigTreeError;
pub use context::{
    ContextError, ContextPathError, IdentityBroker, IdentityBrokerError, MaterializeContext,
    RuntimeContext,
};
pub use error::{CoreError, CoreErrorKind, FingerprintError};
pub use materializer::{
    Materialization, MaterializationDiagnostic, MaterializationMetadata, Materializer,
};
pub use service::{
    MaterializationService, MaterializationServiceError, MaterializationServiceResult,
};

pub use dev_env_model::{
    BackendError, BackendRequest, BackendRequestMessage, BackendResponse, BackendResponseMessage,
    ConfigSnapshot, ConfigSnapshotRef, EffectiveIdentity, EnvValue, Generation, IdentityPeer,
    IdentityRequest, IdentitySnapshot, MaterializationKey, MaterializedEnv, PeerCredentials,
    PrepareRequest, ProviderReceipt, RequestContext, RequestMode, RequestedIdentity,
    ResolvedConfig, DEVENV_BACKEND_PROTOCOL_VERSION,
};
pub use dev_env_provider::{
    ProviderDiagnostic, ProviderJob, ProviderRunResult, ProviderRuntimeError, ProviderSupervisor,
};
