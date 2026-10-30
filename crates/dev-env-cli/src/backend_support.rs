use {
    crate::{CliError, LoadedConfig, OutputApi},
    container_init_protocol::AbstractEndpoint,
    dev_env_core::RuntimeContext,
    dev_env_model::{
        BackendError, BackendProtocolError, BackendRequest, BackendRequestMessage, BackendResponse,
        BackendResponseMessage, BackendStatus, ConfigSnapshot, ControlPlaneState, Generation,
        IdentityPeer, IdentityRequest, MissingProviderPolicy, RequestContext, RequestMode,
        ShellConfig, ShellInvocation as BackendShellInvocation, DEVENV_BACKEND_PROTOCOL_VERSION,
    },
    dev_env_provider::{detect, workspace_fingerprint, SystemExecutableLocator},
    dev_env_shell::{build_invocation, ConfiguredShellAdapter, ShellInvocation},
    libc::{sockaddr_un, socklen_t},
    serde_json::{json, to_string, Value as JsonValue},
    std::{
        collections::BTreeMap,
        env,
        io::{self, Read},
        os::unix::net::{UnixListener, UnixStream},
        path::{Component, Path, PathBuf},
        process::{id, ExitStatus},
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    },
};

#[cfg(unix)]
use libc::{c_int, getegid, geteuid, sighandler_t, signal, SIGINT, SIGTERM};

pub const BACKEND_ABSTRACT_ENDPOINT: &[u8] = b"\0dev-env.backend.v5";
pub const ENDPOINT_DISPLAY: &str = "@dev-env.backend.v5";
pub const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 60_000;
pub const MAX_ACTIVE_CONNECTIONS: usize = 32;
pub const MAX_ACTIVE_REQUESTS: usize = 16;
pub const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

static REQUEST_SEQUENCE: AtomicUsize = AtomicUsize::new(1);
static REJECTED_PEER_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(unix)]
static SIGNAL_REQUESTED: AtomicBool = AtomicBool::new(false);

pub struct BackendTransport;

impl BackendTransport {
    pub fn sockaddr_for(endpoint: &[u8]) -> io::Result<(sockaddr_un, socklen_t)> {
        AbstractEndpoint::from_abstract_bytes(endpoint)?.sockaddr()
    }

    pub fn bind() -> io::Result<UnixListener> {
        Self::bind_endpoint(BACKEND_ABSTRACT_ENDPOINT)
    }

    pub fn bind_endpoint(endpoint: &[u8]) -> io::Result<UnixListener> {
        AbstractEndpoint::from_abstract_bytes(endpoint)?.bind(MAX_ACTIVE_CONNECTIONS as c_int)
    }

    pub fn connect() -> io::Result<UnixStream> {
        Self::connect_endpoint(BACKEND_ABSTRACT_ENDPOINT)
    }

    pub fn connect_endpoint(endpoint: &[u8]) -> io::Result<UnixStream> {
        let endpoint = AbstractEndpoint::from_abstract_bytes(endpoint)?;
        endpoint.connect_until(Instant::now() + CONNECTION_TIMEOUT)
    }
}

pub enum ConnectionRequestError {
    PeerCredentials(io::Error),
    Unauthorized(IdentityPeer),
    Protocol(BackendProtocolError),
}

struct StartupDeadline {
    started_at: Instant,
    deadline: Instant,
}

struct StartupObservation {
    state: String,
    phase: String,
    transport_error: Option<String>,
}

enum StartupReadiness {
    Ready,
    Pending(StartupObservation),
    Failed(CliError),
}

impl StartupDeadline {
    fn new(timeout_ms: Option<u64>) -> Result<Self, CliError> {
        let timeout_ms = match timeout_ms {
            Some(timeout_ms) => timeout_ms,
            None => match env::var("DEVENV_STARTUP_TIMEOUT_MS") {
                Ok(value) => value.parse::<u64>().map_err(|_| {
                    CliError::Backend(
                        "DEVENV_STARTUP_TIMEOUT_MS must be a positive integer".to_owned(),
                    )
                })?,
                Err(env::VarError::NotPresent) => DEFAULT_STARTUP_TIMEOUT_MS,
                Err(env::VarError::NotUnicode(_)) => {
                    return Err(CliError::Backend(
                        "DEVENV_STARTUP_TIMEOUT_MS must be valid UTF-8".to_owned(),
                    ));
                }
            },
        };
        if timeout_ms == 0 {
            return Err(CliError::Backend(
                "startup timeout must be greater than zero milliseconds".to_owned(),
            ));
        }
        let timeout = Duration::from_millis(timeout_ms);
        let started_at = Instant::now();
        let deadline = started_at.checked_add(timeout).ok_or_else(|| {
            CliError::Backend("startup timeout exceeds the supported range".to_owned())
        })?;
        Ok(Self {
            started_at,
            deadline,
        })
    }

    fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    fn elapsed_ms(&self) -> u128 {
        self.started_at.elapsed().as_millis()
    }
}

pub struct BackendSupport;

impl BackendSupport {
    pub fn configured_allowed_peer_uid() -> Result<u32, CliError> {
        match env::var_os("CONTAINER_INIT_HANDOFF_UID") {
            Some(value) => value
                .to_str()
                .and_then(|value| value.parse::<u32>().ok())
                .ok_or_else(|| {
                    CliError::Backend(
                        "CONTAINER_INIT_HANDOFF_UID must be an unsigned integer".to_owned(),
                    )
                }),
            None => Ok(unsafe { geteuid() }),
        }
    }

    pub fn read_authenticated_request(
        stream: &mut UnixStream,
        allowed_peer_uid: u32,
    ) -> Result<(BackendRequestMessage, IdentityPeer), ConnectionRequestError> {
        let peer =
            Self::peer_credentials(stream).map_err(ConnectionRequestError::PeerCredentials)?;
        let request = Self::read_request_for_peer(stream, peer, allowed_peer_uid)?;
        Ok((request, peer))
    }

    pub fn read_request_for_peer(
        reader: &mut impl Read,
        peer: IdentityPeer,
        allowed_peer_uid: u32,
    ) -> Result<BackendRequestMessage, ConnectionRequestError> {
        if !Self::peer_is_authorized(peer, allowed_peer_uid) {
            return Err(ConnectionRequestError::Unauthorized(peer));
        }
        BackendRequestMessage::read_from(reader).map_err(ConnectionRequestError::Protocol)
    }

    pub fn peer_is_authorized(peer: IdentityPeer, allowed_peer_uid: u32) -> bool {
        peer.uid == 0 || peer.uid == allowed_peer_uid
    }

    pub fn protocol_mismatch_response(version: u16) -> BackendResponseMessage {
        BackendResponseMessage::new(
            "protocol-mismatch",
            BackendResponse::Error(BackendError::new(
                "BACKEND-PROTOCOL-MISMATCH",
                false,
                format!(
                    "received dev-env backend protocol v{version}; this backend requires v{}",
                    DEVENV_BACKEND_PROTOCOL_VERSION
                ),
            )),
        )
    }

    pub fn record_rejected_peer(uid: u32) {
        let count = REJECTED_PEER_CONNECTIONS.fetch_add(1, Ordering::Relaxed) + 1;
        if count == 1 || count % 100 == 0 {
            eprintln!(
                "BACKEND-PEER-REJECTED: rejected peer uid {uid} ({count} rejected connections)"
            );
        }
    }

    pub fn exit_status_code(status: ExitStatus) -> i32 {
        status.code().unwrap_or_else(|| {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                128 + status.signal().unwrap_or(SIGTERM)
            }
            #[cfg(not(unix))]
            {
                1
            }
        })
    }

    pub fn make_snapshot(
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
            loaded.config().provenance().clone(),
            [0; 32],
        )
        .map_err(|error| CliError::Backend(error.to_string()))
    }

    pub fn request_backend_inner(
        message: BackendRequest,
    ) -> Result<BackendResponseMessage, CliError> {
        Self::request_with_startup_timeout(message, None)
    }

    pub fn request(request: BackendRequest) -> Result<BackendResponseMessage, CliError> {
        Self::request_once(request)
    }

    pub fn request_with_startup_timeout(
        request: BackendRequest,
        timeout_ms: Option<u64>,
    ) -> Result<BackendResponseMessage, CliError> {
        if matches!(
            &request,
            BackendRequest::Hello
                | BackendRequest::Status
                | BackendRequest::Reload { .. }
                | BackendRequest::Stop
        ) {
            return Self::request_once(request);
        }

        let deadline = StartupDeadline::new(timeout_ms)?;
        loop {
            Self::await_until(&deadline)?;
            match Self::request_once(request.clone()) {
                Ok(response)
                    if matches!(
                        response.response(),
                        BackendResponse::Error(error)
                            if error.class() == "backend_starting" && error.retryable()
                    ) => {}
                result => return result,
            }
        }
    }

    pub fn await_backend_ready(timeout_ms: Option<u64>) -> Result<BackendStatus, CliError> {
        Self::await_until(&StartupDeadline::new(timeout_ms)?)
    }

    fn await_until(deadline: &StartupDeadline) -> Result<BackendStatus, CliError> {
        let mut delay = Duration::from_millis(20);
        loop {
            let observation = match Self::request_once(BackendRequest::Status) {
                Ok(response) => match response.response() {
                    BackendResponse::Status(status) => match startup_readiness(status) {
                        StartupReadiness::Ready => return Ok(status.clone()),
                        StartupReadiness::Pending(observation) => observation,
                        StartupReadiness::Failed(error) => return Err(error),
                    },
                    BackendResponse::Error(error) => {
                        return Err(CliError::Backend(format!(
                            "{}: {}",
                            error.class(),
                            error.message()
                        )));
                    }
                    other => {
                        return Err(CliError::Backend(format!(
                            "backend returned unexpected readiness response: {other:?}"
                        )));
                    }
                },
                Err(CliError::BackendUnavailable(error)) => StartupObservation {
                    state: "unavailable".to_owned(),
                    phase: "endpoint_not_available".to_owned(),
                    transport_error: Some(error),
                },
                Err(error) => return Err(error),
            };

            let remaining = deadline.remaining();
            if remaining.is_zero() {
                let StartupObservation {
                    state,
                    phase,
                    transport_error,
                } = observation;
                let transport = transport_error
                    .map(|error| format!("; last transport error: {error}"))
                    .unwrap_or_default();
                return Err(CliError::BackendStartupTimeout(format!(
                    "control plane did not become serving; last_state={state}, startup_phase={phase}, waited_ms={}{}",
                    deadline.elapsed_ms(),
                    transport
                )));
            }
            let jitter = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|time| Duration::from_millis((time.subsec_nanos() % 11) as u64))
                .unwrap_or_default();
            thread::sleep((delay + jitter).min(remaining));
            delay = (delay * 2).min(Duration::from_millis(250));
        }
    }

    fn request_once(request: BackendRequest) -> Result<BackendResponseMessage, CliError> {
        let mut stream = BackendTransport::connect().map_err(|source| {
            CliError::BackendUnavailable(format!(
                "BACKEND-TRANSPORT-UNAVAILABLE: cannot connect to {ENDPOINT_DISPLAY}: {source}"
            ))
        })?;
        let request_id = format!(
            "cli-{}-{}",
            id(),
            REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let request = match request {
            BackendRequest::Prepare { context } => BackendRequest::Prepare {
                context: context.with_request_id(request_id.clone()),
            },
            request => request,
        };
        let message = BackendRequestMessage::new(request_id, request);
        message
            .write_to(&mut stream)
            .map_err(|error| CliError::Backend(error.to_string()))?;
        BackendResponseMessage::read_from(&mut stream).map_err(|error| match error {
            BackendProtocolError::UnsupportedVersion(version) => CliError::Backend(format!(
                "BACKEND-PROTOCOL-MISMATCH: backend response uses protocol v{version}; expected v{}",
                    DEVENV_BACKEND_PROTOCOL_VERSION
            )),
            error => CliError::Backend(error.to_string()),
        })
    }

    pub fn print_status(status: BackendStatus) -> Result<(), CliError> {
        let output = to_string(&BackendResponse::Status(status))
            .map_err(|error| CliError::Backend(error.to_string()))?;
        OutputApi::write_stdout(&format!("{output}\n"))
            .map_err(|error| CliError::Backend(error.to_string()))
    }

    pub fn print_response(response: BackendResponseMessage) -> Result<(), CliError> {
        if let BackendResponse::Error(error) = &response.response() {
            return Err(CliError::Backend(format!(
                "{}: {}",
                error.class(),
                error.message()
            )));
        }
        let output = to_string(&response.response())
            .map_err(|error| CliError::Backend(error.to_string()))?;
        OutputApi::write_stdout(&format!("{output}\n"))
            .map_err(|error| CliError::Backend(error.to_string()))
    }

    pub fn normalize_path(path: &Path) -> PathBuf {
        let mut output = PathBuf::new();
        for component in path.components() {
            match component {
                Component::RootDir | Component::Prefix(_) => output.push(component.as_os_str()),
                Component::CurDir => {}
                Component::ParentDir => {
                    output.pop();
                }
                Component::Normal(value) => output.push(value),
            }
        }
        output
    }

    pub fn plan_value(snapshot: &ConfigSnapshot) -> JsonValue {
        json!({
            "generation": snapshot.generation(),
            "config_fingerprint": Self::hex(&snapshot.config_fingerprint()),
            "profile_chain": snapshot.profile_chain(),
            "workspace_root": snapshot.workspace_root(),
            "provider_order": snapshot.provider_order(),
        })
    }

    pub fn explain_value(snapshot: &ConfigSnapshot, path: Option<&str>) -> JsonValue {
        json!({
            "generation": snapshot.generation(),
            "config_fingerprint": Self::hex(&snapshot.config_fingerprint()),
            "profile_chain": snapshot.profile_chain(),
            "path": path,
            "provenance": snapshot.source_provenance(),
        })
    }

    pub fn doctor_value(
        snapshot: &ConfigSnapshot,
        peer_uid: u32,
        allowed_peer_uid: u32,
    ) -> JsonValue {
        let workspace_ok = snapshot.workspace_root().is_dir();
        let shells = snapshot
            .shells()
            .iter()
            .map(|(id, shell)| (id.clone(), Path::new(&shell.command()).is_file()))
            .collect::<BTreeMap<_, _>>();
        let providers = snapshot
            .resolved_config()
            .providers()
            .iter()
            .map(|(id, provider)| {
                let available = if provider.executable().starts_with('/') {
                    Path::new(provider.executable()).is_file()
                } else {
                    true
                };
                (id.clone(), available)
            })
            .collect::<BTreeMap<_, _>>();
        let provider_failures = snapshot
            .resolved_config()
            .providers()
            .iter()
            .filter(|(_id, provider)| {
                provider.detect_files().is_empty()
                    && provider.missing() == MissingProviderPolicy::Error
                    && !providers.get(*_id).copied().unwrap_or(true)
            })
            .count();
        json!({
            "ok": workspace_ok && shells.values().all(|ok| *ok) && provider_failures == 0,
            "backend_transport": {
                "endpoint": ENDPOINT_DISPLAY,
                "protocol": dev_env_model::DEVENV_BACKEND_PROTOCOL_VERSION,
                "connected": true,
                "peer_uid": peer_uid,
                "allowed_peer_uid": allowed_peer_uid,
                "peer_authorized": true,
            },
            "workspace": { "path": snapshot.workspace_root(), "is_directory": workspace_ok },
            "shells": shells,
            "providers": providers,
        })
    }

    pub fn requested_identity(context: &RequestContext, peer: IdentityPeer) -> IdentityRequest {
        match &context.requested_identity() {
            IdentityRequest::User { name } => IdentityRequest::User { name: name.clone() },
            IdentityRequest::Root => IdentityRequest::Root,
            IdentityRequest::Peer if peer.uid == 0 => IdentityRequest::Root,
            IdentityRequest::Peer => IdentityRequest::Peer,
        }
    }

    pub fn provider_detect_fingerprints(
        snapshot: &ConfigSnapshot,
        context: &RuntimeContext,
    ) -> BTreeMap<String, [u8; 32]> {
        snapshot
            .provider_order()
            .iter()
            .map(|provider_id| {
                let fingerprint = snapshot
                    .resolved_config()
                    .providers()
                    .get(provider_id)
                    .and_then(|config| {
                        detect(
                            config,
                            context.workspace(),
                            context.process_environment(),
                            &SystemExecutableLocator,
                        )
                        .ok()
                    })
                    .and_then(|detection| {
                        workspace_fingerprint(context.workspace(), &detection.matched_files).ok()
                    })
                    .unwrap_or([0; 32]);
                (provider_id.clone(), fingerprint)
            })
            .collect()
    }

    pub fn build_shell_invocation(
        context: &RequestContext,
        shell_id: &str,
        config: &ShellConfig,
    ) -> BackendShellInvocation {
        let adapter = ConfiguredShellAdapter::new(shell_id);
        let invocation = if matches!(context.mode(), RequestMode::LoginShell) {
            ShellInvocation::login(context.shell_args().iter().cloned().map(Into::into))
        } else {
            ShellInvocation::interactive(context.shell_args().iter().cloned().map(Into::into))
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
                executable: PathBuf::from(&config.command()),
                args: context.shell_args().clone(),
            },
        }
    }

    pub fn hex(bytes: &[u8; 32]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[cfg(unix)]
    pub fn peer_credentials_from_current_thread() -> IdentityPeer {
        // 初始 runtime 由 backend 自身启动，不经过客户端连接授权。
        IdentityPeer {
            pid: id(),
            uid: unsafe { geteuid() },
            gid: unsafe { getegid() },
        }
    }

    #[cfg(target_os = "linux")]
    pub fn peer_credentials(stream: &UnixStream) -> io::Result<IdentityPeer> {
        let credentials = AbstractEndpoint::peer_credentials(stream)?;
        Ok(IdentityPeer {
            pid: credentials.pid,
            uid: credentials.uid,
            gid: credentials.gid,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn peer_credentials(_stream: &UnixStream) -> io::Result<IdentityPeer> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "dev-env backend peer authorization requires Linux SO_PEERCRED",
        ))
    }

    #[cfg(not(unix))]
    pub fn peer_credentials_from_current_thread() -> IdentityPeer {
        IdentityPeer {
            pid: id(),
            uid: 0,
            gid: 0,
        }
    }

    pub fn install_signal_handlers() {
        #[cfg(unix)]
        unsafe {
            signal(SIGTERM, signal_handler as *const () as sighandler_t);
            signal(SIGINT, signal_handler as *const () as sighandler_t);
        }
    }

    pub fn signal_requested() -> bool {
        #[cfg(unix)]
        {
            SIGNAL_REQUESTED.load(Ordering::Acquire)
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

fn startup_readiness(status: &BackendStatus) -> StartupReadiness {
    let observation = StartupObservation {
        state: format!("{:?}", status.control_plane).to_lowercase(),
        phase: status
            .startup_phase
            .clone()
            .unwrap_or_else(|| "none".to_owned()),
        transport_error: None,
    };
    match status.control_plane {
        ControlPlaneState::Serving => StartupReadiness::Ready,
        ControlPlaneState::Starting | ControlPlaneState::Reloading => {
            StartupReadiness::Pending(observation)
        }
        ControlPlaneState::Failed => {
            let detail = status
                .initial_runtime_error
                .as_deref()
                .unwrap_or("backend startup failed");
            StartupReadiness::Failed(CliError::Backend(format!(
                "backend_startup_failed: {detail}"
            )))
        }
        ControlPlaneState::Stopping => StartupReadiness::Failed(CliError::Backend(
            "backend_stopping: backend is stopping".to_owned(),
        )),
    }
}

#[cfg(unix)]
extern "C" fn signal_handler(_: c_int) {
    SIGNAL_REQUESTED.store(true, Ordering::Release);
}

#[cfg(test)]
#[test]
fn readiness_waits_only_for_transient_states_and_fails_fast_on_terminal_states() {
    use dev_env_model::InitialRuntimeState;

    fn status(control_plane: ControlPlaneState) -> BackendStatus {
        BackendStatus {
            control_plane,
            initial_runtime: InitialRuntimeState::Preparing,
            startup_phase: Some("profile_loading".to_owned()),
            initial_runtime_exit_code: None,
            initial_runtime_error: Some("intentional test failure".to_owned()),
            generation: Generation::INITIAL,
            config_fingerprint: [0; 32],
            backend_pid: 1,
            active_requests: 0,
        }
    }

    for state in [ControlPlaneState::Starting, ControlPlaneState::Reloading] {
        assert!(matches!(
            startup_readiness(&status(state)),
            StartupReadiness::Pending(observation)
                if observation.state == format!("{:?}", state).to_lowercase()
                    && observation.phase == "profile_loading"
                    && observation.transport_error.is_none()
        ));
    }
    assert!(matches!(
        startup_readiness(&status(ControlPlaneState::Serving)),
        StartupReadiness::Ready
    ));
    assert!(matches!(
        startup_readiness(&status(ControlPlaneState::Failed)),
        StartupReadiness::Failed(error) if error.to_string().contains("intentional test failure")
    ));
    assert!(matches!(
        startup_readiness(&status(ControlPlaneState::Stopping)),
        StartupReadiness::Failed(error) if error.to_string().contains("backend_stopping")
    ));
}

#[cfg(test)]
#[test]
fn default_backend_startup_timeout_is_sixty_seconds() {
    assert_eq!(DEFAULT_STARTUP_TIMEOUT_MS, 60_000);
}
