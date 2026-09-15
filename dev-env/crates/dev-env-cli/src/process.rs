use crate::error::CliError;
use dev_env_model::MaterializedEnv;
use dev_env_shell::CommandLine;
use std::path::Path;

/// Execute a prepared argv with exactly the materialized environment.
///
/// Unix uses `exec` so signals and the child's exit status retain the normal
/// process semantics of a direct command.  Other platforms use the closest
/// equivalent child wait path while keeping the same environment boundary.
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
    let mut command = std::process::Command::new(&program);
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
