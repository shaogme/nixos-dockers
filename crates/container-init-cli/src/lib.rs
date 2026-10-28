mod application;
mod args;
mod config;
mod doctor;
mod error;
mod output;
mod preflight;

pub(crate) use application::Application;
pub use args::{Cli, CliCommand, CliOptions, ParseError};
pub use config::{ConfigLoader, LoadedConfig};
use container_init_core::RuntimeContext;
pub use doctor::{DoctorCheck, DoctorHandoff, DoctorInspector, DoctorReport, DoctorStatus};
pub use error::CliError;
pub(crate) use output::Output;
pub(crate) use preflight::{PreflightChild, PreflightRunner};
use std::{ffi::OsString, path::PathBuf};

pub fn run<I>(arguments: I) -> Result<i32, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    Application::run(arguments)
}

pub fn load(options: &CliOptions) -> Result<LoadedConfig, CliError> {
    ConfigLoader::load(options)
}

pub fn runtime_context(
    options: &CliOptions,
    change_directory: bool,
) -> Result<RuntimeContext, CliError> {
    ConfigLoader::runtime_context(options, change_directory)
}

pub fn workspace_path(options: &CliOptions) -> Result<PathBuf, CliError> {
    ConfigLoader::workspace_path(options)
}

pub fn inspect(loaded: &LoadedConfig, context: &RuntimeContext) -> DoctorReport {
    DoctorInspector::inspect(loaded, context)
}

pub fn run_handoff_helper() -> i32 {
    PreflightRunner::run_helper()
}
