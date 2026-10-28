use {
    crate::{
        args::{CliOptions, OutputFormat},
        error::CliError,
        ConfigApi, DoctorApi, ExplainApi, LoadedConfig, OutputApi, TrustApi,
    },
    dev_env_core::{Materialization, Materializer},
    dev_env_model::MaterializedEnv,
    dev_env_shell::{build_invocation, CommandLine, ConfiguredShellAdapter, ShellInvocation, Shim},
    std::{ffi::OsString, path::Path, process::Command},
};

pub struct ProcessApi;

impl ProcessApi {
    pub fn execute(
        line: CommandLine,
        environment: &MaterializedEnv,
        cwd: &Path,
    ) -> Result<i32, CliError> {
        let (program, args) = line.clone().into_parts();
        let mut command = line
            .command_with_environment(environment)
            .map_err(CliError::CommandLine)?;
        command.current_dir(cwd);

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let source = command.exec();
            Err(CliError::Launch {
                program,
                args,
                source,
            })
        }

        #[cfg(not(unix))]
        {
            let status = command.status().map_err(|source| CliError::Launch {
                program,
                args,
                source,
            })?;
            Ok(status.code().unwrap_or(1))
        }
    }

    /// Execute a prepared argv while inheriting the launcher's environment.
    ///
    /// This is intentionally separate from [`execute`]: the bootstrap handoff
    /// must receive Docker's `HOST_UID`, `HOST_GID`, `CONTAINER_HOME`, and
    /// `RUN_AS_ROOT` values before `container-init` materializes the final
    /// environment.  In particular, this helper must never call `env_clear()`.
    pub fn execute_inherited(line: CommandLine) -> Result<i32, CliError> {
        let (program, args) = line.clone().into_parts();
        line.validate().map_err(CliError::CommandLine)?;
        let mut command = Command::new(&program);
        command.args(&args);

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let source = command.exec();
            Err(CliError::Launch {
                program,
                args,
                source,
            })
        }

        #[cfg(not(unix))]
        {
            let status = command.status().map_err(|source| CliError::Launch {
                program,
                args,
                source,
            })?;
            Ok(status.code().unwrap_or(1))
        }
    }
}

pub struct OfflineApi;

impl OfflineApi {
    pub fn print(
        options: &CliOptions,
        format: OutputFormat,
        shell: Option<&str>,
    ) -> Result<i32, CliError> {
        let loaded = ConfigApi::load(options)?;
        let materialization = materialize(&loaded, shell)?;
        OutputApi::print_environment(&materialization, format, options.show_secrets)?;
        Ok(0)
    }

    pub fn explain(options: &CliOptions, path: Option<&str>, json: bool) -> Result<i32, CliError> {
        let loaded = ConfigApi::load(options)?;
        let report = ExplainApi::build_report(&loaded, path)?;
        ExplainApi::print_report(&report, json)?;
        Ok(0)
    }

    pub fn doctor(options: &CliOptions, json: bool) -> Result<i32, CliError> {
        let loaded = ConfigApi::load(options)?;
        let report = DoctorApi::inspect(&loaded);
        OutputApi::print_doctor(&report, json)?;
        if report.ok() {
            Ok(0)
        } else {
            Err(CliError::DoctorFailed {
                failed_checks: report.failed_checks().count(),
            })
        }
    }

    pub fn trust(target: &Path) -> Result<i32, CliError> {
        let hash = TrustApi::trust(target)?;
        OutputApi::write_stdout(&format!("trusted: {hash}\n"))?;
        Ok(0)
    }

    pub fn exec(options: &CliOptions, command: Vec<OsString>) -> Result<i32, CliError> {
        let program = command.first().cloned().ok_or(CliError::Backend(
            "offline exec requires a command".to_owned(),
        ))?;
        let line = CommandLine::try_new(program, command.into_iter().skip(1).collect::<Vec<_>>())
            .map_err(CliError::CommandLine)?;
        let loaded = ConfigApi::load(options)?;
        let materialization = materialize(&loaded, None)?;
        ProcessApi::execute(line, materialization.environment(), loaded.cwd())
    }

    pub fn shell(
        options: &CliOptions,
        shell: Option<&str>,
        login: bool,
        args: Vec<OsString>,
    ) -> Result<i32, CliError> {
        let loaded = ConfigApi::load(options)?;
        let shell_id = shell
            .map(str::to_owned)
            .unwrap_or_else(|| loaded.config().shell().default().to_owned());
        let materialization = materialize(&loaded, Some(&shell_id))?;
        let shell_config = loaded
            .config()
            .shells()
            .get(&shell_id)
            .ok_or_else(|| CliError::Backend(format!("unknown shell {shell_id}")))?;
        let adapter = ConfiguredShellAdapter::new(shell_id);
        let invocation = if login {
            ShellInvocation::login(args)
        } else {
            ShellInvocation::interactive(args)
        };
        let line = build_invocation(&adapter, shell_config, &invocation)?;
        ProcessApi::execute(line, materialization.environment(), loaded.cwd())
    }

    pub fn shim(
        options: &CliOptions,
        shell: &str,
        real: &Path,
        args: Vec<OsString>,
    ) -> Result<i32, CliError> {
        let loaded = ConfigApi::load(options)?;
        let materialization = materialize(&loaded, Some(shell))?;
        let line = Shim::new(real).build(&args)?;
        ProcessApi::execute(line, materialization.environment(), loaded.cwd())
    }
}

fn materialize(loaded: &LoadedConfig, shell: Option<&str>) -> Result<Materialization, CliError> {
    let context = ConfigApi::runtime_context(loaded, shell)?;
    Materializer::try_new(loaded.config().clone())?
        .materialize(&context)
        .map_err(CliError::Core)
}
