//! The `dev-env` command-line adapter.
//!
//! The environment model, loader, provider runtime, and shell argv builder
//! remain separate crates.  This crate only connects them to the process
//! boundary and keeps each CLI concern in its own module.

mod application;
mod args;
mod config;
mod doctor;
mod error;
mod explain;
mod output;
mod process;
mod trust;

pub use application::run;
pub use args::{Cli, CliCommand, CliOptions, OutputFormat, ParseError};
pub use config::{
    load, runtime_context, LoadedConfig, DEFAULT_ADMIN_CONFIG, DEFAULT_PROFILES_DIR,
    DEFAULT_PROFILE_FILE,
};
pub use doctor::{inspect, DoctorCheck, DoctorIssue, DoctorReport, DoctorStatus};
pub use error::{
    BootstrapError, BootstrapPathError, CliError, ConfigurationError, DefaultProfileReason,
    IoOperation, OutputError, TrustError,
};
pub use explain::{build_report, ExplainProvenance, ExplainReport};
pub use trust::trust as trust_path;
