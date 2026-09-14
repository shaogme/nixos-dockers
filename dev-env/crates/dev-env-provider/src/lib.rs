//! Provider lifecycle runtime for the `dev-env` environment DSL.
//!
//! This crate deliberately treats providers as data-driven command adapters.
//! A provider executable receives argv elements and a materialized environment;
//! no command is ever passed through a shell.  The crate is also usable with a
//! test command executor, so lifecycle behavior can be tested without any
//! real provider installed.

mod command;
mod condition;
mod context;
mod detect;
mod environment;
mod error;
mod lock;
mod protocol;
mod receipt;
mod runner;
mod template;

pub use command::{
    CommandError, CommandExecutor, CommandOutput, CommandRequest, OutputStream, ProcessExecutor,
};
pub use condition::evaluate as evaluate_condition;
pub use context::ProviderContext;
pub use detect::{
    detect, DetectionError, DetectionResult, ExecutableLocator, SystemExecutableLocator,
};
pub use environment::{
    parse_output, DotenvParseError, EnvironmentDelta, EnvironmentParseError, JsonEnvironmentError,
};
pub use error::{ProviderRuntimeError, ProviderRuntimeErrorKind};
pub use lock::{lock_key, LockError, LockManager, ProviderLockGuard};
pub use protocol::{
    decode_request, encode_response, protocol_error, ProtocolCodecError, ProtocolDiagnostic,
    ProtocolError, ProtocolOperation, ProtocolRequest, ProtocolResponse, PROTOCOL_VERSION,
};
pub use receipt::{
    fingerprint_bytes, provider_config_fingerprint, workspace_fingerprint, FingerprintError,
};
pub use runner::{
    GenericProvider, Provider, ProviderDiagnostic, ProviderRunResult, ProviderRunner,
};
pub use template::{expand_argv, TemplateError};
