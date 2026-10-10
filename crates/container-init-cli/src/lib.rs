mod application;
mod args;
mod config;
mod doctor;
mod error;
mod output;
mod preflight;

pub use application::run;
pub fn run_handoff_helper() -> i32 {
    preflight::run_helper()
}
pub use args::{Cli, CliCommand, CliOptions, ParseError};
pub use config::{load, runtime_context, workspace_path, LoadedConfig};
pub use doctor::{inspect, DoctorCheck, DoctorHandoff, DoctorReport, DoctorStatus};
pub use error::CliError;
