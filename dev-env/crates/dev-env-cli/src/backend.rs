//! The long-lived dev-env backend process.
//!
//! The backend owns configuration loading and the provider execution boundary.
//! Clients only exchange the versioned model protocol over a Unix socket.

use crate::args::{BackendCommand, BackendInitial, CliOptions};
use crate::config::{self, LoadedConfig};
use crate::error::CliError;
use crate::identity_broker;
use dev_env_core::{
    MaterializationDiagnostic, MaterializationMetadata, MaterializationService, Materializer,
    RuntimeContext,
};
use dev_env_model::{
    read_backend_request, read_backend_response, write_backend_request, write_backend_response,
    BackendError, BackendRequest, BackendRequestMessage, BackendResponse, BackendResponseMessage,
    BackendState, BackendStatus, ConfigSnapshot, Generation, HelloInfo, IdentityRequest,
    PreparedResponse, ProviderDiagnosticSummary, ReceiptSummary,
    ShellInvocation as BackendShellInvocation, TrustTarget,
};
use dev_env_shell::{build_invocation, CommandLine, ConfiguredShellAdapter, ShellInvocation};
use fs2::FileExt;
use std::collections::BTreeMap;
use std::env;
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

pub const DEFAULT_BACKEND_SOCKET: &str = "/run/dev-env/backend.sock";
const MAX_ACTIVE_REQUESTS: usize = 16;

pub fn dispatch(command: BackendCommand, options: CliOptions) -> Result<i32, CliError> {
    match command {
        BackendCommand::Run { socket, initial } => run(socket, initial, options),
        BackendCommand::Status { socket, json: _ } => {
            let response = request(socket, BackendRequest::Status)?;
            print_response(response)?;
            Ok(0)
        }
        BackendCommand::Reload { socket, wait } => {
            let response = request(socket, BackendRequest::Reload { wait })?;
            print_response(response)?;
            Ok(0)
        }
        BackendCommand::Stop { socket } => {
            let response = request(socket, BackendRequest::Stop)?;
            print_response(response)?;
            Ok(0)
        }
    }
}

fn run(
    socket: Option<PathBuf>,
    initial: BackendInitial,
    options: CliOptions,
) -> Result<i32, CliError> {
    let socket = socket_path(socket)?;
    let parent = socket.parent().unwrap_or_else(|| Path::new("/"));
    fs::create_dir_all(parent).map_err(|source| {
        CliError::Backend(format!(
            "could not create backend runtime directory {}: {source}",
            parent.display()
        ))
    })?;
    let socket_gid = backend_socket_gid()?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o770)).map_err(|source| {
        CliError::Backend(format!(
            "could not set backend runtime directory permissions: {source}"
        ))
    })?;
    set_owner(parent, unsafe { libc::geteuid() }, socket_gid)?;
    let lock_path = parent.join("backend.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|source| {
            CliError::Backend(format!("could not open {}: {source}", lock_path.display()))
        })?;
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o660)).map_err(|source| {
        CliError::Backend(format!("could not set backend lock permissions: {source}"))
    })?;
    set_owner(&lock_path, unsafe { libc::geteuid() }, socket_gid)?;
    if let Err(source) = lock.try_lock_exclusive() {
        if let Ok(response) = request(Some(socket.clone()), BackendRequest::Status) {
            print_response(response)?;
            return Ok(0);
        }
        return Err(CliError::Backend(format!(
            "backend already running for {}: {source}",
            socket.display()
        )));
    }

    // The lock is held before reading any profile.  A second invocation never
    // reloads configuration or races the active backend; a stale lock is
    // released by the operating system when the previous owner exits.
    let loaded = config::load(&options)?;
    let snapshot = make_snapshot(&loaded, Generation::INITIAL)?;

    if let Ok(metadata) = fs::symlink_metadata(&socket) {
        if !metadata.file_type().is_socket() {
            return Err(CliError::Backend(format!(
                "refusing to replace non-socket backend path {}",
                socket.display()
            )));
        }
        fs::remove_file(&socket).map_err(|source| {
            CliError::Backend(format!("could not remove stale backend socket: {source}"))
        })?;
    }
    let listener = UnixListener::bind(&socket).map_err(|source| {
        CliError::Backend(format!(
            "could not bind backend socket {}: {source}",
            socket.display()
        ))
    })?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o660)).map_err(|source| {
        CliError::Backend(format!(
            "could not set backend socket permissions: {source}"
        ))
    })?;
    set_owner(&socket, unsafe { libc::geteuid() }, socket_gid)?;
    listener.set_nonblocking(true).map_err(|source| {
        CliError::Backend(format!("could not configure backend socket: {source}"))
    })?;

    let runtime = Arc::new(BackendRuntime {
        snapshot: RwLock::new(Arc::new(snapshot)),
        options,
        materialization: RwLock::new(Arc::new(MaterializationService::new(
            Materializer::try_new(loaded.config().clone()).map_err(CliError::Core)?,
        ))),
        state: Mutex::new(BackendState::Ready),
        active_requests: AtomicUsize::new(0),
        stopping: AtomicBool::new(false),
        socket: socket.clone(),
        initial_pid: Mutex::new(None),
        initial_exit_code: Mutex::new(None),
        failure: Mutex::new(None),
    });
    install_signal_handlers();
    let runtime_for_initial = Arc::clone(&runtime);
    thread::spawn(move || run_initial(runtime_for_initial, initial));

    while !runtime.stopping.load(Ordering::Acquire) && !signal_requested() {
        match listener.accept() {
            Ok((stream, _)) => {
                let runtime = Arc::clone(&runtime);
                thread::spawn(move || serve_connection(runtime, stream));
            }
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                runtime.fail(format!("backend listener failed: {source}"));
                break;
            }
        }
    }
    runtime.stopping.store(true, Ordering::Release);
    *runtime.state.lock().expect("backend state lock poisoned") = BackendState::Stopping;
    runtime.stop_initial_runtime();
    let _ = fs::remove_file(&runtime.socket);
    if let Some(failure) = runtime
        .failure
        .lock()
        .expect("backend failure lock poisoned")
        .take()
    {
        return Err(CliError::Backend(failure));
    }
    let exit_code = runtime
        .initial_exit_code
        .lock()
        .expect("initial exit code lock poisoned")
        .take()
        .unwrap_or(0);
    Ok(exit_code)
}

struct BackendRuntime {
    snapshot: RwLock<Arc<ConfigSnapshot>>,
    options: CliOptions,
    materialization: RwLock<Arc<MaterializationService>>,
    state: Mutex<BackendState>,
    active_requests: AtomicUsize,
    stopping: AtomicBool,
    socket: PathBuf,
    initial_pid: Mutex<Option<u32>>,
    initial_exit_code: Mutex<Option<i32>>,
    failure: Mutex<Option<String>>,
}

impl BackendRuntime {
    fn current_snapshot(&self) -> Arc<ConfigSnapshot> {
        Arc::clone(
            &self
                .snapshot
                .read()
                .expect("backend snapshot lock poisoned"),
        )
    }

    fn fail(&self, message: String) {
        *self.state.lock().expect("backend state lock poisoned") = BackendState::Failed;
        let mut failure = self.failure.lock().expect("backend failure lock poisoned");
        if failure.is_none() {
            *failure = Some(message);
        }
        self.stopping.store(true, Ordering::Release);
    }

    fn record_initial_exit(&self, status: std::process::ExitStatus) {
        *self
            .initial_exit_code
            .lock()
            .expect("initial exit code lock poisoned") = Some(exit_status_code(status));
    }

    fn stop_initial_runtime(&self) {
        let Some(pid) = self
            .initial_pid
            .lock()
            .expect("initial runtime lock poisoned")
            .take()
        else {
            return;
        };
        #[cfg(unix)]
        unsafe {
            let group = -(pid as libc::pid_t);
            let _ = libc::kill(group, libc::SIGTERM);
            thread::sleep(Duration::from_millis(50));
            let _ = libc::kill(group, libc::SIGKILL);
        }
    }

    fn error(
        &self,
        request_id: &str,
        class: &str,
        message: impl Into<String>,
        retryable: bool,
    ) -> BackendResponseMessage {
        let snapshot = self.current_snapshot();
        BackendResponseMessage::new(
            request_id,
            BackendResponse::Error(BackendError {
                error_version: dev_env_model::DEVENV_BACKEND_ERROR_VERSION,
                class: class.to_owned(),
                retryable,
                message: message.into(),
                generation: Some(snapshot.generation),
                provider_id: None,
                operation: None,
                request_id: Some(request_id.to_owned()),
            }),
        )
    }
}

fn serve_connection(runtime: Arc<BackendRuntime>, mut stream: UnixStream) {
    let request = match read_backend_request(&mut stream) {
        Ok(request) => request,
        Err(_) => return,
    };
    if runtime.active_requests.fetch_add(1, Ordering::AcqRel) >= MAX_ACTIVE_REQUESTS {
        runtime.active_requests.fetch_sub(1, Ordering::AcqRel);
        let response = runtime.error(
            &request.request_id,
            "busy",
            "backend request limit reached",
            true,
        );
        let _ = write_backend_response(&mut stream, &response);
        return;
    }
    let peer = peer_credentials(&stream);
    if !authorized_peer(peer) {
        return;
    }
    let response = handle_request(&runtime, &request, peer);
    runtime.active_requests.fetch_sub(1, Ordering::AcqRel);
    let _ = write_backend_response(&mut stream, &response);
}

fn authorized_peer(peer: dev_env_model::IdentityPeer) -> bool {
    if peer.uid == 0 {
        return true;
    }
    let allowed = env::var("CONTAINER_INIT_HANDOFF_UID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or_else(|| unsafe { libc::geteuid() });
    peer.uid == allowed
}

fn handle_request(
    runtime: &Arc<BackendRuntime>,
    request: &BackendRequestMessage,
    peer: dev_env_model::IdentityPeer,
) -> BackendResponseMessage {
    let snapshot = runtime.current_snapshot();
    match &request.request {
        BackendRequest::Hello => BackendResponseMessage::new(
            &request.request_id,
            BackendResponse::Hello(HelloInfo {
                state: *runtime.state.lock().expect("backend state lock poisoned"),
                generation: snapshot.generation,
                config_fingerprint: snapshot.config_fingerprint,
                runtime_inputs: snapshot.runtime_inputs.keys().cloned().collect(),
                environment_names: snapshot
                    .resolved_config
                    .environment
                    .variables
                    .keys()
                    .cloned()
                    .collect(),
            }),
        ),
        BackendRequest::Status => BackendResponseMessage::new(
            &request.request_id,
            BackendResponse::Status(BackendStatus {
                state: *runtime.state.lock().expect("backend state lock poisoned"),
                generation: snapshot.generation,
                config_fingerprint: snapshot.config_fingerprint,
                backend_pid: std::process::id(),
                active_requests: runtime
                    .active_requests
                    .load(Ordering::Acquire)
                    .saturating_sub(1),
            }),
        ),
        BackendRequest::Plan => BackendResponseMessage::new(
            &request.request_id,
            BackendResponse::Plan(plan_value(&snapshot)),
        ),
        BackendRequest::Explain { path } => BackendResponseMessage::new(
            &request.request_id,
            BackendResponse::Explain(explain_value(&snapshot, path.as_deref())),
        ),
        BackendRequest::Doctor => BackendResponseMessage::new(
            &request.request_id,
            BackendResponse::Doctor(doctor_value(&snapshot)),
        ),
        BackendRequest::Prepare { context } => handle_prepare(runtime, request, context, peer),
        BackendRequest::Trust { target } if peer.uid == 0 => handle_trust(runtime, request, target),
        BackendRequest::Trust { .. } => runtime.error(
            &request.request_id,
            "unauthorized",
            "trust updates require a root backend owner",
            false,
        ),
        BackendRequest::Reload { .. } if peer.uid == 0 => handle_reload(runtime, request),
        BackendRequest::Reload { .. } => runtime.error(
            &request.request_id,
            "unauthorized",
            "reload requires a root backend owner",
            false,
        ),
        BackendRequest::Stop if peer.uid == 0 => {
            *runtime.state.lock().expect("backend state lock poisoned") = BackendState::Stopping;
            runtime.stopping.store(true, Ordering::Release);
            BackendResponseMessage::new(&request.request_id, BackendResponse::Stopped)
        }
        BackendRequest::Stop => runtime.error(
            &request.request_id,
            "unauthorized",
            "stop requires a root backend owner",
            false,
        ),
    }
}

fn handle_prepare(
    runtime: &Arc<BackendRuntime>,
    request: &BackendRequestMessage,
    context: &dev_env_model::RequestContext,
    peer: dev_env_model::IdentityPeer,
) -> BackendResponseMessage {
    if *runtime.state.lock().expect("backend state lock poisoned") != BackendState::Ready {
        return runtime.error(
            &request.request_id,
            "backend_busy",
            "backend is not ready for Prepare",
            true,
        );
    }
    let snapshot = runtime.current_snapshot();
    let cwd = normalize_path(&context.cwd);
    if !cwd.starts_with(&snapshot.workspace_root) {
        return runtime.error(
            &request.request_id,
            "invalid_input",
            "request cwd is outside the snapshot workspace",
            false,
        );
    }
    if context
        .typed_runtime_inputs
        .keys()
        .any(|name| !snapshot.runtime_inputs.contains_key(name))
    {
        return runtime.error(
            &request.request_id,
            "invalid_input",
            "unknown runtime input",
            false,
        );
    }
    let requested_identity = requested_identity(context, peer);
    if !matches!(requested_identity, IdentityRequest::Peer) && peer.uid != 0 {
        return runtime.error(
            &request.request_id,
            "unauthorized",
            "non-root clients may only request their peer identity",
            false,
        );
    }
    let shell = if context.shell == "default" {
        snapshot.resolved_config.shell.default.clone()
    } else {
        context.shell.clone()
    };
    let process_environment = if snapshot.resolved_config.environment.inherit_process {
        context.filtered_ambient_environment.clone()
    } else {
        BTreeMap::new()
    };
    let identity = match identity_broker::resolve(
        &requested_identity,
        peer,
        &cwd,
        context.typed_runtime_inputs.clone(),
        process_environment.clone(),
    ) {
        Ok(identity) => identity,
        Err(error) => {
            return runtime.error(
                &request.request_id,
                "identity_broker",
                error.to_string(),
                true,
            )
        }
    };
    let runtime_context = RuntimeContext::new(
        snapshot.workspace_root.clone(),
        cwd.clone(),
        shell.clone(),
        process_environment.clone(),
    )
    .with_user_id(identity.uid)
    .with_workspace_config_present(snapshot.workspace_root.join(".dev-env.toml").is_file());
    let detect_fingerprints = provider_detect_fingerprints(&snapshot, &runtime_context);
    let key = match dev_env_model::MaterializationKey::new(
        snapshot.generation,
        snapshot.config_fingerprint,
        snapshot.workspace_root.clone(),
        cwd.clone(),
        shell.clone(),
        identity.clone(),
        context.typed_runtime_inputs.clone(),
        process_environment.clone(),
        detect_fingerprints,
    ) {
        Ok(key) => key,
        Err(error) => {
            return runtime.error(
                &request.request_id,
                "materialization",
                error.to_string(),
                false,
            )
        }
    };
    let Some(shell_config) = snapshot.shells.get(&shell) else {
        return runtime.error(&request.request_id, "invalid_input", "unknown shell", false);
    };
    let shell_invocation = build_shell_invocation(context, &shell, shell_config);
    let metadata =
        MaterializationMetadata::new(&request.request_id, snapshot.generation, identity.clone());
    let materialization = {
        let service = Arc::clone(
            &runtime
                .materialization
                .read()
                .expect("materialization service lock poisoned"),
        );
        match service.prepare(key, &runtime_context, &metadata) {
            Ok(result) => result,
            Err(error) => {
                return runtime.error(
                    &request.request_id,
                    "materialization",
                    error.to_string(),
                    false,
                )
            }
        }
    };
    let diagnostics = materialization
        .diagnostics()
        .iter()
        .map(|diagnostic| match diagnostic {
            MaterializationDiagnostic::Provider { provider, .. } => ProviderDiagnosticSummary {
                provider: provider.clone(),
                class: "provider".to_owned(),
                operation: None,
                status: None,
                timed_out: false,
                orphaned_children: 0,
            },
            MaterializationDiagnostic::ProviderDisabled { provider } => ProviderDiagnosticSummary {
                provider: provider.clone(),
                class: "disabled".to_owned(),
                operation: None,
                status: None,
                timed_out: false,
                orphaned_children: 0,
            },
        })
        .collect::<Vec<_>>();
    let receipt_summary = ReceiptSummary {
        succeeded: true,
        action_count: materialization.environment().provider_receipts.len(),
        warning_count: diagnostics.len(),
    };
    BackendResponseMessage::new(
        &request.request_id,
        BackendResponse::Prepared(PreparedResponse {
            generation: snapshot.generation,
            config_fingerprint: snapshot.config_fingerprint,
            cwd,
            shell_invocation,
            materialized_environment: materialization.environment().clone(),
            supplemental_groups: identity.supplementary_groups.clone(),
            identity,
            diagnostics,
            cache_hit: materialization.cache_hit,
            receipt_summary,
        }),
    )
}

fn handle_reload(
    runtime: &Arc<BackendRuntime>,
    request: &BackendRequestMessage,
) -> BackendResponseMessage {
    *runtime.state.lock().expect("backend state lock poisoned") = BackendState::Reloading;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while runtime.active_requests.load(Ordering::Acquire) > 1
        && std::time::Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(5));
    }
    if runtime.active_requests.load(Ordering::Acquire) > 1 {
        *runtime.state.lock().expect("backend state lock poisoned") = BackendState::Ready;
        return runtime.error(
            &request.request_id,
            "reload_busy",
            "backend requests did not finish before reload deadline",
            true,
        );
    }
    let current = runtime.current_snapshot();
    let result = config::load(&runtime.options).and_then(|loaded| {
        make_snapshot(
            &loaded,
            current
                .generation
                .next()
                .map_err(|error| crate::error::CliError::Backend(error.to_string()))?,
        )
    });
    match result {
        Ok(next) => {
            let generation = next.generation;
            let fingerprint = next.config_fingerprint;
            let materializer = match Materializer::try_new(next.resolved_config.clone()) {
                Ok(materializer) => MaterializationService::new(materializer),
                Err(error) => {
                    *runtime.state.lock().expect("backend state lock poisoned") =
                        BackendState::Ready;
                    return runtime.error(
                        &request.request_id,
                        "reload_failed",
                        error.to_string(),
                        false,
                    );
                }
            };
            *runtime
                .snapshot
                .write()
                .expect("backend snapshot lock poisoned") = Arc::new(next);
            *runtime
                .materialization
                .write()
                .expect("materialization service lock poisoned") = Arc::new(materializer);
            *runtime.state.lock().expect("backend state lock poisoned") = BackendState::Ready;
            BackendResponseMessage::new(
                &request.request_id,
                BackendResponse::Reloaded {
                    generation,
                    config_fingerprint: fingerprint,
                },
            )
        }
        Err(error) => {
            *runtime.state.lock().expect("backend state lock poisoned") = BackendState::Ready;
            runtime.error(
                &request.request_id,
                "reload_failed",
                error.to_string(),
                false,
            )
        }
    }
}

fn handle_trust(
    runtime: &Arc<BackendRuntime>,
    request: &BackendRequestMessage,
    target: &TrustTarget,
) -> BackendResponseMessage {
    let target = match target {
        TrustTarget::Path { path } => path.clone(),
        TrustTarget::Sha256 { digest } => PathBuf::from(hex(digest)),
    };
    match crate::trust::trust(&target) {
        Ok(_) => BackendResponseMessage::new(
            &request.request_id,
            BackendResponse::Trusted {
                generation: runtime.current_snapshot().generation,
            },
        ),
        Err(error) => runtime.error(&request.request_id, "trust", error.to_string(), false),
    }
}

fn run_initial(runtime: Arc<BackendRuntime>, initial: BackendInitial) {
    let snapshot = runtime.current_snapshot();
    let request_id = "initial-runtime";
    let service_command = env::var_os("DEVENV_BACKEND_SERVICE_COMMAND").map(PathBuf::from);
    let root_service = matches!(
        &initial,
        BackendInitial::Exec(args)
            if args.first().is_some_and(|arg| service_command.as_deref() == Some(Path::new(arg)))
    );
    let initial_user = if root_service {
        "root".to_owned()
    } else {
        env::var("DEVENV_BACKEND_INITIAL_USER").unwrap_or_else(|_| "dev".to_owned())
    };
    let context = dev_env_model::RequestContext::new(
        request_id,
        dev_env_model::RequestMode::Shell,
        snapshot.workspace_root.clone(),
        snapshot.resolved_config.shell.default.clone(),
    )
    .with_identity(IdentityRequest::User { name: initial_user })
    .with_runtime_inputs(
        snapshot
            .runtime_inputs
            .keys()
            .filter_map(|name| env::var(name).ok().map(|value| (name.clone(), value))),
    )
    .with_ambient_environment(config::ambient_environment());
    let response = handle_prepare(
        &runtime,
        &BackendRequestMessage::new(
            request_id,
            BackendRequest::Prepare {
                context: context.clone(),
            },
        ),
        &context,
        peer_credentials_from_current_thread(),
    );
    let prepared = match response.response {
        BackendResponse::Prepared(prepared) => prepared,
        BackendResponse::Error(error) => {
            runtime.fail(format!(
                "initial runtime preparation failed: {}: {}",
                error.class, error.message
            ));
            return;
        }
        other => {
            runtime.fail(format!(
                "initial runtime preparation returned an unexpected response: {other:?}"
            ));
            return;
        }
    };
    let command: Result<CommandLine, ()> = match initial {
        BackendInitial::Shell => {
            let shell = snapshot.resolved_config.shell.default.clone();
            let Some(config) = snapshot.shells.get(&shell) else {
                runtime.fail("initial shell is not configured".to_owned());
                return;
            };
            let adapter = ConfiguredShellAdapter::new(shell);
            build_invocation(&adapter, config, &ShellInvocation::interactive(Vec::new()))
                .map_err(|_| ())
        }
        BackendInitial::Exec(args) => {
            let mut args = args.into_iter();
            let Some(program) = args.next() else {
                runtime.fail("initial command is empty".to_owned());
                return;
            };
            CommandLine::try_new(program, args.collect::<Vec<_>>()).map_err(|_| ())
        }
    };
    let Ok(command) = command else {
        runtime.fail("initial runtime argv is invalid".to_owned());
        return;
    };
    let (program, args) = command.clone().into_parts();
    let Ok(mut command) = command.command_with_environment(&prepared.materialized_environment)
    else {
        runtime.fail("could not build initial runtime".to_owned());
        return;
    };
    command
        .env("HOME", &prepared.identity.home)
        .env("USER", &prepared.identity.user)
        .env("LOGNAME", &prepared.identity.user);
    command.current_dir(&prepared.cwd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let identity = prepared.identity.clone();
        let groups = prepared.supplemental_groups.clone();
        unsafe {
            command.pre_exec(move || {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if identity.uid != 0 && libc::geteuid() == 0 {
                    let groups = groups
                        .iter()
                        .map(|group| *group as libc::gid_t)
                        .collect::<Vec<_>>();
                    if libc::setgroups(groups.len(), groups.as_ptr()) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::setgid(identity.gid) != 0 || libc::setuid(identity.uid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            runtime.fail(format!(
                "could not spawn initial runtime {}: {error}",
                program.display()
            ));
            return;
        }
    };
    *runtime
        .initial_pid
        .lock()
        .expect("initial runtime lock poisoned") = Some(child.id());
    match child.wait() {
        Ok(status) => runtime.record_initial_exit(status),
        Err(error) => runtime.fail(format!(
            "initial runtime {} failed: {error}",
            program.display()
        )),
    }
    *runtime
        .initial_pid
        .lock()
        .expect("initial runtime lock poisoned") = None;
    let _ = args;
    runtime.stopping.store(true, Ordering::Release);
}

fn exit_status_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            128 + status.signal().unwrap_or(libc::SIGTERM)
        }
        #[cfg(not(unix))]
        {
            1
        }
    })
}

fn make_snapshot(
    loaded: &LoadedConfig,
    generation: Generation,
) -> Result<ConfigSnapshot, CliError> {
    ConfigSnapshot::from_resolved_config(
        generation,
        loaded
            .profile_chain()
            .iter()
            .map(|profile| profile.id.clone())
            .collect(),
        loaded.config().clone(),
        loaded.config().provenance.clone(),
        [0; 32],
    )
    .map_err(|error| CliError::Backend(error.to_string()))
}

pub(crate) fn prepare(
    context: dev_env_model::RequestContext,
) -> Result<PreparedResponse, CliError> {
    let response = request(None, BackendRequest::Prepare { context })?;
    match response.response {
        BackendResponse::Prepared(prepared) => {
            if !prepared.cwd.is_absolute()
                || !prepared.shell_invocation.executable.is_absolute()
                || prepared
                    .shell_invocation
                    .args
                    .iter()
                    .any(|arg| arg.contains('\0'))
            {
                return Err(CliError::Backend(
                    "backend returned an invalid prepared invocation".to_owned(),
                ));
            }
            prepared
                .materialized_environment
                .validate()
                .map_err(|error| CliError::Backend(error.to_string()))?;
            Ok(prepared)
        }
        BackendResponse::Error(error) => Err(CliError::Backend(format!(
            "{}: {}",
            error.class, error.message
        ))),
        other => Err(CliError::Backend(format!(
            "backend returned unexpected response: {other:?}"
        ))),
    }
}

pub(crate) fn request_backend(message: BackendRequest) -> Result<BackendResponseMessage, CliError> {
    request(None, message)
}

fn request(
    socket: Option<PathBuf>,
    request: BackendRequest,
) -> Result<BackendResponseMessage, CliError> {
    let socket = socket_path(socket)?;
    let mut stream = UnixStream::connect(&socket).map_err(|source| {
        CliError::BackendUnavailable(format!(
            "backend unavailable at {}: {source}",
            socket.display()
        ))
    })?;
    let request_id = format!(
        "cli-{}-{}",
        std::process::id(),
        REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let request = match request {
        BackendRequest::Prepare { mut context } => {
            context.request_id = request_id.clone();
            BackendRequest::Prepare { context }
        }
        request => request,
    };
    let message = BackendRequestMessage::new(request_id, request);
    write_backend_request(&mut stream, &message)
        .map_err(|error| CliError::Backend(error.to_string()))?;
    read_backend_response(&mut stream).map_err(|error| CliError::Backend(error.to_string()))
}

static REQUEST_SEQUENCE: AtomicUsize = AtomicUsize::new(1);

fn print_response(response: BackendResponseMessage) -> Result<(), CliError> {
    if let BackendResponse::Error(error) = &response.response {
        return Err(CliError::Backend(format!(
            "{}: {}",
            error.class, error.message
        )));
    }
    let output = serde_json::to_string(&response.response)
        .map_err(|error| CliError::Backend(error.to_string()))?;
    crate::output::write_stdout(&format!("{output}\n"))
        .map_err(|error| CliError::Backend(error.to_string()))
}

fn socket_path(path: Option<PathBuf>) -> Result<PathBuf, CliError> {
    let path = path
        .or_else(|| std::env::var_os("DEVENV_BACKEND_SOCKET").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_BACKEND_SOCKET));
    if !path.is_absolute() || path.as_os_str().is_empty() || path.to_string_lossy().contains('\0') {
        return Err(CliError::Backend(
            "backend socket must be an absolute path".to_owned(),
        ));
    }
    Ok(path)
}

fn backend_socket_gid() -> Result<u32, CliError> {
    if let Some(value) = env::var_os("CONTAINER_INIT_HANDOFF_GID") {
        return value.to_string_lossy().parse::<u32>().map_err(|_| {
            CliError::Backend("CONTAINER_INIT_HANDOFF_GID must be an integer".to_owned())
        });
    }
    Ok(unsafe { libc::getegid() })
}

fn set_owner(path: &Path, uid: u32, gid: u32) -> Result<(), CliError> {
    let encoded = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| CliError::Backend("backend path contains NUL".to_owned()))?;
    if unsafe { libc::chown(encoded.as_ptr(), uid, gid) } != 0 {
        return Err(CliError::Backend(format!(
            "could not set backend ownership for {}: {}",
            path.display(),
            io::Error::last_os_error()
        )));
    }
    Ok(())
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                output.push(component.as_os_str())
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                output.pop();
            }
            std::path::Component::Normal(value) => output.push(value),
        }
    }
    output
}

fn plan_value(snapshot: &ConfigSnapshot) -> serde_json::Value {
    serde_json::json!({
        "generation": snapshot.generation,
        "config_fingerprint": hex(&snapshot.config_fingerprint),
        "profile_chain": snapshot.profile_chain,
        "workspace_root": snapshot.workspace_root,
        "provider_order": snapshot.provider_order,
    })
}

fn explain_value(snapshot: &ConfigSnapshot, path: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "generation": snapshot.generation,
        "config_fingerprint": hex(&snapshot.config_fingerprint),
        "profile_chain": snapshot.profile_chain,
        "path": path,
        "provenance": snapshot.source_provenance,
    })
}

fn doctor_value(snapshot: &ConfigSnapshot) -> serde_json::Value {
    let workspace_ok = snapshot.workspace_root.is_dir();
    let shells = snapshot
        .shells
        .iter()
        .map(|(id, shell)| (id.clone(), Path::new(&shell.command).is_file()))
        .collect::<BTreeMap<_, _>>();
    let providers = snapshot
        .resolved_config
        .providers
        .iter()
        .map(|(id, provider)| {
            let available = if provider.executable.starts_with('/') {
                Path::new(&provider.executable).is_file()
            } else {
                true
            };
            (id.clone(), available)
        })
        .collect::<BTreeMap<_, _>>();
    let provider_failures = snapshot
        .resolved_config
        .providers
        .iter()
        .filter(|(_id, provider)| {
            provider.detect_files.is_empty()
                && provider.missing == dev_env_model::MissingProviderPolicy::Error
                && !providers.get(*_id).copied().unwrap_or(true)
        })
        .count();
    serde_json::json!({
        "ok": workspace_ok && shells.values().all(|ok| *ok) && provider_failures == 0,
        "workspace": { "path": snapshot.workspace_root, "is_directory": workspace_ok },
        "shells": shells,
        "providers": providers,
    })
}

fn requested_identity(
    context: &dev_env_model::RequestContext,
    peer: dev_env_model::IdentityPeer,
) -> IdentityRequest {
    match &context.requested_identity {
        IdentityRequest::User { name } => IdentityRequest::User { name: name.clone() },
        IdentityRequest::Root => IdentityRequest::Root,
        IdentityRequest::Peer if peer.uid == 0 => IdentityRequest::Root,
        IdentityRequest::Peer => IdentityRequest::Peer,
    }
}

fn provider_detect_fingerprints(
    snapshot: &ConfigSnapshot,
    context: &RuntimeContext,
) -> BTreeMap<String, [u8; 32]> {
    snapshot
        .provider_order
        .iter()
        .map(|provider_id| {
            let fingerprint = snapshot
                .resolved_config
                .providers
                .get(provider_id)
                .and_then(|config| {
                    dev_env_provider::detect(
                        config,
                        &context.workspace,
                        &context.process_environment,
                        &dev_env_provider::SystemExecutableLocator,
                    )
                    .ok()
                })
                .and_then(|detection| {
                    dev_env_provider::workspace_fingerprint(
                        &context.workspace,
                        &detection.matched_files,
                    )
                    .ok()
                })
                .unwrap_or([0; 32]);
            (provider_id.clone(), fingerprint)
        })
        .collect()
}

fn build_shell_invocation(
    context: &dev_env_model::RequestContext,
    shell_id: &str,
    config: &dev_env_model::ShellConfig,
) -> BackendShellInvocation {
    let adapter = ConfiguredShellAdapter::new(shell_id);
    let invocation = if matches!(context.mode, dev_env_model::RequestMode::LoginShell) {
        ShellInvocation::login(context.shell_args.iter().cloned().map(Into::into))
    } else {
        ShellInvocation::interactive(context.shell_args.iter().cloned().map(Into::into))
    };
    let command = build_invocation(&adapter, config, &invocation);
    match command {
        Ok(command) => {
            let (executable, args) = command.into_parts();
            BackendShellInvocation {
                executable,
                args: args
                    .into_iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect(),
            }
        }
        Err(_) => BackendShellInvocation {
            executable: PathBuf::from(&config.command),
            args: context.shell_args.clone(),
        },
    }
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(unix)]
fn peer_credentials_from_current_thread() -> dev_env_model::IdentityPeer {
    // The request handler owns a UnixStream, but SO_PEERCRED is intentionally
    // checked in the connection path below.  This fallback is the backend's
    // own uid for the initial runtime and platforms without peer credentials.
    dev_env_model::IdentityPeer {
        pid: std::process::id(),
        uid: unsafe { libc::geteuid() },
        gid: unsafe { libc::getegid() },
    }
}

#[cfg(target_os = "linux")]
fn peer_credentials(stream: &UnixStream) -> dev_env_model::IdentityPeer {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            std::os::fd::AsRawFd::as_raw_fd(stream),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result == 0 {
        dev_env_model::IdentityPeer {
            pid: credentials.pid as u32,
            uid: credentials.uid,
            gid: credentials.gid,
        }
    } else {
        peer_credentials_from_current_thread()
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn peer_credentials(_stream: &UnixStream) -> dev_env_model::IdentityPeer {
    peer_credentials_from_current_thread()
}

#[cfg(not(unix))]
fn peer_credentials(_stream: &UnixStream) -> dev_env_model::IdentityPeer {
    peer_credentials_from_current_thread()
}

#[cfg(not(unix))]
fn peer_credentials_from_current_thread() -> dev_env_model::IdentityPeer {
    dev_env_model::IdentityPeer {
        pid: std::process::id(),
        uid: 0,
        gid: 0,
    }
}

#[cfg(unix)]
static SIGNAL_REQUESTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn signal_handler(_: libc::c_int) {
    SIGNAL_REQUESTED.store(true, Ordering::Release);
}

fn install_signal_handlers() {
    #[cfg(unix)]
    unsafe {
        libc::signal(
            libc::SIGTERM,
            signal_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            signal_handler as *const () as libc::sighandler_t,
        );
    }
}

fn signal_requested() -> bool {
    #[cfg(unix)]
    {
        SIGNAL_REQUESTED.load(Ordering::Acquire)
    }
    #[cfg(not(unix))]
    {
        false
    }
}
