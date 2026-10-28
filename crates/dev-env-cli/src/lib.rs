//! The `dev-env` command-line adapter.
//!
//! The environment model, loader, provider runtime, and shell argv builder
//! remain separate crates.  This crate only connects them to the process
//! boundary and keeps each CLI concern in its own module.

mod application;
mod args;
mod backend;
mod backend_support;
mod config;
mod doctor;
mod error;
mod execution;
mod explain;
mod identity_broker;
mod output;
mod trust;

use {
    dev_env_core::RuntimeContext,
    std::{ffi::OsString, path::Path},
};

pub use {
    application::Application,
    args::{BackendCommand, BackendInitial, Cli, CliCommand, CliOptions, OutputFormat, ParseError},
    backend::BackendApi,
    backend_support::{
        BackendSupport, BackendTransport, ConnectionRequestError, BACKEND_ABSTRACT_ENDPOINT,
        CONNECTION_TIMEOUT, ENDPOINT_DISPLAY, MAX_ACTIVE_CONNECTIONS, MAX_ACTIVE_REQUESTS,
    },
    config::{
        ConfigApi, LoadedConfig, DEFAULT_ADMIN_CONFIG, DEFAULT_PROFILES_DIR, DEFAULT_PROFILE_FILE,
    },
    doctor::{DoctorApi, DoctorCheck, DoctorIssue, DoctorReport, DoctorStatus},
    error::{
        BootstrapError, BootstrapPathError, CliError, ConfigurationError, DefaultProfileReason,
        IoOperation, OutputError, TrustError,
    },
    execution::{OfflineApi, ProcessApi},
    explain::{ExplainApi, ExplainProvenance, ExplainReport},
    identity_broker::{BrokerError, IdentityBroker},
    output::OutputApi,
    trust::TrustApi,
};

pub fn run<I>(arguments: I) -> Result<i32, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    Application::run(arguments)
}

pub fn load(options: &CliOptions) -> Result<LoadedConfig, CliError> {
    ConfigApi::load(options)
}

pub fn runtime_context(
    loaded: &LoadedConfig,
    shell: Option<&str>,
) -> Result<RuntimeContext, CliError> {
    ConfigApi::runtime_context(loaded, shell)
}

pub fn inspect(loaded: &LoadedConfig) -> DoctorReport {
    DoctorApi::inspect(loaded)
}

pub fn build_report(loaded: &LoadedConfig, path: Option<&str>) -> Result<ExplainReport, CliError> {
    ExplainApi::build_report(loaded, path)
}

pub fn trust_path(target: &Path) -> Result<String, TrustError> {
    TrustApi::trust(target)
}
