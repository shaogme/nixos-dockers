use crate::args::{Cli, CliCommand, CliOptions};
use crate::config::LoadedConfig;
use crate::error::CliError;
use crate::{config, doctor, explain, output, process, trust};
use dev_env_core::{ContextError, CoreError, Materialization, Materializer};
use dev_env_model::ShellConfig;
use dev_env_shell::{build_invocation, CommandLine, ConfiguredShellAdapter, ShellInvocation, Shim};
use std::ffi::OsString;
use std::path::Path;

pub fn run<I>(arguments: I) -> Result<i32, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let cli = Cli::parse(arguments)?;
    match cli.command {
        CliCommand::Help => {
            output::write_stdout(&crate::args::usage(""))?;
            output::write_stdout("\n")?;
            Ok(0)
        }
        CliCommand::Version => {
            output::print_version()?;
            Ok(0)
        }
        CliCommand::Exec { command } => run_exec(&cli.options, command),
        CliCommand::Shell { shell, login, args } => {
            run_shell(&cli.options, shell.as_deref(), login, args)
        }
        CliCommand::LoginShell { args } => run_shell(&cli.options, None, true, args),
        CliCommand::Shim { shell, real, args } => run_shim(&cli.options, &shell, &real, args),
        CliCommand::Print { format, shell } => {
            let loaded = config::load(&cli.options)?;
            let materialization = materialize(&loaded, shell.as_deref())?;
            output::print_environment(&materialization, format, cli.options.show_secrets)?;
            Ok(0)
        }
        CliCommand::Explain { path, json } => {
            let loaded = config::load(&cli.options)?;
            let report = explain::build_report(&loaded, path.as_deref())?;
            explain::print_report(&report, json)?;
            Ok(0)
        }
        CliCommand::Doctor { json } => {
            let loaded = config::load(&cli.options)?;
            let report = doctor::inspect(&loaded);
            output::print_doctor(&report, json)?;
            if report.ok {
                Ok(0)
            } else {
                Err(CliError::DoctorFailed {
                    failed_checks: report.failed_checks().count(),
                })
            }
        }
        CliCommand::Trust { target } => {
            let hash = trust::trust(&target)?;
            output::write_stdout(&format!("trusted: {hash}\n"))?;
            Ok(0)
        }
    }
}

fn run_exec(options: &CliOptions, command: Vec<OsString>) -> Result<i32, CliError> {
    let loaded = config::load(options)?;
    let materialization = materialize(&loaded, None)?;
    let mut arguments = command.into_iter();
    let program = arguments
        .next()
        .expect("argument parser requires a command");
    let line = CommandLine::try_new(program, arguments.collect::<Vec<_>>())?;
    process::execute(line, materialization.environment(), loaded.cwd())
}

fn run_shell(
    options: &CliOptions,
    shell: Option<&str>,
    login: bool,
    args: Vec<OsString>,
) -> Result<i32, CliError> {
    let loaded = config::load(options)?;
    let materialization = materialize(&loaded, shell)?;
    let shell_id = shell.unwrap_or(&loaded.config().shell.default);
    let shell_config = shell_config(&loaded, shell_id)?;
    let adapter = ConfiguredShellAdapter::new(shell_id);
    let invocation = if login {
        ShellInvocation::login(args)
    } else {
        ShellInvocation::interactive(args)
    };
    let line = build_invocation(&adapter, shell_config, &invocation)?;
    process::execute(line, materialization.environment(), loaded.cwd())
}

fn run_shim(
    options: &CliOptions,
    shell: &str,
    real: &Path,
    args: Vec<OsString>,
) -> Result<i32, CliError> {
    let loaded = config::load(options)?;
    let materialization = materialize(&loaded, Some(shell))?;
    // The configured shell command is the real executable that the shim must
    // launch, so it is expected to equal `real` for the image-level bash
    // compatibility shim.  Passing it as the recursion guard would reject
    // every valid invocation.  The shim path itself is owned by the image
    // layout and is not the configured real shell command.
    let line = Shim::new(real).build(&args)?;
    process::execute(line, materialization.environment(), loaded.cwd())
}

fn materialize(loaded: &LoadedConfig, shell: Option<&str>) -> Result<Materialization, CliError> {
    let context = config::runtime_context(loaded, shell)?;
    Materializer::try_new(loaded.config().clone())?
        .materialize(&context)
        .map_err(CliError::Core)
}

fn shell_config<'a>(loaded: &'a LoadedConfig, shell: &str) -> Result<&'a ShellConfig, CliError> {
    loaded.config().shells.get(shell).ok_or_else(|| {
        CliError::Core(CoreError::Context(ContextError::UnknownShell {
            shell: shell.to_owned(),
        }))
    })
}
