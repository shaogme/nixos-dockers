use {
    crate::{
        BackendSupport, CliError, CliOptions, ConfigApi, IdentityBroker, TrustApi,
        MAX_ACTIVE_CONNECTIONS, MAX_ACTIVE_REQUESTS,
    },
    dev_env_core::{
        Materialization, MaterializationDiagnostic, MaterializationMetadata,
        MaterializationService, Materializer, RuntimeContext,
    },
    dev_env_model::{
        BackendError, BackendRequest, BackendRequestMessage, BackendResponse,
        BackendResponseMessage, BackendStatus, ConfigSnapshot, ControlPlaneState,
        EffectiveIdentity, Generation, HelloInfo, IdentityPeer, IdentityRequest,
        InitialRuntimeState, MaterializationKey, MaterializationKeyBuilder, PreparedResponse,
        ProviderDiagnosticSummary, ReceiptSummary, RequestContext, ShellInvocation, TrustTarget,
    },
    dev_env_provider::ProcessExecutor,
    libc::{kill, pid_t, SIGKILL, SIGTERM},
    std::{
        collections::{BTreeMap, HashMap},
        path::PathBuf,
        process::id,
        sync::{
            mpsc::{Receiver, Sender, SyncSender},
            Arc,
        },
        thread,
        time::Duration,
    },
};

pub(super) struct BackendFailure {
    pub(super) class: String,
    pub(super) message: String,
}

struct PrepareRoute {
    request_id: String,
    snapshot: Arc<ConfigSnapshot>,
    cwd: PathBuf,
    shell_invocation: ShellInvocation,
    identity: EffectiveIdentity,
}

struct PrepareWaiter {
    route: PrepareRoute,
    reply: SyncSender<BackendResponseMessage>,
    cache_hit: bool,
}

struct PrepareFlight {
    generation: Generation,
    waiters: Vec<PrepareWaiter>,
}

struct ReloadWaiter {
    request_id: String,
    reply: SyncSender<BackendResponseMessage>,
}

pub(super) struct RunSummary {
    pub(super) failure: Option<BackendFailure>,
    pub(super) exit_code: i32,
}

pub(super) enum CoordinatorEvent {
    ConnectionOpened(SyncSender<bool>),
    ConnectionClosed,
    Request {
        request: BackendRequestMessage,
        peer: IdentityPeer,
        reply: SyncSender<BackendResponseMessage>,
    },
    RequestFinished,
    AuthenticationRejected {
        request_id: String,
        peer: IdentityPeer,
        reply: SyncSender<BackendResponseMessage>,
    },
    ProtocolRejected {
        version: u16,
        reply: SyncSender<BackendResponseMessage>,
    },
    PrepareCompleted {
        generation: Generation,
        key: MaterializationKey,
        result: Result<Arc<Materialization>, (String, String)>,
    },
    ReloadCompleted(Box<Result<(ConfigSnapshot, Materializer), String>>),
    CurrentSnapshot(SyncSender<Arc<ConfigSnapshot>>),
    InitialRunning(u32),
    InitialExited(i32),
    InitialFinished,
    Fail {
        class: String,
        message: String,
    },
    RequestStop,
    CheckStopping(SyncSender<bool>),
    ActiveConnections(SyncSender<usize>),
    Shutdown(SyncSender<RunSummary>),
}

pub(super) struct BackendState {
    sender: Sender<CoordinatorEvent>,
    options: CliOptions,
    snapshot: Arc<ConfigSnapshot>,
    materialization: MaterializationService,
    process_executor: ProcessExecutor,
    allowed_peer_uid: u32,
    control_plane: ControlPlaneState,
    initial_runtime: InitialRuntimeState,
    startup_phase: Option<String>,
    active_connections: usize,
    active_requests: usize,
    stopping: bool,
    initial_pid: Option<u32>,
    initial_exit_code: Option<i32>,
    initial_runtime_error: Option<String>,
    failure: Option<BackendFailure>,
    in_flight: HashMap<MaterializationKey, PrepareFlight>,
    pending_reload: Option<ReloadWaiter>,
    pending_shutdown: Option<SyncSender<RunSummary>>,
}

pub(super) struct BackendStateConfig {
    pub(super) sender: Sender<CoordinatorEvent>,
    pub(super) options: CliOptions,
    pub(super) snapshot: ConfigSnapshot,
    pub(super) materializer: Materializer,
    pub(super) process_executor: ProcessExecutor,
    pub(super) allowed_peer_uid: u32,
}

impl BackendState {
    pub(super) fn new(config: BackendStateConfig) -> Self {
        Self {
            sender: config.sender,
            options: config.options,
            snapshot: Arc::new(config.snapshot),
            materialization: MaterializationService::new(config.materializer),
            process_executor: config.process_executor,
            allowed_peer_uid: config.allowed_peer_uid,
            control_plane: ControlPlaneState::Serving,
            initial_runtime: InitialRuntimeState::Preparing,
            startup_phase: Some("initial_runtime_preparing".to_owned()),
            active_connections: 0,
            active_requests: 0,
            stopping: false,
            initial_pid: None,
            initial_exit_code: None,
            initial_runtime_error: None,
            failure: None,
            in_flight: HashMap::new(),
            pending_reload: None,
            pending_shutdown: None,
        }
    }

    pub(super) fn run(mut self, receiver: Receiver<CoordinatorEvent>) {
        while let Ok(event) = receiver.recv() {
            if self.handle_event(event) {
                break;
            }
        }
    }

    fn handle_event(&mut self, event: CoordinatorEvent) -> bool {
        match event {
            CoordinatorEvent::ConnectionOpened(reply) => {
                let accepted = self.active_connections < MAX_ACTIVE_CONNECTIONS;
                if accepted {
                    self.active_connections += 1;
                }
                let _ = reply.send(accepted);
            }
            CoordinatorEvent::ConnectionClosed => {
                self.active_connections = self.active_connections.saturating_sub(1);
            }
            CoordinatorEvent::Request {
                request,
                peer,
                reply,
            } => self.handle_request(request, peer, reply),
            CoordinatorEvent::RequestFinished => {
                self.active_requests = self.active_requests.saturating_sub(1);
            }
            CoordinatorEvent::AuthenticationRejected {
                request_id,
                peer,
                reply,
            } => {
                let message = format!(
                    "peer uid {} is not allowed; configured peer uid is {}",
                    peer.uid, self.allowed_peer_uid
                );
                let _ =
                    reply.send(self.error(&request_id, "BACKEND-PEER-REJECTED", message, false));
                BackendSupport::record_rejected_peer(peer.uid);
            }
            CoordinatorEvent::ProtocolRejected { version, reply } => {
                let _ = reply.send(BackendSupport::protocol_mismatch_response(version));
            }
            CoordinatorEvent::PrepareCompleted {
                generation,
                key,
                result,
            } => self.prepare_completed(generation, key, result),
            CoordinatorEvent::ReloadCompleted(result) => self.reload_completed(*result),
            CoordinatorEvent::CurrentSnapshot(reply) => {
                let _ = reply.send(Arc::clone(&self.snapshot));
            }
            CoordinatorEvent::InitialRunning(pid) => {
                self.initial_pid = Some(pid);
                self.initial_runtime = InitialRuntimeState::Running;
                self.startup_phase = None;
            }
            CoordinatorEvent::InitialExited(exit_code) => {
                self.initial_exit_code = Some(exit_code);
                self.initial_runtime = InitialRuntimeState::Exited;
            }
            CoordinatorEvent::InitialFinished => {
                self.initial_pid = None;
                self.stopping = true;
                if let Some(reply) = self.pending_shutdown.take() {
                    let _ = reply.send(self.run_summary());
                    return true;
                }
            }
            CoordinatorEvent::Fail { class, message } => self.fail_with_class(class, message),
            CoordinatorEvent::RequestStop => {
                if self.control_plane != ControlPlaneState::Failed {
                    self.control_plane = ControlPlaneState::Stopping;
                }
                self.stopping = true;
            }
            CoordinatorEvent::CheckStopping(reply) => {
                let _ = reply.send(self.stopping);
            }
            CoordinatorEvent::ActiveConnections(reply) => {
                let _ = reply.send(self.active_connections);
            }
            CoordinatorEvent::Shutdown(reply) => {
                self.stop_initial_runtime();
                self.finish_pending_requests();
                if self.initial_pid.is_some() {
                    self.pending_shutdown = Some(reply);
                } else {
                    let _ = reply.send(self.run_summary());
                    return true;
                }
            }
        }
        false
    }

    fn handle_request(
        &mut self,
        request: BackendRequestMessage,
        peer: IdentityPeer,
        reply: SyncSender<BackendResponseMessage>,
    ) {
        self.active_requests += 1;
        if self.active_requests > MAX_ACTIVE_REQUESTS {
            let response = self.error(
                request.request_id(),
                "busy",
                "backend request limit reached",
                true,
            );
            let _ = reply.send(response);
            return;
        }
        if let BackendRequest::Prepare { context } = request.request() {
            self.start_prepare(&request, context, peer, reply);
            return;
        }
        let response = self.dispatch_request(&request, peer, reply);
        if let Some((response, reply)) = response {
            let _ = reply.send(response);
        }
    }

    fn dispatch_request(
        &mut self,
        request: &BackendRequestMessage,
        peer: IdentityPeer,
        reply: SyncSender<BackendResponseMessage>,
    ) -> Option<(BackendResponseMessage, SyncSender<BackendResponseMessage>)> {
        let snapshot = Arc::clone(&self.snapshot);
        let response = match request.request() {
            BackendRequest::Hello => BackendResponse::Hello(HelloInfo {
                control_plane: self.control_plane,
                initial_runtime: self.initial_runtime,
                startup_phase: self.startup_phase.clone(),
                initial_runtime_exit_code: self.initial_exit_code,
                initial_runtime_error: self.initial_runtime_error.clone(),
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
            BackendRequest::Status => BackendResponse::Status(BackendStatus {
                control_plane: self.control_plane,
                initial_runtime: self.initial_runtime,
                startup_phase: self.startup_phase.clone(),
                initial_runtime_exit_code: self.initial_exit_code,
                initial_runtime_error: self.initial_runtime_error.clone(),
                generation: snapshot.generation(),
                config_fingerprint: snapshot.config_fingerprint(),
                backend_pid: id(),
                active_requests: self.active_requests.saturating_sub(1),
            }),
            BackendRequest::Plan => BackendResponse::Plan(BackendSupport::plan_value(&snapshot)),
            BackendRequest::Explain { path } => {
                BackendResponse::Explain(BackendSupport::explain_value(&snapshot, path.as_deref()))
            }
            BackendRequest::Doctor => BackendResponse::Doctor(BackendSupport::doctor_value(
                &snapshot,
                peer.uid,
                self.allowed_peer_uid,
            )),
            BackendRequest::Prepare { .. } => unreachable!(),
            BackendRequest::Trust { target } if peer.uid == 0 => {
                return Some((self.handle_trust(request, target), reply));
            }
            BackendRequest::Trust { .. } => {
                return Some((
                    self.error(
                        request.request_id(),
                        "unauthorized",
                        "trust updates require a root backend owner",
                        false,
                    ),
                    reply,
                ));
            }
            BackendRequest::Reload { .. } if peer.uid == 0 => {
                self.start_reload(request.request_id().to_owned(), reply);
                return None;
            }
            BackendRequest::Reload { .. } => {
                return Some((
                    self.error(
                        request.request_id(),
                        "unauthorized",
                        "reload requires a root backend owner",
                        false,
                    ),
                    reply,
                ));
            }
            BackendRequest::Stop if peer.uid == 0 => {
                self.control_plane = ControlPlaneState::Stopping;
                self.stopping = true;
                BackendResponse::Stopped
            }
            BackendRequest::Stop => {
                return Some((
                    self.error(
                        request.request_id(),
                        "unauthorized",
                        "stop requires a root backend owner",
                        false,
                    ),
                    reply,
                ));
            }
        };
        Some((
            BackendResponseMessage::new(request.request_id(), response),
            reply,
        ))
    }

    fn start_prepare(
        &mut self,
        request: &BackendRequestMessage,
        context: &RequestContext,
        peer: IdentityPeer,
        reply: SyncSender<BackendResponseMessage>,
    ) {
        if self.control_plane != ControlPlaneState::Serving {
            let (class, message) = match self.control_plane {
                ControlPlaneState::Starting | ControlPlaneState::Reloading => (
                    "backend_starting",
                    "backend control plane is not serving Prepare yet",
                ),
                ControlPlaneState::Failed => (
                    "backend_startup_failed",
                    "backend failed while preparing its initial runtime",
                ),
                ControlPlaneState::Stopping => ("backend_stopping", "backend is stopping"),
                ControlPlaneState::Serving => unreachable!(),
            };
            let _ = reply.send(self.error(request.request_id(), class, message, true));
            return;
        }
        let snapshot = Arc::clone(&self.snapshot);
        let cwd = BackendSupport::normalize_path(context.cwd());
        if !cwd.starts_with(snapshot.workspace_root()) {
            let _ = reply.send(self.error(
                request.request_id(),
                "invalid_input",
                format!(
                    "request cwd {} is outside the snapshot workspace {}",
                    cwd.display(),
                    snapshot.workspace_root().display()
                ),
                false,
            ));
            return;
        }
        if context
            .typed_runtime_inputs()
            .keys()
            .any(|name| !snapshot.runtime_inputs().contains_key(name))
        {
            let _ = reply.send(self.error(
                request.request_id(),
                "invalid_input",
                "unknown runtime input",
                false,
            ));
            return;
        }
        let requested_identity = BackendSupport::requested_identity(context, peer);
        if !matches!(requested_identity, IdentityRequest::Peer) && peer.uid != 0 {
            let _ = reply.send(self.error(
                request.request_id(),
                "unauthorized",
                "non-root clients may only request their peer identity",
                false,
            ));
            return;
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
                let _ = reply.send(self.error(
                    request.request_id(),
                    "identity_broker",
                    error.to_string(),
                    true,
                ));
                return;
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
        .with_ambient_environment(process_environment)
        .with_provider_detect_fingerprints(detect_fingerprints)
        .build()
        {
            Ok(key) => key,
            Err(error) => {
                let _ = reply.send(self.error(
                    request.request_id(),
                    "materialization",
                    error.to_string(),
                    false,
                ));
                return;
            }
        };
        let Some(shell_config) = snapshot.shells().get(&shell) else {
            let _ = reply.send(self.error(
                request.request_id(),
                "invalid_input",
                "unknown shell",
                false,
            ));
            return;
        };
        let route = PrepareRoute {
            request_id: request.request_id().to_owned(),
            snapshot: Arc::clone(&snapshot),
            cwd,
            shell_invocation: BackendSupport::build_shell_invocation(context, &shell, shell_config),
            identity: identity.clone(),
        };
        if let Some(cached) = self.materialization.cached(&key) {
            let response = prepared_response(route, cached.materialization(), true);
            let _ = reply.send(response);
            return;
        }
        if let Some(flight) = self.in_flight.get_mut(&key) {
            flight.waiters.push(PrepareWaiter {
                route,
                reply,
                cache_hit: true,
            });
            return;
        }
        let metadata =
            MaterializationMetadata::new(request.request_id(), snapshot.generation(), identity);
        let materializer = self.materialization.shared_materializer();
        let runtime_context = runtime_context.clone();
        let generation = snapshot.generation();
        let sender = self.sender_clone();
        self.in_flight.insert(
            key.clone(),
            PrepareFlight {
                generation,
                waiters: vec![PrepareWaiter {
                    route,
                    reply,
                    cache_hit: false,
                }],
            },
        );
        thread::spawn(move || {
            let result = materializer
                .materialize_with_metadata(&runtime_context, &metadata)
                .map(Arc::new)
                .map_err(|error| (error.code().to_owned(), error.to_string()));
            let _ = sender.send(CoordinatorEvent::PrepareCompleted {
                generation,
                key,
                result,
            });
        });
    }

    fn prepare_completed(
        &mut self,
        generation: Generation,
        key: MaterializationKey,
        result: Result<Arc<Materialization>, (String, String)>,
    ) {
        let Some(flight) = self.in_flight.remove(&key) else {
            return;
        };
        let current_generation = self.snapshot.generation();
        if generation == current_generation && flight.generation == generation {
            if let Ok(materialization) = &result {
                self.materialization
                    .cache_success(key, Arc::clone(materialization));
            }
        }
        for waiter in flight.waiters {
            let response = match &result {
                Ok(materialization) if generation == waiter.route.snapshot.generation() => {
                    prepared_response(waiter.route, materialization, waiter.cache_hit)
                }
                Ok(_) => self.error(
                    &waiter.route.request_id,
                    "stale_generation",
                    "materialization completed for a stale configuration generation",
                    true,
                ),
                Err((class, message)) => {
                    self.error(&waiter.route.request_id, class, message.clone(), false)
                }
            };
            let _ = waiter.reply.send(response);
        }
        self.maybe_start_reload();
    }

    fn start_reload(&mut self, request_id: String, reply: SyncSender<BackendResponseMessage>) {
        if self.pending_reload.is_some() || self.control_plane == ControlPlaneState::Reloading {
            let _ = reply.send(self.error(
                &request_id,
                "reload_busy",
                "another backend reload is already in progress",
                true,
            ));
            return;
        }
        self.control_plane = ControlPlaneState::Reloading;
        self.pending_reload = Some(ReloadWaiter { request_id, reply });
        self.maybe_start_reload();
    }

    fn maybe_start_reload(&mut self) {
        if !self.in_flight.is_empty() {
            return;
        }
        let Some(_) = self.pending_reload else {
            return;
        };
        let options = self.options.clone();
        let current = Arc::clone(&self.snapshot);
        let executor = self.process_executor.clone();
        let sender = self.sender_clone();
        thread::spawn(move || {
            let result = ConfigApi::load(&options)
                .and_then(|loaded| {
                    let generation = current
                        .generation()
                        .next()
                        .map_err(|error| CliError::Backend(error.to_string()))?;
                    let next = BackendSupport::make_snapshot(&loaded, generation)?;
                    let materializer = Materializer::try_new_with_process_executor(
                        next.resolved_config().clone(),
                        executor,
                    )
                    .map_err(CliError::Core)?;
                    Ok((next, materializer))
                })
                .map_err(|error: CliError| error.to_string());
            let _ = sender.send(CoordinatorEvent::ReloadCompleted(Box::new(result)));
        });
    }

    fn reload_completed(&mut self, result: Result<(ConfigSnapshot, Materializer), String>) {
        let Some(waiter) = self.pending_reload.take() else {
            return;
        };
        let response = match result {
            Ok((snapshot, materializer)) => {
                let generation = snapshot.generation();
                let fingerprint = snapshot.config_fingerprint();
                self.snapshot = Arc::new(snapshot);
                self.materialization = MaterializationService::new(materializer);
                BackendResponseMessage::new(
                    waiter.request_id,
                    BackendResponse::Reloaded {
                        generation,
                        config_fingerprint: fingerprint,
                    },
                )
            }
            Err(message) => self.error(&waiter.request_id, "reload_failed", message, false),
        };
        self.control_plane = ControlPlaneState::Serving;
        let _ = waiter.reply.send(response);
    }

    fn handle_trust(
        &mut self,
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
                    generation: self.snapshot.generation(),
                },
            ),
            Err(error) => self.error(request.request_id(), "trust", error.to_string(), false),
        }
    }

    fn error(
        &self,
        request_id: &str,
        class: &str,
        message: impl Into<String>,
        retryable: bool,
    ) -> BackendResponseMessage {
        BackendResponseMessage::new(
            request_id,
            BackendResponse::Error(
                BackendError::new(class, retryable, message)
                    .with_generation(self.snapshot.generation())
                    .with_request_id(request_id),
            ),
        )
    }

    fn fail_with_class(&mut self, class: String, message: String) {
        if self.stopping {
            return;
        }
        self.control_plane = ControlPlaneState::Failed;
        self.initial_runtime = InitialRuntimeState::Failed;
        self.startup_phase = Some("initial_runtime_failed".to_owned());
        self.initial_runtime_error = Some(message.clone());
        if self.failure.is_none() {
            self.failure = Some(BackendFailure { class, message });
        }
        self.stopping = true;
    }

    fn stop_initial_runtime(&mut self) {
        let Some(pid) = self.initial_pid else {
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

    fn finish_pending_requests(&mut self) {
        let error_text = "backend is stopping";
        let flights = self
            .in_flight
            .drain()
            .map(|(_, flight)| flight)
            .collect::<Vec<_>>();
        for flight in flights {
            for waiter in flight.waiters {
                let error = self.error(
                    &waiter.route.request_id,
                    "backend_stopping",
                    error_text,
                    true,
                );
                let _ = waiter.reply.send(error);
            }
        }
        if let Some(waiter) = self.pending_reload.take() {
            let error = self.error(&waiter.request_id, "backend_stopping", error_text, true);
            let _ = waiter.reply.send(error);
        }
    }

    fn run_summary(&mut self) -> RunSummary {
        RunSummary {
            failure: self.failure.take(),
            exit_code: self.initial_exit_code.unwrap_or(0),
        }
    }

    fn sender_clone(&self) -> Sender<CoordinatorEvent> {
        self.sender.clone()
    }
}

fn prepared_response(
    route: PrepareRoute,
    materialization: &Materialization,
    cache_hit: bool,
) -> BackendResponseMessage {
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
        route.request_id,
        BackendResponse::Prepared(Box::new(PreparedResponse {
            generation: route.snapshot.generation(),
            config_fingerprint: route.snapshot.config_fingerprint(),
            cwd: route.cwd,
            shell_invocation: route.shell_invocation,
            materialized_environment: materialization.environment().clone(),
            supplemental_groups: route.identity.supplementary_groups().clone(),
            identity: route.identity,
            diagnostics,
            cache_hit,
            receipt_summary,
        })),
    )
}
