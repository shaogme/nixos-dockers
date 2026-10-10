use crate::socket::BackendSocket;
use container_init_core::{HandoffCommand, IdentitySource, ResolvedIdentity, WorkspaceStatus};
use container_init_protocol::{
    read_message_until, write_message_until, BackendError, BackendState, BackendStatus,
    ClientMessage, ClientRequest, CommitResult as WireCommitResult, CwdObject,
    ExecTransactionState, IdentitySource as WireIdentitySource, PlanPage,
    PreparedHandoff as WirePreparedHandoff, ProtocolError, ReceiptSummary, ServerMessage,
    ServerResponse, WorkspaceStatus as WireWorkspaceStatus, MAX_PLAN_PAGE_ACTIONS,
    MAX_REQUEST_FRAME_BYTES, MAX_RESPONSE_FRAME_BYTES, PROTOCOL_VERSION,
};
use libc::{
    c_char, c_int, connect, getegid, geteuid, getgroups, getsockopt, gid_t, poll, pollfd,
    sa_family_t, sockaddr_un, socket, socklen_t, AF_UNIX, EAGAIN, EALREADY, EBADF, EINPROGRESS,
    EINTR, POLLNVAL, POLLOUT, SOCK_CLOEXEC, SOCK_NONBLOCK, SOCK_STREAM, SOL_SOCKET, SO_ERROR,
};
use serde_json::{self, json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, fs, io, mem,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt, net::UnixStream},
    },
    path::{Component, PathBuf},
    ptr, thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct BackendClient {
    socket_path: PathBuf,
    timeout: Duration,
}

/// 校验协议 DTO 后返回给产品层使用的已准备请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedHandoff {
    pub prepare_id: String,
    pub snapshot_id: String,
    pub command: HandoffCommand,
    pub cwd: PathBuf,
    pub cwd_object: CwdObject,
    pub login_environment: BTreeMap<String, String>,
    pub identity: ResolvedIdentity,
    pub supplemental_groups: Vec<u32>,
    pub root_service: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitResult {
    pub receipt_summary: ReceiptSummary,
}

#[derive(Clone, Debug)]
struct CallerCredentials {
    uid: u32,
    gid: u32,
    groups: Option<Vec<u32>>,
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

impl fmt::Display for BackendClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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

impl Error for BackendClientError {}

impl From<io::Error> for BackendClientError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ProtocolError> for BackendClientError {
    fn from(error: ProtocolError) -> Self {
        protocol_client_error(error)
    }
}

fn protocol_client_error(error: ProtocolError) -> BackendClientError {
    match &error {
        ProtocolError::Io(source) if source.kind() == io::ErrorKind::TimedOut => {
            BackendClientError::TimedOut
        }
        _ => BackendClientError::Protocol(error),
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
        self.status_until(self.new_deadline()?)
    }

    pub fn status_until(&self, deadline: Instant) -> Result<BackendStatus, BackendClientError> {
        match self.request_until(ClientRequest::Status, deadline)? {
            ServerResponse::Status(status) => Ok(status),
            ServerResponse::Error(error) => Err(BackendClientError::Backend(error)),
            _ => Err(BackendClientError::UnexpectedResponse),
        }
    }

    pub fn plan(&self) -> Result<Value, BackendClientError> {
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
                    return Ok(json!({
                        "online": true,
                        "profile": profile.expect("the first plan page establishes a profile"),
                        "snapshot_id": snapshot_id.expect("the first plan page establishes a snapshot"),
                        "actions": actions,
                    }));
                }
            }
        }
    }

    pub fn doctor(&self) -> Result<Value, BackendClientError> {
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
        self.prepare_until(argv, cwd, inputs, ambient_environment, self.new_deadline()?)
    }

    pub fn prepare_until(
        &self,
        argv: Vec<String>,
        cwd: PathBuf,
        inputs: BTreeMap<String, String>,
        ambient_environment: &BTreeMap<String, String>,
        deadline: Instant,
    ) -> Result<PreparedHandoff, BackendClientError> {
        container_init_protocol::validate_argv(&argv).map_err(BackendClientError::Protocol)?;
        let caller = caller_credentials()?;
        let mut delay = Duration::from_millis(10);
        loop {
            if Instant::now() >= deadline {
                return Err(BackendClientError::TimedOut);
            }
            match self.prepare_once_until(
                argv.clone(),
                cwd.clone(),
                inputs.clone(),
                ambient_environment,
                deadline,
                &caller,
            ) {
                Err(BackendClientError::Backend(error)) if error.retryable => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Err(BackendClientError::TimedOut);
                    }
                    thread::sleep(delay.min(remaining));
                    delay = delay.saturating_mul(2).min(Duration::from_millis(100));
                }
                result => return result,
            }
        }
    }

    fn prepare_once_until(
        &self,
        argv: Vec<String>,
        cwd: PathBuf,
        inputs: BTreeMap<String, String>,
        ambient_environment: &BTreeMap<String, String>,
        deadline: Instant,
        caller: &CallerCredentials,
    ) -> Result<PreparedHandoff, BackendClientError> {
        let mut stream = self.connect_until(deadline)?;
        let hello = ClientMessage {
            version: PROTOCOL_VERSION,
            request: ClientRequest::Hello,
        };
        write_message_until(&mut stream, &hello, deadline, MAX_REQUEST_FRAME_BYTES)
            .map_err(|failure| protocol_client_error(failure.error))?;
        let info = match read_server_response(&mut stream, deadline)? {
            ServerResponse::Hello(info) => info,
            ServerResponse::Error(error) => return Err(BackendClientError::Backend(error)),
            _ => return Err(BackendClientError::UnexpectedResponse),
        };
        if !matches!(info.state, BackendState::Ready) {
            return Err(BackendClientError::Backend(BackendError {
                class: "backend_not_ready".to_owned(),
                retryable: true,
                message: "backend is still starting".to_owned(),
                action_id: None,
                path: None,
            }));
        }
        if info.limits != container_init_protocol::ProtocolLimits::current() {
            return Err(BackendClientError::Protocol(
                ProtocolError::IncompatibleLimits,
            ));
        }

        let expected_snapshot = info.snapshot_id.clone();
        let allowed_inputs = info.runtime_inputs.into_iter().collect::<BTreeSet<_>>();
        if inputs.keys().any(|name| !allowed_inputs.contains(name)) {
            return Err(BackendClientError::Backend(BackendError {
                class: "invalid_input".to_owned(),
                retryable: false,
                message: "input is not enabled for runtime requests".to_owned(),
                action_id: None,
                path: None,
            }));
        }
        let allowed_environment = info.environment_names.into_iter().collect::<BTreeSet<_>>();
        let environment = ambient_environment
            .iter()
            .filter(|(name, _)| allowed_environment.contains(*name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let exec = ClientMessage {
            version: PROTOCOL_VERSION,
            request: ClientRequest::PrepareExec {
                argv,
                cwd,
                inputs: inputs.clone(),
                environment,
                request_budget_ms: deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .clamp(1, 300_000) as u64,
            },
        };
        container_init_protocol::validate_message(&exec).map_err(BackendClientError::Protocol)?;
        write_message_until(&mut stream, &exec, deadline, MAX_REQUEST_FRAME_BYTES)
            .map_err(|failure| protocol_client_error(failure.error))?;
        match read_server_response(&mut stream, deadline) {
            Ok(ServerResponse::PreparedExec(prepared)) => {
                let prepared = from_wire_prepared(prepared);
                let prepare_id = prepared.prepare_id.clone();
                let validated = validate_prepared(prepared, caller).and_then(|prepared| {
                    if prepared.snapshot_id == expected_snapshot {
                        Ok(prepared)
                    } else {
                        Err(BackendClientError::UnexpectedResponse)
                    }
                });
                if validated.is_err() && !prepare_id.is_empty() {
                    let _ = self.abort_until(&prepare_id, deadline);
                }
                validated
            }
            Ok(ServerResponse::Error(error)) => Err(BackendClientError::Backend(error)),
            Ok(_) => Err(BackendClientError::UnexpectedResponse),
            Err(error) => Err(error),
        }
    }

    pub fn commit_until(
        &self,
        prepare_id: &str,
        deadline: Instant,
    ) -> Result<CommitResult, BackendClientError> {
        let request = ClientRequest::CommitExec {
            prepare_id: prepare_id.to_owned(),
        };
        let first = self.request_until(request, deadline);
        match first {
            Ok(ServerResponse::CommitResult(result)) => {
                if result.prepare_id != prepare_id {
                    return Err(BackendClientError::UnexpectedResponse);
                }
                match commit_result_if_done(result) {
                    Some(result) => result,
                    None => self.poll_exec_result(prepare_id, deadline),
                }
            }
            Ok(ServerResponse::Error(error)) if error.class == "unknown_transaction" => {
                self.poll_exec_result(prepare_id, deadline)
            }
            Ok(ServerResponse::Error(error)) => Err(BackendClientError::Backend(error)),
            Ok(_) => Err(BackendClientError::UnexpectedResponse),
            Err(_) => self.poll_exec_result(prepare_id, deadline),
        }
    }

    fn poll_exec_result(
        &self,
        prepare_id: &str,
        deadline: Instant,
    ) -> Result<CommitResult, BackendClientError> {
        let mut delay = Duration::from_millis(10);
        loop {
            if Instant::now() >= deadline {
                return Err(BackendClientError::OutcomeUnknown);
            }
            match self.get_exec_result_until(prepare_id, deadline) {
                Ok(result) => match commit_result_if_done(result) {
                    Some(result) => return result,
                    None => {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            return Err(BackendClientError::OutcomeUnknown);
                        }
                        thread::sleep(delay.min(remaining));
                        delay = delay.saturating_mul(2).min(Duration::from_millis(100));
                    }
                },
                Err(error) if transport_failure(&error) => {
                    return Err(BackendClientError::OutcomeUnknown)
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn abort_until(
        &self,
        prepare_id: &str,
        deadline: Instant,
    ) -> Result<(), BackendClientError> {
        match self.request_until(
            ClientRequest::AbortExec {
                prepare_id: prepare_id.to_owned(),
            },
            deadline,
        )? {
            ServerResponse::CommitResult(result)
                if result.prepare_id == prepare_id
                    && result.state == ExecTransactionState::Aborted =>
            {
                Ok(())
            }
            ServerResponse::Error(error) => Err(BackendClientError::Backend(error)),
            _ => Err(BackendClientError::UnexpectedResponse),
        }
    }

    pub fn get_exec_result_until(
        &self,
        prepare_id: &str,
        deadline: Instant,
    ) -> Result<WireCommitResult, BackendClientError> {
        match self.request_until(
            ClientRequest::GetExecResult {
                prepare_id: prepare_id.to_owned(),
            },
            deadline,
        )? {
            ServerResponse::CommitResult(result) if result.prepare_id == prepare_id => Ok(result),
            ServerResponse::CommitResult(_) => Err(BackendClientError::UnexpectedResponse),
            ServerResponse::Error(error) if error.class == "unknown_transaction" => {
                Err(BackendClientError::OutcomeUnknown)
            }
            ServerResponse::Error(error) => Err(BackendClientError::Backend(error)),
            _ => Err(BackendClientError::UnexpectedResponse),
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
            .map_err(|failure| protocol_client_error(failure.error))?;
        read_server_response(&mut stream, deadline)
    }

    pub fn deadline(&self) -> Result<Instant, BackendClientError> {
        if self.timeout.is_zero() {
            return Err(BackendClientError::TimedOut);
        }
        Instant::now()
            .checked_add(self.timeout)
            .ok_or(BackendClientError::TimedOut)
    }

    fn new_deadline(&self) -> Result<Instant, BackendClientError> {
        self.deadline()
    }

    fn connect_until(&self, deadline: Instant) -> Result<UnixStream, BackendClientError> {
        if deadline <= Instant::now() {
            return Err(BackendClientError::TimedOut);
        }
        BackendSocket::validate_socket_path(&self.socket_path)?;
        let path = self.socket_path.as_os_str().as_bytes();
        if path.is_empty() || path.contains(&0) {
            return Err(BackendClientError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend socket path is empty or contains NUL",
            )));
        }
        let mut address: sockaddr_un = unsafe { mem::zeroed() };
        if path.len() >= address.sun_path.len() {
            return Err(BackendClientError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend socket path exceeds the Unix socket path limit",
            )));
        }
        address.sun_family = AF_UNIX as sa_family_t;
        for (slot, byte) in address.sun_path.iter_mut().zip(path.iter().copied()) {
            *slot = byte as c_char;
        }
        let fd = unsafe { socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0) };
        if fd < 0 {
            return Err(BackendClientError::Io(io::Error::last_os_error()));
        }
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        let address_length = (mem::size_of::<sa_family_t>() + path.len() + 1) as socklen_t;
        let result = unsafe {
            connect(
                stream.as_raw_fd(),
                (&address as *const sockaddr_un).cast(),
                address_length,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(EINPROGRESS) | Some(EALREADY) | Some(EAGAIN) | Some(EINTR) => {
                    wait_connect(stream.as_raw_fd(), deadline)?
                }
                _ => return Err(BackendClientError::Io(error)),
            }
        }
        let peer = BackendSocket::peer_credentials(&stream)?;
        let effective_uid = unsafe { geteuid() };
        let socket_metadata = fs::symlink_metadata(&self.socket_path)?;
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
            WireWorkspaceStatus::Mounted => WorkspaceStatus::Mounted,
            WireWorkspaceStatus::NotMounted => WorkspaceStatus::NotMounted,
            WireWorkspaceStatus::Unavailable => WorkspaceStatus::Unavailable,
        },
    };
    PreparedHandoff {
        prepare_id: prepared.prepare_id,
        snapshot_id: prepared.snapshot_id,
        command: HandoffCommand {
            program: prepared.command.program,
            args: prepared.command.args,
        },
        cwd: prepared.cwd,
        cwd_object: prepared.cwd_object,
        login_environment: prepared.login_environment,
        identity,
        supplemental_groups: prepared.supplemental_groups,
        root_service: prepared.root_service,
    }
}

fn commit_result_if_done(
    result: WireCommitResult,
) -> Option<Result<CommitResult, BackendClientError>> {
    match result.state {
        ExecTransactionState::Committed => Some(
            result
                .receipt_summary
                .map(|receipt_summary| CommitResult { receipt_summary })
                .ok_or(BackendClientError::UnexpectedResponse),
        ),
        ExecTransactionState::Failed => Some(Err(BackendClientError::Backend(
            result.error.unwrap_or(BackendError {
                class: "commit_failed".to_owned(),
                retryable: false,
                message: "backend request plan failed".to_owned(),
                action_id: None,
                path: None,
            }),
        ))),
        ExecTransactionState::OutcomeUnknown => Some(Err(BackendClientError::OutcomeUnknown)),
        ExecTransactionState::Prepared | ExecTransactionState::Committing => None,
        ExecTransactionState::Aborted => Some(Err(BackendClientError::Backend(BackendError {
            class: "transaction_aborted".to_owned(),
            retryable: false,
            message: "prepared request was aborted before commit".to_owned(),
            action_id: None,
            path: None,
        }))),
    }
}

fn transport_failure(error: &BackendClientError) -> bool {
    matches!(
        error,
        BackendClientError::Io(_)
            | BackendClientError::Protocol(ProtocolError::Io(_))
            | BackendClientError::TimedOut
    )
}

fn identity_source_from_wire(source: WireIdentitySource) -> IdentitySource {
    match source {
        WireIdentitySource::RunAsRoot => IdentitySource::RunAsRoot,
        WireIdentitySource::ExplicitHost => IdentitySource::ExplicitHost,
        WireIdentitySource::ExplicitContainer => IdentitySource::ExplicitContainer,
        WireIdentitySource::WorkspaceMount => IdentitySource::WorkspaceMount,
        WireIdentitySource::ProfileDefault => IdentitySource::ProfileDefault,
        WireIdentitySource::Current => IdentitySource::Current,
    }
}

fn read_server_response(
    stream: &mut UnixStream,
    deadline: Instant,
) -> Result<ServerResponse, BackendClientError> {
    let response: ServerMessage = read_message_until(stream, deadline, MAX_RESPONSE_FRAME_BYTES)?;
    if response.version != PROTOCOL_VERSION {
        return Err(BackendClientError::Protocol(
            ProtocolError::UnsupportedVersion(response.version),
        ));
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
    if page.actions.len() > MAX_PLAN_PAGE_ACTIONS
        || !page.online
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

fn wait_connect(fd: c_int, deadline: Instant) -> Result<(), BackendClientError> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(BackendClientError::TimedOut);
        }
        let millis = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut descriptor = pollfd {
            fd,
            events: POLLOUT,
            revents: 0,
        };
        let result = unsafe { poll(&mut descriptor, 1, millis) };
        if result > 0 {
            if descriptor.revents & POLLNVAL != 0 {
                return Err(BackendClientError::Io(io::Error::from_raw_os_error(EBADF)));
            }
            let mut socket_error: c_int = 0;
            let mut length = mem::size_of_val(&socket_error) as socklen_t;
            if unsafe {
                getsockopt(
                    fd,
                    SOL_SOCKET,
                    SO_ERROR,
                    (&mut socket_error as *mut c_int).cast(),
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

fn validate_prepared(
    prepared: PreparedHandoff,
    caller: &CallerCredentials,
) -> Result<PreparedHandoff, BackendClientError> {
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
            .any(|component| matches!(component, Component::ParentDir))
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
        || prepared.supplemental_groups.len() > container_init_protocol::max_supplementary_groups()
        || (!prepared.root_service
            && !prepared
                .supplemental_groups
                .contains(&prepared.identity.gid))
        || prepared
            .supplemental_groups
            .windows(2)
            .any(|groups| groups[0] >= groups[1])
        || !handoff_credentials_match(
            caller,
            &prepared.identity,
            &prepared.supplemental_groups,
            prepared.root_service,
        )
        || prepared.prepare_id.len() != 48
        || !prepared
            .prepare_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || prepared.snapshot_id.is_empty()
        || prepared.snapshot_id.len() > 128
        || prepared.cwd_object.inode == 0
    {
        return Err(BackendClientError::UnexpectedResponse);
    }
    Ok(prepared)
}

fn handoff_credentials_match(
    caller: &CallerCredentials,
    identity: &ResolvedIdentity,
    supplemental_groups: &[u32],
    root_service: bool,
) -> bool {
    caller.uid == 0
        || (!root_service
            && !identity.run_as_root
            && identity.uid == caller.uid
            && identity.gid == caller.gid
            && caller.groups.as_deref() == Some(supplemental_groups))
}

fn caller_credentials() -> Result<CallerCredentials, BackendClientError> {
    let uid = unsafe { geteuid() };
    let gid = unsafe { getegid() };
    let groups = if uid == 0 {
        None
    } else {
        Some(current_groups()?)
    };
    Ok(CallerCredentials { uid, gid, groups })
}

fn current_groups() -> Result<Vec<u32>, BackendClientError> {
    let count = unsafe { getgroups(0, ptr::null_mut()) };
    if count < 0 {
        return Err(BackendClientError::Io(io::Error::last_os_error()));
    }
    if count as usize > container_init_protocol::max_supplementary_groups() {
        return Err(BackendClientError::Protocol(ProtocolError::invalid_frame(
            "caller supplementary groups exceed the platform limit",
        )));
    }
    let mut groups = vec![0 as gid_t; count as usize];
    let result = unsafe { getgroups(count, groups.as_mut_ptr()) };
    if result < 0 {
        return Err(BackendClientError::Io(io::Error::last_os_error()));
    }
    let mut groups = groups[..result as usize].to_vec();
    groups.push(unsafe { getegid() });
    groups.sort_unstable();
    groups.dedup();
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::{handoff_credentials_match, BackendClient, BackendClientError, CallerCredentials};
    use container_init_core::{IdentitySource, ResolvedIdentity, WorkspaceStatus};
    use container_init_protocol::{
        read_message, write_message, BackendError, BackendState, ClientMessage, ClientRequest,
        CommitResult as WireCommitResult, ExecTransactionState, HelloInfo, ReceiptSummary,
        ServerMessage, ServerResponse, PROTOCOL_VERSION,
    };
    use std::{
        collections::BTreeMap,
        io,
        os::unix::net::UnixListener,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        thread,
        time::{Duration, Instant},
    };
    use tempfile::TempDir;

    #[test]
    fn non_root_prepare_requires_matching_uid_gid_and_complete_groups() {
        let identity = ResolvedIdentity {
            uid: 1000,
            gid: 100,
            user: "dev".to_owned(),
            home: "/home/dev".into(),
            run_as_root: false,
            uid_source: IdentitySource::Current,
            gid_source: IdentitySource::Current,
            workspace: WorkspaceStatus::Unavailable,
        };
        let caller = CallerCredentials {
            uid: 1000,
            gid: 100,
            groups: Some(vec![100, 200]),
        };
        assert!(handoff_credentials_match(
            &caller,
            &identity,
            &[100, 200],
            false
        ));

        let mut wrong_uid = caller.clone();
        wrong_uid.uid = 1001;
        assert!(!handoff_credentials_match(
            &wrong_uid,
            &identity,
            &[100, 200],
            false
        ));

        let mut wrong_gid = caller.clone();
        wrong_gid.gid = 101;
        assert!(!handoff_credentials_match(
            &wrong_gid,
            &identity,
            &[100, 200],
            false
        ));

        assert!(!handoff_credentials_match(
            &caller,
            &identity,
            &[100],
            false
        ));
        assert!(!handoff_credentials_match(
            &caller,
            &identity,
            &[100, 200],
            true
        ));
    }

    #[test]
    fn prepare_timeout_includes_the_prepare_exchange() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("backend.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _: ClientMessage = read_message(&mut stream).unwrap();
            write_message(
                &mut stream,
                &ServerMessage {
                    version: PROTOCOL_VERSION,
                    response: ServerResponse::Hello(HelloInfo {
                        state: BackendState::Ready,
                        profile: "test".into(),
                        snapshot_id: "snapshot".into(),
                        runtime_inputs: vec![],
                        environment_names: vec![],
                        limits: container_init_protocol::ProtocolLimits::current(),
                    }),
                },
            )
            .unwrap();
            let _: ClientMessage = read_message(&mut stream).unwrap();
            thread::sleep(Duration::from_millis(150));
        });

        let client = BackendClient::new(&socket, Duration::from_millis(80));
        let result = client.prepare(
            vec!["/bin/true".to_owned()],
            temp.path().to_path_buf(),
            BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(
            matches!(result, Err(BackendClientError::TimedOut)),
            "{result:?}"
        );
        worker.join().unwrap();
    }

    #[test]
    fn status_read_uses_the_callers_short_absolute_deadline() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("backend.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _: ClientMessage = read_message(&mut stream).unwrap();
            thread::sleep(Duration::from_millis(120));
        });
        let client = BackendClient::new(&socket, Duration::from_millis(500));
        let started = Instant::now();
        let result = client.status_until(started + Duration::from_millis(50));
        assert!(matches!(result, Err(BackendClientError::TimedOut)));
        assert!(started.elapsed() < Duration::from_millis(100));
        worker.join().unwrap();
    }

    #[test]
    fn commit_queries_the_original_token_when_the_first_reply_is_still_committing() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("backend.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let worker = thread::spawn(move || {
            let (mut commit, _) = listener.accept().unwrap();
            let message: ClientMessage = read_message(&mut commit).unwrap();
            assert!(matches!(
                message.request,
                ClientRequest::CommitExec { ref prepare_id } if prepare_id == "aabb"
            ));
            write_message(
                &mut commit,
                &ServerMessage {
                    version: PROTOCOL_VERSION,
                    response: ServerResponse::CommitResult(WireCommitResult {
                        prepare_id: "aabb".to_owned(),
                        state: ExecTransactionState::Committing,
                        receipt_summary: None,
                        error: None,
                    }),
                },
            )
            .unwrap();

            let (mut query, _) = listener.accept().unwrap();
            let message: ClientMessage = read_message(&mut query).unwrap();
            assert!(matches!(
                message.request,
                ClientRequest::GetExecResult { ref prepare_id } if prepare_id == "aabb"
            ));
            write_message(
                &mut query,
                &ServerMessage {
                    version: PROTOCOL_VERSION,
                    response: ServerResponse::CommitResult(WireCommitResult {
                        prepare_id: "aabb".to_owned(),
                        state: ExecTransactionState::Committed,
                        receipt_summary: Some(ReceiptSummary {
                            succeeded: true,
                            action_count: 1,
                            warning_count: 0,
                        }),
                        error: None,
                    }),
                },
            )
            .unwrap();
        });
        let client = BackendClient::new(&socket, Duration::from_secs(1));
        let result = client
            .commit_until("aabb", Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert!(result.receipt_summary.succeeded);
        assert_eq!(result.receipt_summary.action_count, 1);
        worker.join().unwrap();
    }

    #[test]
    fn prepare_does_not_retry_after_a_prepare_frame_is_sent() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("backend.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let worker = thread::spawn(move || {
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("backend accept failed: {error}"),
                }
            };
            let _: ClientMessage = read_message(&mut stream).unwrap();
            write_message(
                &mut stream,
                &ServerMessage {
                    version: PROTOCOL_VERSION,
                    response: ServerResponse::Hello(HelloInfo {
                        state: BackendState::Ready,
                        profile: "test".into(),
                        snapshot_id: "snapshot".into(),
                        runtime_inputs: vec![],
                        environment_names: vec![],
                        limits: container_init_protocol::ProtocolLimits::current(),
                    }),
                },
            )
            .unwrap();
            let _: ClientMessage = read_message(&mut stream).unwrap();
            observed.fetch_add(1, Ordering::SeqCst);
            drop(stream);
            for _ in 0..20 {
                match listener.accept() {
                    Ok((_, _)) => {
                        observed.fetch_add(1, Ordering::SeqCst);
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
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
        assert!(result.is_err());
        worker.join().unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn prepare_retries_only_a_backend_error_that_guarantees_no_dispatch() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("backend.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let worker = thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let _: ClientMessage = read_message(&mut first).unwrap();
            write_message(
                &mut first,
                &ServerMessage {
                    version: PROTOCOL_VERSION,
                    response: ServerResponse::Hello(HelloInfo {
                        state: BackendState::Ready,
                        profile: "test".into(),
                        snapshot_id: "snapshot".into(),
                        runtime_inputs: vec![],
                        environment_names: vec![],
                        limits: container_init_protocol::ProtocolLimits::current(),
                    }),
                },
            )
            .unwrap();
            let _: ClientMessage = read_message(&mut first).unwrap();
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
            let _: ClientMessage = read_message(&mut second).unwrap();
            write_message(
                &mut second,
                &ServerMessage {
                    version: PROTOCOL_VERSION,
                    response: ServerResponse::Hello(HelloInfo {
                        state: BackendState::Ready,
                        profile: "test".into(),
                        snapshot_id: "snapshot".into(),
                        runtime_inputs: vec![],
                        environment_names: vec![],
                        limits: container_init_protocol::ProtocolLimits::current(),
                    }),
                },
            )
            .unwrap();
            let _: ClientMessage = read_message(&mut second).unwrap();
            thread::sleep(Duration::from_millis(80));
        });
        let client = BackendClient::new(&socket, Duration::from_millis(40));
        let result = client.prepare(
            vec!["/bin/true".into()],
            temp.path().to_path_buf(),
            BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(
            matches!(result, Err(BackendClientError::TimedOut)),
            "{result:?}"
        );
        worker.join().unwrap();
    }
}
