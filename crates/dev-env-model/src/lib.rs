//! Pure model for the `dev-env` environment DSL.
//!
//! This crate intentionally does not read files, inspect the process, execute
//! providers, or launch shells.  It contains the serializable schema, typed
//! input and expression parsers, provenance types, and static invariants used
//! by the loader and materializer crates.

mod backend;
mod condition;
mod config;
mod environment;
mod error;
mod identity;
mod input;
mod materialized;
mod path;
mod provenance;
mod provider;
mod request;
mod shell;
mod snapshot;
mod validation;
mod value;

pub use backend::{
    decode_backend_request, decode_backend_response, encode_backend_request,
    encode_backend_response, read_backend_request, read_backend_response, write_backend_request,
    write_backend_response, BackendError, BackendProtocolError, BackendRequest,
    BackendRequestEnvelope, BackendRequestMessage, BackendResponse, BackendResponseEnvelope,
    BackendResponseMessage, BackendState, BackendStatus, HelloInfo, PrepareResponse,
    PreparedResponse, ProviderDiagnosticSummary, ReceiptSummary, ShellInvocation, TrustTarget,
    BACKEND_PROTOCOL_VERSION, DEVENV_BACKEND_ERROR_VERSION, DEVENV_BACKEND_PROTOCOL_VERSION,
    MAX_BACKEND_FRAME_BYTES,
};

pub use condition::{Condition, ConditionError, ConditionValue};
pub use config::{
    ConfigLayer, EnvironmentLayer, MergePolicy, OverrideOperation, OverrideSpec, PolicyConfig,
    ProfileDocument, ProfileSet, ResolvedConfig, ShellSelection, UnknownInputPolicy,
    UntrustedWorkspacePolicy, WorkspaceConfig, WorkspaceSearch,
};
pub use environment::{
    ConditionalEnvironmentVariable, ConfiguredValuePrecedence, EnvironmentConfig, EnvironmentPath,
    PathMode,
};
pub use error::{ModelError, ModelErrorReason};
pub use identity::{
    EffectiveIdentity, IdentityError, IdentityPeer, IdentityRequest, IdentityRequestError,
    IdentitySnapshot, IdentitySource, PeerCredentials, RequestedIdentity, WorkspaceStatus,
};
pub use input::{InputError, InputSpec, InputType, InputValue, ParsedInput};
pub use materialized::{EnvValue, MaterializedEnv, ProviderReceipt};
pub use path::{PathRenderError, PathTemplate};
pub use provenance::{
    ConfigValue, Layer, Origin, ProvenanceEntry, ProvenanceIndex, Sensitivity, SourceId,
};
pub use provider::{
    FailurePolicy, MissingProviderPolicy, PrepareStep, ProviderConfig, ShellEnvConfig,
    ShellEnvFormat,
};
pub use request::{PrepareRequest, RequestContext, RequestMode, RequestValidationError};
pub use shell::{ShellArgError, ShellConfig, ShellEnvEntry, ShellEnvParseError, ShellKind};
pub use snapshot::{
    ConfigSnapshot, ConfigSnapshotRef, ExecutableResolutionRules, Generation, MaterializationKey,
    MaterializationKeyError, PreparePolicy, ProviderPolicy, SnapshotError, SnapshotGeneration,
    WorkspaceOverlayPolicy,
};
pub use value::ValueTree;

pub type Value = ValueTree;
pub type Profile = ProfileDocument;
pub type PathConfig = EnvironmentPath;

pub const DEV_ENV_SCHEMA_V1: u32 = 1;
