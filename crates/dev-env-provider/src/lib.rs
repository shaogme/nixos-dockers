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
mod protocol;
mod receipt;
mod runner;
mod supervisor;
mod template;
mod worker;
mod worker_protocol;

use dev_env_model::{Condition, ProviderConfig, ShellEnvEntry, ShellEnvFormat};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub use command::{
    CommandError, CommandExecutor, CommandOutput, CommandRequest, OutputStream, ProcessExecutor,
    ProviderProcessSupervisor,
};
pub use condition::ConditionEvaluator;
pub use context::ProviderContext;
pub use detect::{
    DetectionError, DetectionResult, ExecutableLocator, ProviderDetection, SystemExecutableLocator,
};
pub use environment::{
    DotenvParseError, EnvironmentDelta, EnvironmentParseError, JsonEnvironmentError,
};
pub use error::{ProviderRuntimeError, ProviderRuntimeErrorKind};
pub use protocol::{
    ProtocolCodec, ProtocolCodecError, ProtocolDiagnostic, ProtocolError, ProtocolOperation,
    ProtocolRequest, ProtocolResponse, PROTOCOL_VERSION,
};
pub use receipt::{FingerprintError, ProviderFingerprints};
pub use runner::{
    GenericProvider, Provider, ProviderDiagnostic, ProviderRunResult, ProviderRunResultParts,
    ProviderRunner,
};
pub use supervisor::{ProviderJob, ProviderJobParts, ProviderSupervisor};
pub use template::{ProviderTemplate, TemplateError};
pub use worker::ProviderWorker;

pub fn evaluate_condition(condition: &Condition, context: &ProviderContext) -> bool {
    ConditionEvaluator::evaluate(condition, context)
}

pub fn detect(
    config: &ProviderConfig,
    workspace: &Path,
    environment: &BTreeMap<String, String>,
    locator: &dyn ExecutableLocator,
) -> Result<DetectionResult, DetectionError> {
    ProviderDetection::detect(config, workspace, environment, locator)
}

pub fn parse_output(
    format: ShellEnvFormat,
    output: &str,
) -> Result<Vec<ShellEnvEntry>, EnvironmentParseError> {
    EnvironmentDelta::parse_output(format, output)
}

pub fn decode_request(input: &[u8]) -> Result<ProtocolRequest, ProtocolCodecError> {
    ProtocolCodec::decode_request(input)
}

pub fn encode_response(response: &ProtocolResponse) -> Result<Vec<u8>, ProtocolCodecError> {
    ProtocolCodec::encode_response(response)
}

pub fn protocol_error(error: &ProviderRuntimeError) -> ProtocolError {
    ProtocolCodec::protocol_error(error)
}

pub fn fingerprint_bytes(bytes: &[u8]) -> [u8; 32] {
    ProviderFingerprints::fingerprint_bytes(bytes)
}

pub fn provider_config_fingerprint(config: &ProviderConfig) -> Result<[u8; 32], FingerprintError> {
    ProviderFingerprints::provider_config(config)
}

pub fn workspace_fingerprint(
    workspace: &Path,
    matched_files: &[PathBuf],
) -> Result<[u8; 32], FingerprintError> {
    ProviderFingerprints::workspace(workspace, matched_files)
}

pub fn expand_argv(
    arguments: &[String],
    provider: &str,
    context: &ProviderContext,
) -> Result<Vec<String>, TemplateError> {
    ProviderTemplate::expand_argv(arguments, provider, context)
}
