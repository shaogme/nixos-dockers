use crate::{
    Cli, CliCommand, CliError, CliOptions, ConfigLoader, DoctorInspector, LoadedConfig, Output,
    ParseError, PreflightChild,
};
use container_init_backend::{
    BackendClaim, BackendClient, BackendClientError, BackendLease, BackendPaths, BackendRunError,
    BackendStartOptions, PrepareOptions,
};
use container_init_bootstrap_model::ActionKind;
use container_init_core::{ExecutionOptions, PosixSystem, SshCapability};
use container_init_protocol::{
    AbstractEndpoint, ProtocolError, DEFAULT_CONTAINER_INIT_BACKEND_ENDPOINT, FRAME_TIMEOUT_MS,
};
use serde_json::to_value;
use std::{
    collections::BTreeMap, env, ffi::OsString, io, os::unix::process::ExitStatusExt, path::PathBuf,
    time::Duration,
};

const DEFAULT_BACKEND_TIMEOUT: Duration = Duration::from_millis(FRAME_TIMEOUT_MS);

pub(crate) struct Application;

impl Application {
    pub(crate) fn run<I>(arguments: I) -> Result<i32, CliError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let cli = Cli::parse(arguments).map_err(parse_error)?;
        let (options, command) = cli.into_parts();
        if matches!(&command, CliCommand::Run { .. } | CliCommand::Exec { .. }) {
            let endpoint = backend_endpoint(&options)?;
            let peer_uid = backend_peer_uid(&options)?;
            env::set_var("CONTAINER_INIT_BACKEND_ENDPOINT", endpoint.name());
            env::set_var("CONTAINER_INIT_BACKEND_PEER_UID", peer_uid.to_string());
        }
        match command {
            CliCommand::Help => {
                println!("{}", Cli::usage(""));
                Ok(0)
            }
            CliCommand::Version => {
                println!("container-init {}", env!("CARGO_PKG_VERSION"));
                Ok(0)
            }
            CliCommand::Plan { json } => {
                if !has_local_config_options(&options) {
                    let client = backend_client(&options)?;
                    if let Some(value) = optional_backend(client.plan())? {
                        Output::print_backend(&value, json, "plan")?;
                        return Ok(0);
                    }
                }
                let loaded = ConfigLoader::load(&options)?;
                Output::print_plan(&loaded, json)?;
                Ok(0)
            }
            CliCommand::Doctor { json } => {
                if !has_local_config_options(&options) {
                    let client = backend_client(&options)?;
                    if let Some(value) = optional_backend(client.doctor())? {
                        Output::print_backend(&value, json, "doctor")?;
                        return Ok(0);
                    }
                }
                let loaded = ConfigLoader::load(&options)?;
                let context = ConfigLoader::runtime_context(&options, false)?;
                let report = DoctorInspector::inspect(&loaded, &context);
                Output::print_doctor(&report, json)?;
                if !report.ok() {
                    let (check, source) = report
                        .into_failed_check()
                        .expect("a failed doctor report must contain a failed check");
                    return Err(CliError::DoctorFailed { check, source });
                }
                Ok(0)
            }
            CliCommand::Status { json } => {
                let status = backend_client(&options)?
                    .status()
                    .map_err(backend_client_error)?;
                let value = to_value(status).map_err(CliError::Output)?;
                Output::print_backend(&value, json, "status")?;
                Ok(0)
            }
            CliCommand::Run { command } => {
                let paths = backend_paths(&options)?;
                let lease = match BackendLease::claim(paths, request_timeout(&options)?)
                    .map_err(|error| CliError::Backend(error.to_string()))?
                {
                    BackendClaim::Acquired(lease) => lease,
                    BackendClaim::AlreadyRunning(status) => {
                        println!("backend already running");
                        let value = to_value(status).map_err(CliError::Output)?;
                        Output::print_backend(&value, false, "status")?;
                        return Ok(0);
                    }
                };

                let loaded = ConfigLoader::load(&options)?;
                let context = ConfigLoader::runtime_context(&options, true)?;
                let execution_options = build_execution_options(&options, &loaded);
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
                let cwd = env::current_dir().map_err(|source| {
                    CliError::io("read current working directory", None, source)
                })?;
                let inputs = options.inputs.iter().cloned().collect::<BTreeMap<_, _>>();
                if inputs.len() != options.inputs.len() {
                    return Err(CliError::Configuration(
                        "runtime input names may only be specified once".to_owned(),
                    ));
                }
                let ambient = env::vars().collect::<BTreeMap<_, _>>();
                let client = backend_client(&options)?;
                let deadline = client.deadline().map_err(backend_client_error)?;
                let prepared = client
                    .prepare_until(PrepareOptions {
                        argv: command,
                        cwd: cwd.clone(),
                        inputs,
                        ambient_environment: &ambient,
                        deadline,
                    })
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
}

fn backend_client(options: &CliOptions) -> Result<BackendClient, CliError> {
    Ok(BackendClient::new(
        backend_endpoint(options)?,
        backend_peer_uid(options)?,
        request_timeout(options)?,
    ))
}

fn backend_paths(options: &CliOptions) -> Result<BackendPaths, CliError> {
    Ok(BackendPaths {
        endpoint: backend_endpoint(options)?,
        owner_uid: PosixSystem::new().current_ids().0,
        peer_uid: backend_peer_uid(options)?,
    })
}

fn backend_endpoint(options: &CliOptions) -> Result<AbstractEndpoint, CliError> {
    let name = match &options.backend_endpoint {
        Some(name) => name.clone(),
        None => match env::var("CONTAINER_INIT_BACKEND_ENDPOINT") {
            Ok(name) => name,
            Err(env::VarError::NotPresent) => DEFAULT_CONTAINER_INIT_BACKEND_ENDPOINT.to_owned(),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(CliError::Configuration(
                    "CONTAINER_INIT_BACKEND_ENDPOINT must be valid UTF-8".to_owned(),
                ));
            }
        },
    };
    AbstractEndpoint::new(name).map_err(|error| CliError::Configuration(error.to_string()))
}

fn backend_peer_uid(options: &CliOptions) -> Result<u32, CliError> {
    if let Some(uid) = options.backend_peer_uid {
        return Ok(uid);
    }
    match env::var("CONTAINER_INIT_BACKEND_PEER_UID") {
        Ok(value) => value.parse::<u32>().map_err(|_| {
            CliError::Configuration(
                "CONTAINER_INIT_BACKEND_PEER_UID must be an unsigned integer".to_owned(),
            )
        }),
        Err(env::VarError::NotPresent) => Ok(PosixSystem::new().current_ids().0),
        Err(env::VarError::NotUnicode(_)) => Err(CliError::Configuration(
            "CONTAINER_INIT_BACKEND_PEER_UID must be valid UTF-8".to_owned(),
        )),
    }
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

fn build_execution_options(options: &CliOptions, loaded: &LoadedConfig) -> ExecutionOptions {
    let mut execution_options = ExecutionOptions::default();
    if loaded
        .plan()
        .actions()
        .iter()
        .any(|action| action.kind == ActionKind::ServiceSshPrepare)
    {
        let keygen = loaded
            .config()
            .actions()
            .iter()
            .find(|action| action.kind() == ActionKind::ServiceSshPrepare)
            .and_then(|action| action.ssh_keygen().clone())
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
