use crate::args::{CliOptions, OutputFormat};
use crate::config::{self, LoadedConfig};
use crate::{doctor, explain, output, process, trust};
use dev_env_core::{Materialization, Materializer};
use dev_env_shell::{build_invocation, CommandLine, ConfiguredShellAdapter, ShellInvocation};
use std::ffi::OsString;
use std::path::Path;

pub(crate) fn print(
    options: &CliOptions,
    format: OutputFormat,
    shell: Option<&str>,
) -> Result<i32, crate::error::CliError> {
    let loaded = config::load(options)?;
    let materialization = materialize(&loaded, shell)?;
    output::print_environment(&materialization, format, options.show_secrets)?;
    Ok(0)
}

pub(crate) fn explain(
    options: &CliOptions,
    path: Option<&str>,
    json: bool,
) -> Result<i32, crate::error::CliError> {
    let loaded = config::load(options)?;
    let report = explain::build_report(&loaded, path)?;
    explain::print_report(&report, json)?;
    Ok(0)
}

pub(crate) fn doctor(options: &CliOptions, json: bool) -> Result<i32, crate::error::CliError> {
    let loaded = config::load(options)?;
    let report = doctor::inspect(&loaded);
    output::print_doctor(&report, json)?;
    if report.ok {
        Ok(0)
    } else {
        Err(crate::error::CliError::DoctorFailed {
            failed_checks: report.failed_checks().count(),
        })
    }
}

pub(crate) fn trust(target: &Path) -> Result<i32, crate::error::CliError> {
    let hash = trust::trust(target)?;
    output::write_stdout(&format!("trusted: {hash}\n"))?;
    Ok(0)
}

pub(crate) fn exec(
    options: &CliOptions,
    command: Vec<OsString>,
) -> Result<i32, crate::error::CliError> {
    let program = command
        .first()
        .cloned()
        .ok_or(crate::error::CliError::Backend(
            "offline exec requires a command".to_owned(),
        ))?;
    let line = CommandLine::try_new(program, command.into_iter().skip(1).collect::<Vec<_>>())
        .map_err(crate::error::CliError::CommandLine)?;
    let loaded = config::load(options)?;
    let materialization = materialize(&loaded, None)?;
    process::execute(line, &materialization.environment, loaded.cwd())
}

pub(crate) fn shell(
    options: &CliOptions,
    shell: Option<&str>,
    login: bool,
    args: Vec<OsString>,
) -> Result<i32, crate::error::CliError> {
    let loaded = config::load(options)?;
    let shell_id = shell
        .map(str::to_owned)
        .unwrap_or_else(|| loaded.config().shell.default.clone());
    let materialization = materialize(&loaded, Some(&shell_id))?;
    let shell_config = loaded
        .config()
        .shells
        .get(&shell_id)
        .ok_or_else(|| crate::error::CliError::Backend(format!("unknown shell {shell_id}")))?;
    let adapter = ConfiguredShellAdapter::new(shell_id);
    let invocation = if login {
        ShellInvocation::login(args)
    } else {
        ShellInvocation::interactive(args)
    };
    let line = build_invocation(&adapter, shell_config, &invocation)?;
    process::execute(line, &materialization.environment, loaded.cwd())
}

pub(crate) fn shim(
    options: &CliOptions,
    shell: &str,
    real: &Path,
    args: Vec<OsString>,
) -> Result<i32, crate::error::CliError> {
    let loaded = config::load(options)?;
    let materialization = materialize(&loaded, Some(shell))?;
    let line = dev_env_shell::Shim::new(real).build(&args)?;
    process::execute(line, &materialization.environment, loaded.cwd())
}

fn materialize(
    loaded: &LoadedConfig,
    shell: Option<&str>,
) -> Result<Materialization, crate::error::CliError> {
    let context = config::runtime_context(loaded, shell)?;
    Materializer::try_new(loaded.config().clone())?
        .materialize(&context)
        .map_err(crate::error::CliError::Core)
}
