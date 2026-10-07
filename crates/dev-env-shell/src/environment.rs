use crate::error::EnvironmentFormatError;
use dev_env_model::{EnvValue, MaterializedEnv, Sensitivity};
use serde_json::Value;
use std::collections::BTreeMap;

/// Formats supported by the read-only environment printing interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentFormat {
    Shell,
    Dotenv,
    Json,
}

/// Controls redaction when an environment is rendered for human or machine
/// consumption.  Redaction never changes the environment injected into a
/// child process.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderOptions {
    pub show_secrets: bool,
}

impl RenderOptions {
    pub const fn show_secrets() -> Self {
        Self { show_secrets: true }
    }
}

pub fn format_environment(
    format: EnvironmentFormat,
    environment: &MaterializedEnv,
    options: RenderOptions,
) -> Result<String, EnvironmentFormatError> {
    environment
        .validate()
        .map_err(|source| EnvironmentFormatError::Model { source })?;
    match format {
        EnvironmentFormat::Shell => format_shell_validated(environment, options),
        EnvironmentFormat::Dotenv => format_dotenv_validated(environment, options),
        EnvironmentFormat::Json => format_json_validated(environment, options),
    }
}

pub fn format_shell(
    environment: &MaterializedEnv,
    options: RenderOptions,
) -> Result<String, EnvironmentFormatError> {
    format_environment(EnvironmentFormat::Shell, environment, options)
}

pub fn format_dotenv(
    environment: &MaterializedEnv,
    options: RenderOptions,
) -> Result<String, EnvironmentFormatError> {
    format_environment(EnvironmentFormat::Dotenv, environment, options)
}

pub fn format_json(
    environment: &MaterializedEnv,
    options: RenderOptions,
) -> Result<String, EnvironmentFormatError> {
    format_environment(EnvironmentFormat::Json, environment, options)
}

fn format_shell_validated(
    environment: &MaterializedEnv,
    options: RenderOptions,
) -> Result<String, EnvironmentFormatError> {
    let mut output = String::new();
    for (name, value) in &environment.values {
        output.push_str("export ");
        output.push_str(name);
        output.push('=');
        output.push_str(&quote_posix(rendered_value(value, options)));
        output.push('\n');
    }
    Ok(output)
}

fn format_dotenv_validated(
    environment: &MaterializedEnv,
    options: RenderOptions,
) -> Result<String, EnvironmentFormatError> {
    let mut output = String::new();
    for (name, value) in &environment.values {
        output.push_str(name);
        output.push('=');
        output.push_str(&quote_posix(rendered_value(value, options)));
        output.push('\n');
    }
    Ok(output)
}

fn format_json_validated(
    environment: &MaterializedEnv,
    options: RenderOptions,
) -> Result<String, EnvironmentFormatError> {
    let values = environment
        .values
        .iter()
        .map(|(name, value)| {
            (
                name.clone(),
                Value::String(rendered_value(value, options).to_owned()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    serde_json::to_string(&values).map_err(|source| EnvironmentFormatError::Json { source })
}

fn rendered_value(value: &EnvValue, options: RenderOptions) -> &str {
    if options.show_secrets || value.sensitivity == Sensitivity::Public {
        &value.value
    } else {
        "<redacted>"
    }
}

/// Quote one environment value for a POSIX assignment without invoking a
/// shell.  Single quotes make `$`, backticks, command substitutions, pipes,
/// and redirections data rather than syntax.
fn quote_posix(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('\'');
    let mut first = true;
    for part in value.split('\'') {
        if !first {
            output.push_str("'\\''");
        }
        output.push_str(part);
        first = false;
    }
    output.push('\'');
    output
}
