use crate::{
    args::{Cli, CliCommand, CliOptions, ParseError},
    config, doctor,
    error::CliError,
    output,
    preflight::PreflightChild,
};
use container_init_backend::{
    BackendClaim, BackendClient, BackendClientError, BackendLease, BackendPaths, BackendRunError,
    BackendStartOptions,
};
use container_init_bootstrap_model::ActionKind;
use container_init_core::{ExecutionOptions, PosixSystem, SshCapability};
use container_init_protocol::ProtocolError;
use serde_json::to_value;
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    io,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    time::Duration,
};

const DEFAULT_BACKEND_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run<I>(arguments: I) -> Result<i32, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let cli = Cli::parse(arguments).map_err(parse_error)?;
    match cli.command {
        CliCommand::Help => {
            println!("{}", crate::args::usage(""));
            Ok(0)
        }
        CliCommand::Version => {
            println!("container-init {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        CliCommand::Plan { json } => {
            if !has_local_config_options(&cli.options) {
                let client = backend_client(&cli.options)?;
                if let Some(value) = optional_backend(client.plan())? {
                    output::print_backend(&value, json, "plan")?;
                    return Ok(0);
                }
            }
            let loaded = config::load(&cli.options)?;
            output::print_plan(&loaded, json)?;
            Ok(0)
        }
        CliCommand::Doctor { json } => {
            if !has_local_config_options(&cli.options) {
                let client = backend_client(&cli.options)?;
                if let Some(value) = optional_backend(client.doctor())? {
                    output::print_backend(&value, json, "doctor")?;
                    return Ok(0);
                }
            }
            let loaded = config::load(&cli.options)?;
            let context = config::runtime_context(&cli.options, false)?;
            let report = doctor::inspect(&loaded, &context);
            output::print_doctor(&report, json)?;
            if !report.ok {
                let check = report
                    .checks
                    .into_iter()
                    .find(|check| check.status == doctor::DoctorStatus::Fail)
                    .expect("a failed doctor report must contain a failed check");
                return Err(CliError::DoctorFailed {
                    check: check.name,
                    source: check.error.map(Box::new),
                });
            }
            Ok(0)
        }
        CliCommand::Status { json } => {
            let status = backend_client(&cli.options)?
                .status()
                .map_err(backend_client_error)?;
            let value = to_value(status).map_err(CliError::Output)?;
            output::print_backend(&value, json, "status")?;
            Ok(0)
        }
        CliCommand::Run { command } => {
            let workspace = config::workspace_path(&cli.options)?;
            let paths = backend_paths(&cli.options, &workspace)?;
            let lease = match BackendLease::claim(paths, request_timeout(&cli.options)?)
                .map_err(|error| CliError::Backend(error.to_string()))?
            {
                BackendClaim::Acquired(lease) => lease,
                BackendClaim::AlreadyRunning(status) => {
                    println!("backend already running");
                    let value = to_value(status).map_err(CliError::Output)?;
                    output::print_backend(&value, false, "status")?;
                    return Ok(0);
                }
            };

            let loaded = config::load(&cli.options)?;
            let context = config::runtime_context(&cli.options, true)?;
            let execution_options = build_execution_options(&cli.options, &loaded);
            lease
                .start(BackendStartOptions {
                    profile: loaded.profile().to_owned(),
                    config: loaded.config().clone(),
                    plan: loaded.plan().clone(),
                    startup_context: context,
                    command,
                    execution_options,
                })
                .map_err(backend_run_error)
        }
        CliCommand::Exec { command } => {
            let cwd = env::current_dir()
                .map_err(|source| CliError::io("read current working directory", None, source))?;
            let inputs = cli
                .options
                .inputs
                .iter()
                .cloned()
                .collect::<BTreeMap<_, _>>();
            if inputs.len() != cli.options.inputs.len() {
                return Err(CliError::Configuration(
                    "runtime input names may only be specified once".to_owned(),
                ));
            }
            let ambient = env::vars().collect::<BTreeMap<_, _>>();
            let client = backend_client(&cli.options)?;
            let deadline = client.deadline().map_err(backend_client_error)?;
            let prepared = client
                .prepare_until(command, cwd.clone(), inputs, &ambient, deadline)
                .map_err(backend_client_error)?;
            let mut child = match PreflightChild::spawn(&prepared, cwd) {
                Ok(child) => child,
                Err(error) => {
                    let _ = client.abort_until(&prepared.prepare_id, deadline);
                    return Err(CliError::Backend(format!(
                        "handoff preflight could not start: {error}"
                    )));
                }
            };
            if let Err(error) = child.wait_ready(deadline) {
                child.abort();
                let _ = client.abort_until(&prepared.prepare_id, deadline);
                return Err(CliError::Backend(error.to_string()));
            }
            if let Err(error) = client.commit_until(&prepared.prepare_id, deadline) {
                child.abort();
                let _ = client.abort_until(&prepared.prepare_id, deadline);
                return Err(backend_client_error(error));
            }
            if let Err(error) = child.release() {
                child.abort();
                return Err(CliError::Backend(format!(
                    "request reconcile committed but handoff release failed: {error}"
                )));
            }
            if let Err(error) = child.wait_for_exec(deadline) {
                child.abort();
                return Err(CliError::Backend(error.to_string()));
            }
            let status = child.wait_for_runtime().map_err(|error| {
                CliError::Backend(format!("handoff process wait failed: {error}"))
            })?;
            Ok(status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
        }
    }
}

fn backend_client(options: &CliOptions) -> Result<BackendClient, CliError> {
    let workspace = config::workspace_path(options)?;
    Ok(BackendClient::new(
        backend_socket(options, &workspace),
        request_timeout(options)?,
    ))
}

fn backend_paths(options: &CliOptions, workspace: &Path) -> Result<BackendPaths, CliError> {
    let socket_path = backend_socket(options, workspace);
    let parent = socket_path
        .parent()
        .ok_or_else(|| CliError::Configuration("backend socket path has no parent".to_owned()))?
        .to_path_buf();
    Ok(BackendPaths {
        socket_path,
        lock_path: parent.join("backend.lock"),
        owner_uid: PosixSystem::new().current_ids().0,
    })
}

fn backend_socket(options: &CliOptions, workspace: &Path) -> PathBuf {
    if let Some(path) = &options.backend_socket {
        return path.clone();
    }
    if let Some(path) = env::var_os("CONTAINER_INIT_BACKEND_SOCKET") {
        return PathBuf::from(path);
    }
    let runtime_dir = env::var_os("CONTAINER_INIT_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let uid = PosixSystem::new().current_ids().0;
            if uid == 0 {
                PathBuf::from("/run/container-init")
            } else if let Some(runtime) = env::var_os("XDG_RUNTIME_DIR") {
                PathBuf::from(runtime).join("container-init")
            } else {
                workspace.join(".container-init")
            }
        });
    runtime_dir.join("backend.sock")
}

fn request_timeout(options: &CliOptions) -> Result<Duration, CliError> {
    if let Some(timeout) = options.request_timeout {
        return Ok(timeout);
    }
    if let Some(milliseconds) = env::var_os("CONTAINER_INIT_BACKEND_TIMEOUT_MS") {
        let milliseconds = milliseconds.to_string_lossy().parse::<u64>().map_err(|_| {
            CliError::Configuration(
                "CONTAINER_INIT_BACKEND_TIMEOUT_MS must be an integer".to_owned(),
            )
        })?;
        return Ok(Duration::from_millis(milliseconds));
    }
    Ok(DEFAULT_BACKEND_TIMEOUT)
}

fn has_local_config_options(options: &CliOptions) -> bool {
    options.profile.is_some()
        || options.profiles_dir.is_some()
        || options.admin_profiles_dir.is_some()
        || options.default_profile_file.is_some()
        || options.workspace.is_some()
        || options.receipt_path.is_some()
        || !options.inputs.is_empty()
}

fn optional_backend<T>(result: Result<T, BackendClientError>) -> Result<Option<T>, CliError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if backend_unavailable(&error) => Ok(None),
        Err(error) => Err(backend_client_error(error)),
    }
}

fn backend_unavailable(error: &BackendClientError) -> bool {
    let io_error = match error {
        BackendClientError::Io(error) => Some(error),
        BackendClientError::Protocol(ProtocolError::Io(error)) => Some(error),
        _ => None,
    };
    io_error.is_some_and(|error| {
        matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        )
    })
}

fn backend_client_error(error: BackendClientError) -> CliError {
    CliError::Backend(error.to_string())
}

fn backend_run_error(error: BackendRunError) -> CliError {
    match error {
        BackendRunError::Core(error) => CliError::Core(error),
        BackendRunError::Io(error) => CliError::Backend(error.to_string()),
    }
}

fn build_execution_options(
    options: &CliOptions,
    loaded: &crate::config::LoadedConfig,
) -> ExecutionOptions {
    let mut execution_options = ExecutionOptions::default();
    if loaded
        .plan()
        .actions()
        .iter()
        .any(|action| action.kind == ActionKind::ServiceSshPrepare)
    {
        let keygen = loaded
            .config()
            .actions
            .iter()
            .find(|action| action.kind == ActionKind::ServiceSshPrepare)
            .and_then(|action| action.ssh_keygen.clone())
            .unwrap_or_else(|| SshCapability::default().keygen().display().to_string());
        execution_options = execution_options.with_ssh(SshCapability::new(keygen));
    }
    if let Some(path) = options
        .receipt_path
        .clone()
        .or_else(|| env::var_os("CONTAINER_INIT_RECEIPT_PATH").map(PathBuf::from))
    {
        execution_options = execution_options.with_receipt_path(path);
    }
    execution_options
}

fn parse_error(error: ParseError) -> CliError {
    match error {
        ParseError::Invalid(message) => CliError::Arguments(message),
    }
}
