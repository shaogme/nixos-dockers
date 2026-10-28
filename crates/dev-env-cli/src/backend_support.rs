use {
    crate::{CliError, LoadedConfig, OutputApi},
    dev_env_core::RuntimeContext,
    dev_env_model::{
        BackendError, BackendProtocolError, BackendRequest, BackendRequestMessage, BackendResponse,
        BackendResponseMessage, ConfigSnapshot, Generation, IdentityPeer, IdentityRequest,
        MissingProviderPolicy, PreparedResponse, RequestContext, RequestMode, ShellConfig,
        ShellInvocation as BackendShellInvocation, DEVENV_BACKEND_PROTOCOL_VERSION,
    },
    dev_env_provider::{detect, workspace_fingerprint, SystemExecutableLocator},
    dev_env_shell::{build_invocation, ConfiguredShellAdapter, ShellInvocation},
    libc::{
        bind, c_char, close, connect, fchmod, fchown, fstat, listen, open, openat, sa_family_t,
        sockaddr_un, stat, AF_UNIX, O_CLOEXEC, O_CREAT, O_DIRECTORY, O_NOFOLLOW, O_RDONLY, O_RDWR,
        SOCK_CLOEXEC, SOCK_STREAM, S_IFMT, S_IFREG,
    },
    serde_json::{json, to_string, Value as JsonValue},
    std::{
        collections::BTreeMap,
        env,
        ffi::{CStr, CString},
        fs::{self, File},
        io::{self, Read},
        mem::{size_of_val, zeroed},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::net::{UnixListener, UnixStream},
        },
        path::{Component, Path, PathBuf},
        process::{id, ExitStatus},
        ptr::copy_nonoverlapping,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::Duration,
    },
};

#[cfg(target_os = "linux")]
use {
    libc::{getsockopt, socklen_t, ucred, SOL_SOCKET, SO_PEERCRED},
    std::mem::size_of,
};

#[cfg(unix)]
use libc::{c_int, getegid, geteuid, sighandler_t, signal, socket, SIGINT, SIGTERM};

pub const BACKEND_RUNTIME_DIRECTORY: &str = "/run/dev-env";
pub const BACKEND_LOCK_NAME: &[u8] = b"backend.lock\0";
pub const BACKEND_ABSTRACT_ENDPOINT: &[u8] = b"\0dev-env.backend.v4";
pub const ENDPOINT_DISPLAY: &str = "@dev-env.backend.v4";
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
        let mut address: sockaddr_un = unsafe { zeroed() };
        let path_capacity = size_of_val(&address.sun_path);
        if endpoint.first() != Some(&0)
            || endpoint.len() > path_capacity
            || endpoint[1..].contains(&0)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "abstract backend endpoint has invalid Linux sockaddr_un encoding",
            ));
        }
        address.sun_family = AF_UNIX as sa_family_t;
        unsafe {
            copy_nonoverlapping(
                endpoint.as_ptr().cast::<c_char>(),
                address.sun_path.as_mut_ptr(),
                endpoint.len(),
            );
        }
        let address_length = (size_of::<sa_family_t>() + endpoint.len()) as socklen_t;
        Ok((address, address_length))
    }

    pub fn bind() -> io::Result<UnixListener> {
        Self::bind_endpoint(BACKEND_ABSTRACT_ENDPOINT)
    }

    pub fn bind_endpoint(endpoint: &[u8]) -> io::Result<UnixListener> {
        let (address, address_length) = Self::sockaddr_for(endpoint)?;
        let descriptor = unsafe { socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let bound = unsafe {
            bind(
                descriptor,
                (&address as *const sockaddr_un).cast(),
                address_length,
            )
        };
        if bound != 0 {
            let source = io::Error::last_os_error();
            unsafe { close(descriptor) };
            return Err(source);
        }
        if unsafe { listen(descriptor, MAX_ACTIVE_CONNECTIONS as c_int) } != 0 {
            let source = io::Error::last_os_error();
            unsafe { close(descriptor) };
            return Err(source);
        }
        Ok(unsafe { UnixListener::from_raw_fd(descriptor) })
    }

    pub fn connect() -> io::Result<UnixStream> {
        Self::connect_endpoint(BACKEND_ABSTRACT_ENDPOINT)
    }

    pub fn connect_endpoint(endpoint: &[u8]) -> io::Result<UnixStream> {
        let (address, address_length) = Self::sockaddr_for(endpoint)?;
        let descriptor = unsafe { socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let connected = unsafe {
            connect(
                descriptor,
                (&address as *const sockaddr_un).cast(),
                address_length,
            )
        };
        if connected != 0 {
            let source = io::Error::last_os_error();
            unsafe { close(descriptor) };
            return Err(source);
        }
        Ok(unsafe { UnixStream::from_raw_fd(descriptor) })
    }
}

pub enum ConnectionRequestError {
    PeerCredentials(io::Error),
    Unauthorized(IdentityPeer),
    Protocol(BackendProtocolError),
}

pub struct BackendSupport;

impl BackendSupport {
    pub fn backend_lock() -> Result<File, CliError> {
        fs::create_dir_all(BACKEND_RUNTIME_DIRECTORY).map_err(|source| {
            CliError::Backend(format!(
                "could not initialize root-managed backend runtime directory: {source}"
            ))
        })?;
        let directory_path = CString::new(BACKEND_RUNTIME_DIRECTORY)
            .expect("backend runtime directory has no embedded NUL");
        let directory_descriptor = unsafe {
            open(
                directory_path.as_ptr(),
                O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC,
            )
        };
        if directory_descriptor < 0 {
            return Err(CliError::Backend(format!(
                "could not open trusted backend runtime directory: {}",
                io::Error::last_os_error()
            )));
        }
        let directory = unsafe { File::from_raw_fd(directory_descriptor) };
        if unsafe { fchown(directory.as_raw_fd(), 0, 0) } != 0
            || unsafe { fchmod(directory.as_raw_fd(), 0o700) } != 0
        {
            return Err(CliError::Backend(format!(
                "could not secure backend runtime directory: {}",
                io::Error::last_os_error()
            )));
        }
        let lock_name = CStr::from_bytes_with_nul(BACKEND_LOCK_NAME)
            .expect("backend lock name must be NUL terminated");
        let lock_descriptor = unsafe {
            openat(
                directory.as_raw_fd(),
                lock_name.as_ptr(),
                O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW,
                0o600,
            )
        };
        if lock_descriptor < 0 {
            return Err(CliError::Backend(format!(
                "could not open trusted backend instance lock: {}",
                io::Error::last_os_error()
            )));
        }
        let lock = unsafe { File::from_raw_fd(lock_descriptor) };
        let mut metadata: stat = unsafe { zeroed() };
        if unsafe { fstat(lock.as_raw_fd(), &mut metadata) } != 0 {
            return Err(CliError::Backend(format!(
                "could not inspect backend instance lock: {}",
                io::Error::last_os_error()
            )));
        }
        if metadata.st_mode & S_IFMT != S_IFREG || metadata.st_uid != 0 || metadata.st_nlink != 1 {
            return Err(CliError::Backend(
                "backend instance lock must be a root-owned regular file with one link".to_owned(),
            ));
        }
        if unsafe { fchmod(lock.as_raw_fd(), 0o600) } != 0 {
            return Err(CliError::Backend(format!(
                "could not secure backend instance lock: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(lock)
    }

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

    pub fn prepare_inner(context: RequestContext) -> Result<PreparedResponse, CliError> {
        let response = Self::request(BackendRequest::Prepare { context })?;
        match response.response() {
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
                Ok((**prepared).clone())
            }
            BackendResponse::Error(error) => Err(CliError::Backend(format!(
                "{}: {}",
                error.class(),
                error.message()
            ))),
            other => Err(CliError::Backend(format!(
                "backend returned unexpected response: {other:?}"
            ))),
        }
    }

    pub fn request_backend_inner(
        message: BackendRequest,
    ) -> Result<BackendResponseMessage, CliError> {
        Self::request(message)
    }

    pub fn request(request: BackendRequest) -> Result<BackendResponseMessage, CliError> {
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
        let mut credentials = ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut length = size_of::<ucred>() as socklen_t;
        let result = unsafe {
            getsockopt(
                stream.as_raw_fd(),
                SOL_SOCKET,
                SO_PEERCRED,
                (&mut credentials as *mut ucred).cast(),
                &mut length,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if length as usize != size_of::<ucred>() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SO_PEERCRED returned an invalid credential size",
            ));
        }
        Ok(IdentityPeer {
            pid: credentials.pid as u32,
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

#[cfg(unix)]
extern "C" fn signal_handler(_: c_int) {
    SIGNAL_REQUESTED.store(true, Ordering::Release);
}
