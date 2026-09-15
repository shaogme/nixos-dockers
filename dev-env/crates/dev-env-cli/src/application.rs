use crate::args::{Cli, CliCommand, CliOptions};
use crate::config::LoadedConfig;
use crate::error::{BootstrapError, BootstrapPathError, CliError};
use crate::{config, doctor, explain, output, process, trust};
use dev_env_core::{ContextError, CoreError, Materialization, Materializer};
use dev_env_model::ShellConfig;
use dev_env_shell::{build_invocation, CommandLine, ConfiguredShellAdapter, ShellInvocation, Shim};
use std::ffi::OsString;
use std::fs;
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
    if config::effective_user_id() == 0 {
        if let Some(paths) = BootstrapPaths::from_environment()? {
            let line = paths.build_command(&args)?;
            return process::execute_inherited(line);
        }
    }

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

const CONTAINER_INIT_ENV: &str = "DEVENV_CONTAINER_INIT";
const BOOTSTRAP_REAL_SHELL_ENV: &str = "DEVENV_BOOTSTRAP_REAL_SHELL";

struct BootstrapPaths {
    container_init: std::path::PathBuf,
    real_shell: std::path::PathBuf,
}

impl BootstrapPaths {
    fn from_environment() -> Result<Option<Self>, CliError> {
        let container_init = std::env::var_os(CONTAINER_INIT_ENV);
        let real_shell = std::env::var_os(BOOTSTRAP_REAL_SHELL_ENV);
        if container_init.is_none() && real_shell.is_none() {
            return Ok(None);
        }

        let container_init = container_init
            .ok_or(BootstrapError::MissingVariable {
                variable: CONTAINER_INIT_ENV,
            })
            .map(std::path::PathBuf::from)?;
        let real_shell = real_shell
            .ok_or(BootstrapError::MissingVariable {
                variable: BOOTSTRAP_REAL_SHELL_ENV,
            })
            .map(std::path::PathBuf::from)?;

        validate_bootstrap_executable(CONTAINER_INIT_ENV, &container_init)?;
        validate_bootstrap_executable(BOOTSTRAP_REAL_SHELL_ENV, &real_shell)?;
        if real_shell == Path::new("/bin/bash") || real_shell == Path::new("/usr/bin/bash") {
            return Err(BootstrapError::RecursiveShell { path: real_shell }.into());
        }

        Ok(Some(Self {
            container_init,
            real_shell,
        }))
    }

    fn build_command(&self, args: &[OsString]) -> Result<CommandLine, CliError> {
        let mut bootstrap_args = Vec::with_capacity(args.len() + 3);
        bootstrap_args.push(OsString::from("run"));
        bootstrap_args.push(OsString::from("--"));
        bootstrap_args.push(self.real_shell.clone().into_os_string());
        bootstrap_args.extend(args.iter().cloned());
        CommandLine::try_new(self.container_init.clone(), bootstrap_args)
            .map_err(CliError::CommandLine)
    }
}

fn validate_bootstrap_executable(variable: &'static str, path: &Path) -> Result<(), CliError> {
    let reason = if path.as_os_str().is_empty() {
        Some(BootstrapPathError::Empty)
    } else if contains_nul(path) {
        Some(BootstrapPathError::Nul)
    } else if !path.is_absolute() {
        Some(BootstrapPathError::Relative)
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(BootstrapError::InvalidPath {
            variable,
            path: path.to_path_buf(),
            reason,
        }
        .into());
    }

    let metadata = fs::metadata(path).map_err(|source| BootstrapError::NotExecutable {
        variable,
        path: path.to_path_buf(),
        source,
    })?;
    let executable = metadata.is_file() && {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    };
    if !executable {
        return Err(BootstrapError::NotExecutable {
            variable,
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "path is not a regular executable file",
            ),
        }
        .into());
    }
    Ok(())
}

fn contains_nul(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().contains(&0)
    }
    #[cfg(not(unix))]
    {
        path.as_os_str().to_string_lossy().contains('\0')
    }
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
