//! The long-lived dev-env backend process.
//!
//! The backend owns configuration loading and the provider execution boundary.
//! Clients only exchange the versioned model protocol over a Unix socket.

use {
    crate::{
        BackendCommand, BackendInitial, BackendSupport, BackendTransport, CliError, CliOptions,
        ConfigApi, ConnectionRequestError, IdentityBroker, TrustApi, CONNECTION_TIMEOUT,
        ENDPOINT_DISPLAY, MAX_ACTIVE_CONNECTIONS, MAX_ACTIVE_REQUESTS,
    },
    dev_env_core::{
        MaterializationDiagnostic, MaterializationMetadata, MaterializationService, Materializer,
        RuntimeContext,
    },
    dev_env_model::{
        BackendError, BackendProtocolError, BackendRequest, BackendRequestMessage, BackendResponse,
        BackendResponseMessage, BackendState, BackendStatus, ConfigSnapshot, Generation, HelloInfo,
        IdentityPeer, IdentityRequest, MaterializationKeyBuilder, PreparedResponse,
        ProviderDiagnosticSummary, ReceiptSummary, RequestContext, RequestMode, TrustTarget,
    },
    dev_env_shell::{build_invocation, CommandLine, ConfiguredShellAdapter, ShellInvocation},
    fs2::FileExt,
    libc::{geteuid, gid_t, kill, pid_t, setgid, setgroups, setpgid, setuid, SIGKILL, SIGTERM},
    std::{
        collections::BTreeMap,
        env,
        io::{self, Error as IoError},
        os::unix::net::UnixStream,
        path::{Path, PathBuf},
        process::{id, ExitStatus},
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex, RwLock,
        },
        thread,
        time::{Duration, Instant},
    },
};

#[cfg(test)]
use {
    crate::BACKEND_ABSTRACT_ENDPOINT,
    dev_env_model::DEVENV_BACKEND_PROTOCOL_VERSION,
    libc::sa_family_t,
    std::{io::Read, mem::size_of, slice::from_raw_parts},
};

pub struct BackendApi;

impl BackendApi {
    pub fn dispatch(command: BackendCommand, options: CliOptions) -> Result<i32, CliError> {
        dispatch_inner(command, options)
    }

    pub fn prepare(context: RequestContext) -> Result<PreparedResponse, CliError> {
        BackendSupport::prepare_inner(context)
    }

    pub fn request_backend(message: BackendRequest) -> Result<BackendResponseMessage, CliError> {
        BackendSupport::request_backend_inner(message)
    }
}

struct BackendRuntime {
    snapshot: RwLock<Arc<ConfigSnapshot>>,
    options: CliOptions,
    allowed_peer_uid: u32,
    materialization: RwLock<Arc<MaterializationService>>,
    state: Mutex<BackendState>,
    active_connections: AtomicUsize,
    active_requests: AtomicUsize,
    stopping: AtomicBool,
    initial_pid: Mutex<Option<u32>>,
    initial_exit_code: Mutex<Option<i32>>,
    failure: Mutex<Option<String>>,
}

fn dispatch_inner(command: BackendCommand, options: CliOptions) -> Result<i32, CliError> {
    match command {
        BackendCommand::Run { initial } => run(initial, options),
        BackendCommand::Status { json: _ } => {
            let response = BackendSupport::request(BackendRequest::Status)?;
            BackendSupport::print_response(response)?;
            Ok(0)
        }
        BackendCommand::Reload { wait } => {
            let response = BackendSupport::request(BackendRequest::Reload { wait })?;
            BackendSupport::print_response(response)?;
            Ok(0)
        }
        BackendCommand::Stop => {
            let response = BackendSupport::request(BackendRequest::Stop)?;
            BackendSupport::print_response(response)?;
            Ok(0)
        }
    }
}

fn run(initial: BackendInitial, options: CliOptions) -> Result<i32, CliError> {
    let allowed_peer_uid = BackendSupport::configured_allowed_peer_uid()?;
    let lock = BackendSupport::backend_lock()?;
    if let Err(source) = lock.try_lock_exclusive() {
        if source.kind() != io::ErrorKind::WouldBlock {
            return Err(CliError::Backend(format!(
                "could not acquire backend instance lock: {source}"
            )));
        }
        if let Ok(response) = BackendSupport::request(BackendRequest::Status) {
            BackendSupport::print_response(response)?;
            return Ok(0);
        }
        return Err(CliError::Backend(format!(
            "BACKEND-INSTANCE-CONFLICT: backend.lock is held but the abstract endpoint {ENDPOINT_DISPLAY} is unavailable"
        )));
    }

    // The lock is held before reading any profile.  A second invocation never
    // reloads configuration or races the active backend; a stale lock is
    // released by the operating system when the previous owner exits.
    let loaded = ConfigApi::load(&options)?;
    let snapshot = BackendSupport::make_snapshot(&loaded, Generation::INITIAL)?;

    let listener = BackendTransport::bind().map_err(|source| {
        CliError::Backend(format!(
            "could not bind abstract backend endpoint {ENDPOINT_DISPLAY}: {source}"
        ))
    })?;
    listener.set_nonblocking(true).map_err(|source| {
        CliError::Backend(format!(
            "could not configure abstract backend endpoint: {source}"
        ))
    })?;

    let runtime = Arc::new(BackendRuntime {
        snapshot: RwLock::new(Arc::new(snapshot)),
        options,
        allowed_peer_uid,
        materialization: RwLock::new(Arc::new(MaterializationService::new(
            Materializer::try_new(loaded.config().clone()).map_err(CliError::Core)?,
        ))),
        // The socket is available while the initial runtime is being
        // prepared, but clients must not receive Prepare responses until the
        // initial handoff has successfully spawned.
        state: Mutex::new(BackendState::Starting),
        active_connections: AtomicUsize::new(0),
        active_requests: AtomicUsize::new(0),
        stopping: AtomicBool::new(false),
        initial_pid: Mutex::new(None),
        initial_exit_code: Mutex::new(None),
        failure: Mutex::new(None),
    });
    BackendSupport::install_signal_handlers();
    let runtime_for_initial = Arc::clone(&runtime);
    thread::spawn(move || run_initial(runtime_for_initial, initial));

    while !runtime.stopping.load(Ordering::Acquire) && !BackendSupport::signal_requested() {
        match listener.accept() {
            Ok((stream, _)) => {
                let active = runtime.active_connections.fetch_add(1, Ordering::AcqRel);
                if active >= MAX_ACTIVE_CONNECTIONS {
                    runtime.active_connections.fetch_sub(1, Ordering::AcqRel);
                    continue;
                }
                let _ = stream.set_read_timeout(Some(CONNECTION_TIMEOUT));
                let _ = stream.set_write_timeout(Some(CONNECTION_TIMEOUT));
                let runtime = Arc::clone(&runtime);
                thread::spawn(move || {
                    serve_connection(Arc::clone(&runtime), stream);
                    runtime.active_connections.fetch_sub(1, Ordering::AcqRel);
                });
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

    fn record_initial_exit(&self, status: ExitStatus) {
        *self
            .initial_exit_code
            .lock()
            .expect("initial exit code lock poisoned") =
            Some(BackendSupport::exit_status_code(status));
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
            let group = -(pid as pid_t);
            let _ = kill(group, SIGTERM);
            thread::sleep(Duration::from_millis(50));
            let _ = kill(group, SIGKILL);
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
            BackendResponse::Error(
                BackendError::new(class, retryable, message)
                    .with_generation(snapshot.generation())
                    .with_request_id(request_id),
            ),
        )
    }
}

fn serve_connection(runtime: Arc<BackendRuntime>, mut stream: UnixStream) {
    let (request, peer) =
        match BackendSupport::read_authenticated_request(&mut stream, runtime.allowed_peer_uid) {
            Ok(request) => request,
            Err(ConnectionRequestError::Unauthorized(peer)) => {
                BackendSupport::record_rejected_peer(peer.uid);
                let response = runtime.error(
                    "peer-authentication",
                    "BACKEND-PEER-REJECTED",
                    format!(
                        "peer uid {} is not allowed; configured peer uid is {}",
                        peer.uid, runtime.allowed_peer_uid
                    ),
                    false,
                );
                let _ = response.write_to(&mut stream);
                return;
            }
            Err(ConnectionRequestError::PeerCredentials(_source)) => return,
            Err(ConnectionRequestError::Protocol(BackendProtocolError::UnsupportedVersion(
                version,
            ))) => {
                let response = BackendSupport::protocol_mismatch_response(version);
                let _ = response.write_to(&mut stream);
                return;
            }
            Err(ConnectionRequestError::Protocol(_source)) => return,
        };
    if runtime.active_requests.fetch_add(1, Ordering::AcqRel) >= MAX_ACTIVE_REQUESTS {
        runtime.active_requests.fetch_sub(1, Ordering::AcqRel);
        let response = runtime.error(
            request.request_id(),
            "busy",
            "backend request limit reached",
            true,
        );
        let _ = response.write_to(&mut stream);
        return;
    }
    let response = handle_request(&runtime, &request, peer);
    runtime.active_requests.fetch_sub(1, Ordering::AcqRel);
    let _ = response.write_to(&mut stream);
}

fn handle_request(
    runtime: &Arc<BackendRuntime>,
    request: &BackendRequestMessage,
    peer: IdentityPeer,
) -> BackendResponseMessage {
    let snapshot = runtime.current_snapshot();
    match &request.request() {
        BackendRequest::Hello => BackendResponseMessage::new(
            request.request_id(),
            BackendResponse::Hello(HelloInfo {
                state: *runtime.state.lock().expect("backend state lock poisoned"),
                generation: snapshot.generation(),
                config_fingerprint: snapshot.config_fingerprint(),
                runtime_inputs: snapshot.runtime_inputs().keys().cloned().collect(),
                environment_names: snapshot
                    .resolved_config()
                    .environment()
                    .variables()
                    .keys()
                    .cloned()
                    .collect(),
            }),
        ),
        BackendRequest::Status => BackendResponseMessage::new(
            request.request_id().to_owned(),
            BackendResponse::Status(BackendStatus {
                state: *runtime.state.lock().expect("backend state lock poisoned"),
                generation: snapshot.generation(),
                config_fingerprint: snapshot.config_fingerprint(),
                backend_pid: id(),
                active_requests: runtime
                    .active_requests
                    .load(Ordering::Acquire)
                    .saturating_sub(1),
            }),
        ),
        BackendRequest::Plan => BackendResponseMessage::new(
            request.request_id().to_owned(),
            BackendResponse::Plan(BackendSupport::plan_value(&snapshot)),
        ),
        BackendRequest::Explain { path } => BackendResponseMessage::new(
            request.request_id().to_owned(),
            BackendResponse::Explain(BackendSupport::explain_value(&snapshot, path.as_deref())),
        ),
        BackendRequest::Doctor => BackendResponseMessage::new(
            request.request_id().to_owned(),
            BackendResponse::Doctor(BackendSupport::doctor_value(
                &snapshot,
                peer.uid,
                runtime.allowed_peer_uid,
            )),
        ),
        BackendRequest::Prepare { context } => {
            handle_prepare(runtime, request, context, peer, false)
        }
        BackendRequest::Trust { target } if peer.uid == 0 => handle_trust(runtime, request, target),
        BackendRequest::Trust { .. } => runtime.error(
            request.request_id(),
            "unauthorized",
            "trust updates require a root backend owner",
            false,
        ),
        BackendRequest::Reload { .. } if peer.uid == 0 => handle_reload(runtime, request),
        BackendRequest::Reload { .. } => runtime.error(
            request.request_id(),
            "unauthorized",
            "reload requires a root backend owner",
            false,
        ),
        BackendRequest::Stop if peer.uid == 0 => {
            *runtime.state.lock().expect("backend state lock poisoned") = BackendState::Stopping;
            runtime.stopping.store(true, Ordering::Release);
            BackendResponseMessage::new(request.request_id().to_owned(), BackendResponse::Stopped)
        }
        BackendRequest::Stop => runtime.error(
            request.request_id(),
            "unauthorized",
            "stop requires a root backend owner",
            false,
        ),
    }
}

fn handle_prepare(
    runtime: &Arc<BackendRuntime>,
    request: &BackendRequestMessage,
    context: &RequestContext,
    peer: IdentityPeer,
    allow_starting: bool,
) -> BackendResponseMessage {
    let state = *runtime.state.lock().expect("backend state lock poisoned");
    if state != BackendState::Ready && !(allow_starting && state == BackendState::Starting) {
        return runtime.error(
            request.request_id(),
            "backend_busy",
            "backend is not ready for Prepare",
            true,
        );
    }
    let snapshot = runtime.current_snapshot();
    let cwd = BackendSupport::normalize_path(context.cwd());
    if !cwd.starts_with(snapshot.workspace_root()) {
        return runtime.error(
            request.request_id(),
            "invalid_input",
            "request cwd is outside the snapshot workspace",
            false,
        );
    }
    if context
        .typed_runtime_inputs()
        .keys()
        .any(|name| !snapshot.runtime_inputs().contains_key(name))
    {
        return runtime.error(
            request.request_id(),
            "invalid_input",
            "unknown runtime input",
            false,
        );
    }
    let requested_identity = BackendSupport::requested_identity(context, peer);
    if !matches!(requested_identity, IdentityRequest::Peer) && peer.uid != 0 {
        return runtime.error(
            request.request_id(),
            "unauthorized",
            "non-root clients may only request their peer identity",
            false,
        );
    }
    let shell = if context.shell() == "default" {
        snapshot.resolved_config().shell().default().to_owned()
    } else {
        context.shell().to_owned()
    };
    let process_environment = if snapshot.resolved_config().environment().inherit_process() {
        context.filtered_ambient_environment().clone()
    } else {
        BTreeMap::new()
    };
    let identity = match IdentityBroker::resolve(
        &requested_identity,
        peer,
        &cwd,
        context.typed_runtime_inputs().clone(),
        process_environment.clone(),
    ) {
        Ok(identity) => identity,
        Err(error) => {
            return runtime.error(
                request.request_id(),
                "identity_broker",
                error.to_string(),
                true,
            )
        }
    };
    let runtime_context = RuntimeContext::new(
        snapshot.workspace_root().clone(),
        cwd.clone(),
        shell.clone(),
        process_environment.clone(),
    )
    .with_user_id(identity.uid())
    .with_workspace_config_present(snapshot.workspace_root().join(".dev-env.toml").is_file());
    let detect_fingerprints =
        BackendSupport::provider_detect_fingerprints(&snapshot, &runtime_context);
    let key = match MaterializationKeyBuilder::new(
        snapshot.generation(),
        snapshot.workspace_root().clone(),
        cwd.clone(),
        shell.clone(),
        identity.clone(),
    )
    .with_config_fingerprint(snapshot.config_fingerprint())
    .with_runtime_inputs(context.typed_runtime_inputs().clone())
    .with_ambient_environment(process_environment.clone())
    .with_provider_detect_fingerprints(detect_fingerprints)
    .build()
    {
        Ok(key) => key,
        Err(error) => {
            return runtime.error(
                request.request_id(),
                "materialization",
                error.to_string(),
                false,
            )
        }
    };
    let Some(shell_config) = snapshot.shells().get(&shell) else {
        return runtime.error(
            request.request_id(),
            "invalid_input",
            "unknown shell",
            false,
        );
    };
    let shell_invocation = BackendSupport::build_shell_invocation(context, &shell, shell_config);
    let metadata = MaterializationMetadata::new(
        request.request_id(),
        snapshot.generation(),
        identity.clone(),
    );
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
                    request.request_id(),
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
        action_count: materialization.environment().provider_receipts().len(),
        warning_count: diagnostics.len(),
    };
    BackendResponseMessage::new(
        request.request_id(),
        BackendResponse::Prepared(Box::new(PreparedResponse {
            generation: snapshot.generation(),
            config_fingerprint: snapshot.config_fingerprint(),
            cwd,
            shell_invocation,
            materialized_environment: materialization.environment().clone(),
            supplemental_groups: identity.supplementary_groups().clone(),
            identity,
            diagnostics,
            cache_hit: materialization.cache_hit(),
            receipt_summary,
        })),
    )
}

fn handle_reload(
    runtime: &Arc<BackendRuntime>,
    request: &BackendRequestMessage,
) -> BackendResponseMessage {
    *runtime.state.lock().expect("backend state lock poisoned") = BackendState::Reloading;
    let deadline = Instant::now() + Duration::from_secs(30);
    while runtime.active_requests.load(Ordering::Acquire) > 1 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    if runtime.active_requests.load(Ordering::Acquire) > 1 {
        *runtime.state.lock().expect("backend state lock poisoned") = BackendState::Ready;
        return runtime.error(
            request.request_id(),
            "reload_busy",
            "backend requests did not finish before reload deadline",
            true,
        );
    }
    let current = runtime.current_snapshot();
    let result = ConfigApi::load(&runtime.options).and_then(|loaded| {
        BackendSupport::make_snapshot(
            &loaded,
            current
                .generation()
                .next()
                .map_err(|error| CliError::Backend(error.to_string()))?,
        )
    });
    match result {
        Ok(next) => {
            let generation = next.generation();
            let fingerprint = next.config_fingerprint();
            let materializer = match Materializer::try_new(next.resolved_config().clone()) {
                Ok(materializer) => MaterializationService::new(materializer),
                Err(error) => {
                    *runtime.state.lock().expect("backend state lock poisoned") =
                        BackendState::Ready;
                    return runtime.error(
                        request.request_id(),
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
                request.request_id(),
                BackendResponse::Reloaded {
                    generation,
                    config_fingerprint: fingerprint,
                },
            )
        }
        Err(error) => {
            *runtime.state.lock().expect("backend state lock poisoned") = BackendState::Ready;
            runtime.error(
                request.request_id(),
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
        TrustTarget::Sha256 { digest } => PathBuf::from(BackendSupport::hex(digest)),
    };
    match TrustApi::trust(&target) {
        Ok(_) => BackendResponseMessage::new(
            request.request_id(),
            BackendResponse::Trusted {
                generation: runtime.current_snapshot().generation(),
            },
        ),
        Err(error) => runtime.error(request.request_id(), "trust", error.to_string(), false),
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
        env::var("CONTAINER_INIT_HANDOFF_USER").unwrap_or_else(|_| "dev".to_owned())
    };
    let context = RequestContext::new(
        request_id,
        RequestMode::Shell,
        snapshot.workspace_root().clone(),
        snapshot.resolved_config().shell().default().to_owned(),
    )
    .with_identity(IdentityRequest::User { name: initial_user })
    .with_runtime_inputs(
        snapshot
            .runtime_inputs()
            .keys()
            .filter_map(|name| env::var(name).ok().map(|value| (name.clone(), value))),
    )
    .with_ambient_environment(ConfigApi::ambient_environment());
    let response = handle_prepare(
        &runtime,
        &BackendRequestMessage::new(
            request_id,
            BackendRequest::Prepare {
                context: context.clone(),
            },
        ),
        &context,
        BackendSupport::peer_credentials_from_current_thread(),
        true,
    );
    let prepared = match response.response() {
        BackendResponse::Prepared(prepared) => prepared.as_ref().clone(),
        BackendResponse::Error(error) => {
            runtime.fail(format!(
                "initial runtime preparation failed: {}: {}",
                error.class(),
                error.message()
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
            let shell = snapshot.resolved_config().shell().default().to_owned();
            let Some(config) = snapshot.shells().get(&shell) else {
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
        .env("HOME", prepared.identity.home())
        .env("USER", prepared.identity.user())
        .env("LOGNAME", prepared.identity.user());
    command.current_dir(&prepared.cwd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let identity = prepared.identity.clone();
        let groups = prepared.supplemental_groups.clone();
        unsafe {
            command.pre_exec(move || {
                if setpgid(0, 0) == -1 {
                    return Err(IoError::last_os_error());
                }
                if identity.uid() != 0 && geteuid() == 0 {
                    let groups = groups
                        .iter()
                        .map(|group| *group as gid_t)
                        .collect::<Vec<_>>();
                    if setgroups(groups.len(), groups.as_ptr()) != 0 {
                        return Err(IoError::last_os_error());
                    }
                    if setgid(identity.gid()) != 0 || setuid(identity.uid()) != 0 {
                        return Err(IoError::last_os_error());
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
    *runtime.state.lock().expect("backend state lock poisoned") = BackendState::Ready;
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

#[cfg(test)]
struct ReadCounter {
    reads: usize,
}

#[cfg(test)]
impl Read for ReadCounter {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reads += 1;
        buffer[0] = b'!';
        Ok(1)
    }
}

#[cfg(test)]
#[test]
fn abstract_endpoint_has_linux_abstract_namespace_encoding() {
    let (address, length) = BackendTransport::sockaddr_for(BACKEND_ABSTRACT_ENDPOINT).unwrap();
    let encoded = unsafe {
        from_raw_parts(
            address.sun_path.as_ptr().cast::<u8>(),
            BACKEND_ABSTRACT_ENDPOINT.len(),
        )
    };
    assert_eq!(encoded, BACKEND_ABSTRACT_ENDPOINT);
    assert_eq!(
        length as usize,
        size_of::<sa_family_t>() + BACKEND_ABSTRACT_ENDPOINT.len()
    );
    assert_eq!(DEVENV_BACKEND_PROTOCOL_VERSION, 4);
}

#[cfg(test)]
#[test]
fn abstract_transport_connects_without_a_filesystem_socket() {
    let endpoint = format!("\0dev-env-test-{}", id());
    let listener = BackendTransport::bind_endpoint(endpoint.as_bytes()).unwrap();
    let client = BackendTransport::connect_endpoint(endpoint.as_bytes()).unwrap();
    let (server, _) = listener.accept().unwrap();
    assert!(listener.local_addr().unwrap().as_pathname().is_none());
    assert_eq!(
        BackendSupport::peer_credentials(&server).unwrap().uid,
        unsafe { geteuid() }
    );
    drop(client);
}

#[cfg(test)]
#[test]
fn uid_policy_does_not_depend_on_group_membership() {
    let peer = IdentityPeer {
        pid: 12,
        uid: 1000,
        gid: 4242,
    };
    assert!(BackendSupport::peer_is_authorized(peer, 1000));
    assert!(!BackendSupport::peer_is_authorized(peer, 1001));
    assert!(BackendSupport::peer_is_authorized(
        IdentityPeer { uid: 0, ..peer },
        1001
    ));
}

#[cfg(test)]
#[test]
fn unauthorized_peer_is_rejected_before_frame_read() {
    let peer = IdentityPeer {
        pid: 12,
        uid: 1000,
        gid: 4242,
    };
    let mut reader = ReadCounter { reads: 0 };
    let result = BackendSupport::read_request_for_peer(&mut reader, peer, 1001);
    assert!(matches!(result, Err(ConnectionRequestError::Unauthorized(found)) if found == peer));
    assert_eq!(reader.reads, 0);
}

#[cfg(test)]
#[test]
fn legacy_protocol_version_gets_a_typed_mismatch() {
    let response = BackendSupport::protocol_mismatch_response(3);
    assert_eq!(response.protocol(), DEVENV_BACKEND_PROTOCOL_VERSION);
    assert!(matches!(
        response.response(),
        BackendResponse::Error(error)
            if error.class() == "BACKEND-PROTOCOL-MISMATCH"
                && error.message().contains("protocol v3")
    ));
}
