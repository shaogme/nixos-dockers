use crate::protocol::{
    read_message_until, write_message_until, BackendError, BackendStatus, ClientMessage,
    ClientRequest, PlanPage, PreparedHandoff as WirePreparedHandoff, ProtocolError, ReceiptSummary,
    ServerMessage, ServerResponse, MAX_REQUEST_FRAME_BYTES, MAX_RESPONSE_FRAME_BYTES,
    PROTOCOL_VERSION,
};
use crate::socket::{peer_credentials, validate_socket_path};
use container_init_core::{HandoffCommand, IdentitySource, ResolvedIdentity, WorkspaceStatus};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct BackendClient {
    socket_path: PathBuf,
    timeout: Duration,
}

/// Product-facing prepared request returned after validating the wire DTO.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedHandoff {
    pub command: HandoffCommand,
    pub cwd: PathBuf,
    pub login_environment: BTreeMap<String, String>,
    pub identity: ResolvedIdentity,
    pub supplemental_groups: Vec<u32>,
    pub root_service: bool,
    pub receipt_summary: ReceiptSummary,
}

#[derive(Debug)]
pub enum BackendClientError {
    Io(io::Error),
    Protocol(ProtocolError),
    Backend(BackendError),
    UnexpectedResponse,
    TimedOut,
    OutcomeUnknown,
}

impl std::fmt::Display for BackendClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "backend connection failed: {error}"),
            Self::Protocol(error) => error.fmt(f),
            Self::Backend(error) => write!(f, "backend {}: {}", error.class, error.message),
            Self::UnexpectedResponse => f.write_str("backend returned an unexpected response"),
            Self::TimedOut => f.write_str("backend request exceeded its deadline"),
            Self::OutcomeUnknown => f.write_str(
                "backend execution may have started, but its result is unknown; the request was not retried",
            ),
        }
    }
}

impl std::error::Error for BackendClientError {}

impl From<io::Error> for BackendClientError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ProtocolError> for BackendClientError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}

impl BackendClient {
    pub fn new(path: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            socket_path: path.into(),
            timeout,
        }
    }

    pub fn status(&self) -> Result<BackendStatus, BackendClientError> {
        match self.request(ClientRequest::Status)? {
            ServerResponse::Status(status) => Ok(status),
            ServerResponse::Error(error) => Err(BackendClientError::Backend(error)),
            _ => Err(BackendClientError::UnexpectedResponse),
        }
    }

    pub fn plan(&self) -> Result<serde_json::Value, BackendClientError> {
        let deadline = self.new_deadline()?;
        let mut snapshot_id = None;
        let mut profile = None;
        let mut actions = Vec::new();
        let mut offset = 0;
        loop {
            let response = self.request_until(
                ClientRequest::Plan {
                    snapshot_id: snapshot_id.clone(),
                    offset,
                },
                deadline,
            )?;
            let page = match response {
                ServerResponse::PlanPage(page) => page,
                ServerResponse::Error(error) => return Err(BackendClientError::Backend(error)),
                _ => return Err(BackendClientError::UnexpectedResponse),
            };
            validate_plan_page(&page, snapshot_id.as_deref(), profile.as_deref(), offset)?;
            if snapshot_id.is_none() {
                snapshot_id = Some(page.snapshot_id.clone());
                profile = Some(page.profile.clone());
            }
            actions.extend(page.actions);
            match page.next_offset {
                Some(next) => offset = next,
                None => {
                    return Ok(serde_json::json!({
                        "online": true,
                        "profile": profile.expect("the first plan page establishes a profile"),
                        "snapshot_id": snapshot_id.expect("the first plan page establishes a snapshot"),
                        "actions": actions,
                    }));
                }
            }
        }
    }

    pub fn doctor(&self) -> Result<serde_json::Value, BackendClientError> {
        match self.request(ClientRequest::Doctor)? {
            ServerResponse::Doctor(report) => Ok(report),
            ServerResponse::Error(error) => Err(BackendClientError::Backend(error)),
            _ => Err(BackendClientError::UnexpectedResponse),
        }
    }

    pub fn prepare(
        &self,
        argv: Vec<String>,
        cwd: PathBuf,
        inputs: BTreeMap<String, String>,
        ambient_environment: &BTreeMap<String, String>,
    ) -> Result<PreparedHandoff, BackendClientError> {
        let deadline = self.new_deadline()?;
        let mut delay = Duration::from_millis(10);
        loop {
            match self.prepare_once(
                argv.clone(),
                cwd.clone(),
                &inputs,
                ambient_environment,
                deadline,
            ) {
                Ok(prepared) => return Ok(prepared),
                Err(PrepareAttemptError::OutcomeUnknown) => {
                    return Err(BackendClientError::OutcomeUnknown)
                }
                Err(PrepareAttemptError::Final(error)) => return Err(error),
                Err(PrepareAttemptError::SafeToRetry(_error)) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(BackendClientError::TimedOut);
                    }
                    let sleep = delay.min(remaining);
                    thread::sleep(sleep);
                    delay = delay.saturating_mul(2).min(Duration::from_millis(100));
                }
            }
        }
    }

    fn prepare_once(
        &self,
        argv: Vec<String>,
        cwd: PathBuf,
        inputs: &BTreeMap<String, String>,
        ambient_environment: &BTreeMap<String, String>,
        deadline: Instant,
    ) -> Result<PreparedHandoff, PrepareAttemptError> {
        let mut stream = self.connect_until(deadline).map_err(safe_retry_error)?;
        let hello = ClientMessage {
            version: PROTOCOL_VERSION,
            request: ClientRequest::Hello,
        };
        write_message_until(&mut stream, &hello, deadline, MAX_REQUEST_FRAME_BYTES)
            .map_err(|failure| safe_retry_error(BackendClientError::Protocol(failure.error)))?;
        let info = match read_server_response(&mut stream, deadline) {
            Ok(ServerResponse::Hello(info)) => info,
            Ok(ServerResponse::Error(error)) if error.retryable => {
                return Err(PrepareAttemptError::SafeToRetry(
                    BackendClientError::Backend(error),
                ))
            }
            Ok(ServerResponse::Error(error)) => {
                return Err(PrepareAttemptError::Final(BackendClientError::Backend(
                    error,
                )))
            }
            Ok(_) => {
                return Err(PrepareAttemptError::Final(
                    BackendClientError::UnexpectedResponse,
                ))
            }
            Err(error) => return Err(safe_retry_error(error)),
        };
        if !matches!(info.state, crate::BackendState::Ready) {
            return Err(PrepareAttemptError::SafeToRetry(
                BackendClientError::Backend(BackendError {
                    class: "backend_not_ready".to_owned(),
                    retryable: true,
                    message: "backend is still starting".to_owned(),
                    action_id: None,
                    path: None,
                }),
            ));
        }

        let allowed_inputs = info.runtime_inputs.into_iter().collect::<BTreeSet<_>>();
        if inputs.keys().any(|name| !allowed_inputs.contains(name)) {
            return Err(PrepareAttemptError::Final(BackendClientError::Backend(
                BackendError {
                    class: "invalid_input".to_owned(),
                    retryable: false,
                    message: "input is not enabled for runtime requests".to_owned(),
                    action_id: None,
                    path: None,
                },
            )));
        }
        let allowed_environment = info.environment_names.into_iter().collect::<BTreeSet<_>>();
        let environment = ambient_environment
            .iter()
            .filter(|(name, _)| allowed_environment.contains(*name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let exec = ClientMessage {
            version: PROTOCOL_VERSION,
            request: ClientRequest::Exec {
                argv,
                cwd,
                inputs: inputs.clone(),
                environment,
            },
        };
        if let Err(failure) =
            write_message_until(&mut stream, &exec, deadline, MAX_REQUEST_FRAME_BYTES)
        {
            if failure.bytes_written == 0 {
                return Err(safe_retry_error(BackendClientError::Protocol(
                    failure.error,
                )));
            }
            return Err(PrepareAttemptError::OutcomeUnknown);
        }
        match read_server_response(&mut stream, deadline) {
            Ok(ServerResponse::Prepared(prepared)) => {
                let prepared = from_wire_prepared(prepared);
                validate_prepared(prepared).map_err(|_| PrepareAttemptError::OutcomeUnknown)
            }
            Ok(ServerResponse::Error(error)) if error.retryable => Err(
                PrepareAttemptError::SafeToRetry(BackendClientError::Backend(error)),
            ),
            Ok(ServerResponse::Error(error)) => Err(PrepareAttemptError::Final(
                BackendClientError::Backend(error),
            )),
            Ok(_) => Err(PrepareAttemptError::OutcomeUnknown),
            Err(_) => Err(PrepareAttemptError::OutcomeUnknown),
        }
    }

    fn request(&self, request: ClientRequest) -> Result<ServerResponse, BackendClientError> {
        self.request_until(request, self.new_deadline()?)
    }

    fn request_until(
        &self,
        request: ClientRequest,
        deadline: Instant,
    ) -> Result<ServerResponse, BackendClientError> {
        let mut stream = self.connect_until(deadline)?;
        let message = ClientMessage {
            version: PROTOCOL_VERSION,
            request,
        };
        write_message_until(&mut stream, &message, deadline, MAX_REQUEST_FRAME_BYTES)
            .map_err(|failure| BackendClientError::Protocol(failure.error))?;
        read_server_response(&mut stream, deadline)
    }

    fn new_deadline(&self) -> Result<Instant, BackendClientError> {
        if self.timeout.is_zero() {
            return Err(BackendClientError::TimedOut);
        }
        Instant::now()
            .checked_add(self.timeout)
            .ok_or(BackendClientError::TimedOut)
    }

    fn connect_until(&self, deadline: Instant) -> Result<UnixStream, BackendClientError> {
        if deadline <= Instant::now() {
            return Err(BackendClientError::TimedOut);
        }
        validate_socket_path(&self.socket_path)?;
        let path = self.socket_path.as_os_str().as_bytes();
        if path.is_empty() || path.contains(&0) {
            return Err(BackendClientError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend socket path is empty or contains NUL",
            )));
        }
        let mut address: libc::sockaddr_un = unsafe { mem::zeroed() };
        if path.len() >= address.sun_path.len() {
            return Err(BackendClientError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend socket path exceeds the Unix socket path limit",
            )));
        }
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (slot, byte) in address.sun_path.iter_mut().zip(path.iter().copied()) {
            *slot = byte as libc::c_char;
        }
        let fd = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        if fd < 0 {
            return Err(BackendClientError::Io(io::Error::last_os_error()));
        }
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        let address_length =
            (mem::size_of::<libc::sa_family_t>() + path.len() + 1) as libc::socklen_t;
        let result = unsafe {
            libc::connect(
                stream.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                address_length,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINPROGRESS)
                | Some(libc::EALREADY)
                | Some(libc::EAGAIN)
                | Some(libc::EINTR) => wait_connect(stream.as_raw_fd(), deadline)?,
                _ => return Err(BackendClientError::Io(error)),
            }
        }
        let peer = peer_credentials(&stream)?;
        let effective_uid = unsafe { libc::geteuid() };
        let socket_metadata = std::fs::symlink_metadata(&self.socket_path)?;
        if socket_metadata.uid() != peer.uid
            || (effective_uid == 0 && peer.uid != 0)
            || (effective_uid != 0 && peer.uid != 0 && peer.uid != effective_uid)
        {
            return Err(BackendClientError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "backend socket owner does not match its peer credentials",
            )));
        }
        Ok(stream)
    }
}

fn from_wire_prepared(prepared: WirePreparedHandoff) -> PreparedHandoff {
    let identity = prepared.identity;
    let identity = ResolvedIdentity {
        uid: identity.uid,
        gid: identity.gid,
        user: identity.user,
        home: identity.home,
        run_as_root: identity.run_as_root,
        uid_source: identity_source_from_wire(identity.uid_source),
        gid_source: identity_source_from_wire(identity.gid_source),
        workspace: match identity.workspace {
            container_init_protocol::WorkspaceStatus::Mounted => WorkspaceStatus::Mounted,
            container_init_protocol::WorkspaceStatus::NotMounted => WorkspaceStatus::NotMounted,
            container_init_protocol::WorkspaceStatus::Unavailable => WorkspaceStatus::Unavailable,
        },
    };
    PreparedHandoff {
        command: HandoffCommand {
            program: prepared.command.program,
            args: prepared.command.args,
        },
        cwd: prepared.cwd,
        login_environment: prepared.login_environment,
        identity,
        supplemental_groups: prepared.supplemental_groups,
        root_service: prepared.root_service,
        receipt_summary: prepared.receipt_summary,
    }
}

fn identity_source_from_wire(source: container_init_protocol::IdentitySource) -> IdentitySource {
    match source {
        container_init_protocol::IdentitySource::RunAsRoot => IdentitySource::RunAsRoot,
        container_init_protocol::IdentitySource::ExplicitHost => IdentitySource::ExplicitHost,
        container_init_protocol::IdentitySource::ExplicitContainer => {
            IdentitySource::ExplicitContainer
        }
        container_init_protocol::IdentitySource::WorkspaceMount => IdentitySource::WorkspaceMount,
        container_init_protocol::IdentitySource::ProfileDefault => IdentitySource::ProfileDefault,
        container_init_protocol::IdentitySource::Current => IdentitySource::Current,
    }
}

#[derive(Debug)]
enum PrepareAttemptError {
    SafeToRetry(BackendClientError),
    Final(BackendClientError),
    OutcomeUnknown,
}

fn safe_retry_error(error: BackendClientError) -> PrepareAttemptError {
    match &error {
        BackendClientError::Protocol(ProtocolError::InvalidFrame(_))
        | BackendClientError::Protocol(ProtocolError::UnsupportedVersion(_))
        | BackendClientError::Protocol(ProtocolError::FrameTooLarge { .. })
        | BackendClientError::UnexpectedResponse
        | BackendClientError::Backend(_) => PrepareAttemptError::Final(error),
        BackendClientError::Protocol(ProtocolError::Io(_))
        | BackendClientError::Io(_)
        | BackendClientError::TimedOut => PrepareAttemptError::SafeToRetry(error),
        BackendClientError::OutcomeUnknown => PrepareAttemptError::OutcomeUnknown,
    }
}

fn read_server_response(
    stream: &mut UnixStream,
    deadline: Instant,
) -> Result<ServerResponse, BackendClientError> {
    let response: ServerMessage = read_message_until(stream, deadline, MAX_RESPONSE_FRAME_BYTES)?;
    if response.version != PROTOCOL_VERSION {
        return Err(BackendClientError::UnexpectedResponse);
    }
    Ok(response.response)
}

fn validate_plan_page(
    page: &PlanPage,
    expected_snapshot: Option<&str>,
    expected_profile: Option<&str>,
    offset: usize,
) -> Result<(), BackendClientError> {
    let end = offset.saturating_add(page.actions.len());
    if !page.online
        || page.offset != offset
        || page.snapshot_id.is_empty()
        || page.snapshot_id.len() > 128
        || expected_snapshot.is_some_and(|expected| expected != page.snapshot_id)
        || expected_profile.is_some_and(|expected| expected != page.profile)
        || page.total_actions < end
        || page
            .next_offset
            .is_some_and(|next| next != end || next <= offset)
        || (page.next_offset.is_none() && end != page.total_actions)
        || (page.next_offset.is_some() && page.actions.is_empty())
    {
        return Err(BackendClientError::UnexpectedResponse);
    }
    Ok(())
}

fn wait_connect(fd: libc::c_int, deadline: Instant) -> Result<(), BackendClientError> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(BackendClientError::TimedOut);
        }
        let millis = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if result > 0 {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(BackendClientError::Io(io::Error::from_raw_os_error(
                    libc::EBADF,
                )));
            }
            let mut socket_error: libc::c_int = 0;
            let mut length = mem::size_of_val(&socket_error) as libc::socklen_t;
            if unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    (&mut socket_error as *mut libc::c_int).cast(),
                    &mut length,
                )
            } != 0
            {
                return Err(BackendClientError::Io(io::Error::last_os_error()));
            }
            if socket_error == 0 {
                return Ok(());
            }
            return Err(BackendClientError::Io(io::Error::from_raw_os_error(
                socket_error,
            )));
        }
        if result == 0 {
            return Err(BackendClientError::TimedOut);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(BackendClientError::Io(error));
        }
    }
}

fn validate_prepared(prepared: PreparedHandoff) -> Result<PreparedHandoff, BackendClientError> {
    let uid = unsafe { libc::geteuid() };
    let expected_environment = ["HOME", "USER", "LOGNAME"];
    let login_environment_valid = prepared.login_environment.len() == expected_environment.len()
        && expected_environment.iter().all(|name| {
            prepared.login_environment.get(*name).is_some_and(|value| {
                !value.is_empty() && !value.contains('\0') && !value.contains('\n')
            })
        });
    let home_matches = prepared.login_environment.get("HOME").is_some_and(|home| {
        home.starts_with('/') && home == prepared.identity.home.to_string_lossy().as_ref()
    });
    let user_matches = prepared
        .login_environment
        .get("USER")
        .is_some_and(|user| user == &prepared.identity.user);
    let logname_matches = prepared
        .login_environment
        .get("LOGNAME")
        .is_some_and(|user| user == &prepared.identity.user);
    if !prepared.command.program.is_absolute()
        || prepared
            .command
            .args
            .iter()
            .any(|argument| argument.contains('\0'))
        || !prepared.cwd.is_absolute()
        || prepared
            .cwd
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        || !prepared.cwd.is_dir()
        || !login_environment_valid
        || (!prepared.root_service && (!home_matches || !user_matches || !logname_matches))
        || (prepared.root_service
            && (!prepared
                .login_environment
                .get("HOME")
                .is_some_and(|value| value == "/root")
                || !prepared
                    .login_environment
                    .get("USER")
                    .is_some_and(|value| value == "root")
                || !prepared
                    .login_environment
                    .get("LOGNAME")
                    .is_some_and(|value| value == "root")))
        || !prepared.identity.home.is_absolute()
        || prepared.identity.user.contains('\0')
        || prepared.supplemental_groups.len() > 4096
        || (!prepared.root_service
            && !prepared
                .supplemental_groups
                .contains(&prepared.identity.gid))
        || prepared
            .supplemental_groups
            .windows(2)
            .any(|groups| groups[0] >= groups[1])
        || (uid != 0 && (prepared.identity.uid != uid || prepared.identity.run_as_root))
        || (uid != 0 && prepared.root_service)
        || prepared.receipt_summary.action_count > 65_536
        || prepared.receipt_summary.warning_count > prepared.receipt_summary.action_count
    {
        return Err(BackendClientError::UnexpectedResponse);
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::{BackendClient, BackendClientError};
    use crate::protocol::{
        write_message, BackendError, ClientMessage, ServerMessage, ServerResponse, PROTOCOL_VERSION,
    };
    use std::collections::BTreeMap;
    use std::os::unix::net::UnixListener;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn prepare_times_out_while_backend_socket_closes_connections() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("backend.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let worker = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                drop(stream);
            }
        });

        let client = BackendClient::new(&socket, Duration::from_millis(80));
        let result = client.prepare(
            vec!["/bin/true".to_owned()],
            temp.path().to_path_buf(),
            BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(matches!(result, Err(BackendClientError::TimedOut)));
        worker.join().unwrap();
    }

    #[test]
    fn prepare_does_not_retry_after_an_execution_frame_is_sent() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("backend.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = requests.clone();
        let worker = thread::spawn(move || {
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("backend accept failed: {error}"),
                }
            };
            let _: ClientMessage = crate::protocol::read_message(&mut stream).unwrap();
            write_message(
                &mut stream,
                &ServerMessage {
                    version: PROTOCOL_VERSION,
                    response: ServerResponse::Hello(crate::protocol::HelloInfo {
                        state: crate::BackendState::Ready,
                        profile: "test".into(),
                        snapshot_id: "snapshot".into(),
                        runtime_inputs: vec![],
                        environment_names: vec![],
                    }),
                },
            )
            .unwrap();
            let _: ClientMessage = crate::protocol::read_message(&mut stream).unwrap();
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            drop(stream);
            for _ in 0..20 {
                match listener.accept() {
                    Ok((_, _)) => {
                        observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        let client = BackendClient::new(&socket, Duration::from_millis(300));
        let result = client.prepare(
            vec!["/bin/true".into()],
            temp.path().to_path_buf(),
            BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(matches!(result, Err(BackendClientError::OutcomeUnknown)));
        worker.join().unwrap();
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn prepare_retries_only_a_backend_error_that_guarantees_no_dispatch() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("backend.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let worker = thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let _: ClientMessage = crate::protocol::read_message(&mut first).unwrap();
            write_message(
                &mut first,
                &ServerMessage {
                    version: PROTOCOL_VERSION,
                    response: ServerResponse::Error(BackendError {
                        class: "capacity".into(),
                        retryable: true,
                        message: "busy".into(),
                        action_id: None,
                        path: None,
                    }),
                },
            )
            .unwrap();
            let (mut second, _) = listener.accept().unwrap();
            let _: ClientMessage = crate::protocol::read_message(&mut second).unwrap();
        });
        let client = BackendClient::new(&socket, Duration::from_millis(40));
        let result = client.prepare(
            vec!["/bin/true".into()],
            temp.path().to_path_buf(),
            BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(matches!(result, Err(BackendClientError::TimedOut)));
        worker.join().unwrap();
    }
}
