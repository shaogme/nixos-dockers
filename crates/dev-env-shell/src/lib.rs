//! Shell adapters for the `dev-env` runtime.
//!
//! This crate only constructs argv and applies an already materialized
//! environment.  It never evaluates shell source and never uses a shell to
//! launch a configured command.

mod adapter;
mod command;
mod environment;
mod error;
mod invocation;
mod shim;

use dev_env_model::{MaterializedEnv, ShellConfig};
use environment::EnvironmentRenderer;
use std::{ffi::OsString, path::Path};

pub use adapter::{ConfiguredShellAdapter, ShellAdapter};
pub use command::{CommandLine, CommandLineError};
pub use environment::{EnvironmentFormat, RenderOptions};
pub use error::{EnvironmentFormatError, ShellBuildError, ShimError, ShimPathError};
pub use invocation::{ShellInvocation, ShellInvocationError};
pub use shim::Shim;

pub fn format_environment(
    format: EnvironmentFormat,
    environment: &MaterializedEnv,
    options: RenderOptions,
) -> Result<String, EnvironmentFormatError> {
    EnvironmentRenderer::format_environment(format, environment, options)
}

pub fn format_shell(
    environment: &MaterializedEnv,
    options: RenderOptions,
) -> Result<String, EnvironmentFormatError> {
    EnvironmentRenderer::format_shell(environment, options)
}

pub fn format_dotenv(
    environment: &MaterializedEnv,
    options: RenderOptions,
) -> Result<String, EnvironmentFormatError> {
    EnvironmentRenderer::format_dotenv(environment, options)
}

pub fn format_json(
    environment: &MaterializedEnv,
    options: RenderOptions,
) -> Result<String, EnvironmentFormatError> {
    EnvironmentRenderer::format_json(environment, options)
}

pub fn build_invocation<A: ShellAdapter + ?Sized>(
    adapter: &A,
    config: &ShellConfig,
    invocation: &ShellInvocation,
) -> Result<CommandLine, ShellInvocationError> {
    invocation.build(adapter, config)
}

pub fn build_shim(real: impl AsRef<Path>, args: &[OsString]) -> Result<CommandLine, ShimError> {
    Shim::new(real.as_ref().to_path_buf()).build(args)
}

pub fn validate_shim(real: &Path, configured: &Path) -> Result<(), ShimError> {
    Shim::new(real.to_path_buf()).validate_configured_command(configured)
}
