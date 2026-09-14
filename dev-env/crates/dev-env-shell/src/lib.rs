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

pub use adapter::{ConfiguredShellAdapter, ShellAdapter};
pub use command::{CommandLine, CommandLineError};
pub use environment::{
    format_dotenv, format_environment, format_json, format_shell, EnvironmentFormat, RenderOptions,
};
pub use error::{
    EnvironmentFormatError, ShellBuildError, ShellInvocationError, ShimError, ShimPathError,
};
pub use invocation::{build_invocation, ShellInvocation};
pub use shim::{build_shim, validate_shim, Shim};

/// A convenient name for callers that want the generic POSIX-style adapter.
pub type PosixShellAdapter = ConfiguredShellAdapter;

/// An adapter whose command line is still passed as argv.  The alias is
/// intentionally separate at the API boundary even though command
/// construction is data-driven for both POSIX and argv-oriented shells.
pub type ArgvShellAdapter = ConfiguredShellAdapter;
