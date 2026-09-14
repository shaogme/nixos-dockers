use crate::args::OutputFormat;
use crate::error::{CliError, OutputError};
use dev_env_core::Materialization;
use dev_env_shell::{format_environment, EnvironmentFormat, RenderOptions};
use std::io::{self, Write};

pub fn print_environment(
    materialization: &Materialization,
    format: OutputFormat,
    show_secrets: bool,
) -> Result<(), CliError> {
    let environment = materialization.environment();
    let rendered = format_environment(
        environment_format(format),
        environment,
        if show_secrets {
            RenderOptions::show_secrets()
        } else {
            RenderOptions::default()
        },
    )?;
    write_stdout(&rendered)?;
    if !rendered.ends_with('\n') {
        write_stdout("\n")?;
    }
    Ok(())
}

pub fn environment_format(format: OutputFormat) -> EnvironmentFormat {
    match format {
        OutputFormat::Dotenv => EnvironmentFormat::Dotenv,
        OutputFormat::Json => EnvironmentFormat::Json,
        OutputFormat::Shell => EnvironmentFormat::Shell,
    }
}

pub fn write_stdout(value: &str) -> Result<(), CliError> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(value.as_bytes())
        .map_err(OutputError::Io)
        .map_err(CliError::Output)
}

pub fn print_version() -> Result<(), CliError> {
    write_stdout(&format!("dev-env {}\n", env!("CARGO_PKG_VERSION")))
}

pub fn print_doctor(report: &crate::doctor::DoctorReport, json: bool) -> Result<(), CliError> {
    if json {
        let rendered = serde_json::to_string_pretty(report).map_err(OutputError::Json)?;
        return write_stdout(&format!("{rendered}\n"));
    }
    let mut rendered = String::new();
    rendered.push_str(&format!("profile: {}\n", report.profile));
    rendered.push_str(&format!(
        "profile-chain: {}\n",
        report.profile_chain.join(" -> ")
    ));
    rendered.push_str(&format!("workspace: {}\n", report.workspace));
    rendered.push_str(&format!("cwd: {}\n", report.cwd));
    rendered.push_str(&format!(
        "config-fingerprint: {}\n",
        report.config_fingerprint
    ));
    for check in &report.checks {
        rendered.push_str(&format!(
            "{:>5} {}: {}\n",
            format!("{:?}", check.status).to_lowercase(),
            check.name,
            check.message
        ));
    }
    rendered.push_str(&format!(
        "status: {}\n",
        if report.ok { "ok" } else { "failed" }
    ));
    write_stdout(&rendered)
}
