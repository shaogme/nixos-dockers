use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Instant;
use unix_frame::FrameBudget;

pub use unix_frame::FramePermit;

mod client;

pub use client::{IdentityBrokerClient, IdentityBrokerError};

pub const PROTOCOL_VERSION: u16 = 4;
pub const MAX_REQUEST_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_RESPONSE_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ARGV_ITEMS: usize = 512;
pub const MAX_ARGV_ITEM_BYTES: usize = 64 * 1024;
pub const MAX_ARGV_TOTAL_BYTES: usize = 1024 * 1024;
pub const MAX_RUNTIME_INPUTS: usize = 128;
pub const MAX_ENVIRONMENT_NAMES: usize = 512;
pub const MAX_RUNTIME_VALUE_BYTES: usize = 64 * 1024;
pub const MAX_RUNTIME_VALUES_TOTAL_BYTES: usize = 512 * 1024;
pub const MAX_CWD_BYTES: usize = 4 * 1024;
pub const MAX_PLAN_PAGE_ACTIONS: usize = 256;
pub const FRAME_TIMEOUT_MS: u64 = 30_000;
pub const MAX_IN_FLIGHT_FRAME_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolLimits {
    pub request_frame_bytes: usize,
    pub response_frame_bytes: usize,
    pub argv_items: usize,
    pub argv_item_bytes: usize,
    pub argv_total_bytes: usize,
    pub runtime_inputs: usize,
    pub environment_names: usize,
    pub runtime_value_bytes: usize,
    pub runtime_values_total_bytes: usize,
    pub cwd_bytes: usize,
    pub plan_page_actions: usize,
    pub supplementary_groups: usize,
    pub frame_timeout_ms: u64,
}

impl ProtocolLimits {
    pub fn current() -> Self {
        Self {
            request_frame_bytes: MAX_REQUEST_FRAME_BYTES,
            response_frame_bytes: MAX_RESPONSE_FRAME_BYTES,
            argv_items: MAX_ARGV_ITEMS,
            argv_item_bytes: MAX_ARGV_ITEM_BYTES,
            argv_total_bytes: MAX_ARGV_TOTAL_BYTES,
            runtime_inputs: MAX_RUNTIME_INPUTS,
            environment_names: MAX_ENVIRONMENT_NAMES,
            runtime_value_bytes: MAX_RUNTIME_VALUE_BYTES,
            runtime_values_total_bytes: MAX_RUNTIME_VALUES_TOTAL_BYTES,
            cwd_bytes: MAX_CWD_BYTES,
            plan_page_actions: MAX_PLAN_PAGE_ACTIONS,
            supplementary_groups: max_supplementary_groups(),
            frame_timeout_ms: FRAME_TIMEOUT_MS,
        }
    }
}

pub fn max_supplementary_groups() -> usize {
    let platform_limit = unsafe { libc::sysconf(libc::_SC_NGROUPS_MAX) };
    if platform_limit > 0 {
        (platform_limit as usize).min(65_536)
    } else {
        65_536
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    RunAsRoot,
    ExplicitHost,
    ExplicitContainer,
    WorkspaceMount,
    ProfileDefault,
    Current,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Mounted,
    NotMounted,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedIdentity {
    pub uid: u32,
    pub gid: u32,
    pub user: String,
    pub home: PathBuf,
    pub run_as_root: bool,
    pub uid_source: IdentitySource,
    pub gid_source: IdentitySource,
    pub workspace: WorkspaceStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HandoffCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendState {
    Starting,
    Ready,
    Failed,
    Stopping,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClientMessage {
    pub version: u16,
    pub request: ClientRequest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientRequest {
    Hello,
    PrepareExec {
        argv: Vec<String>,
        cwd: PathBuf,
        #[serde(default)]
        inputs: BTreeMap<String, String>,
        #[serde(default)]
        environment: BTreeMap<String, String>,
        request_budget_ms: u64,
    },
    CommitExec {
        prepare_id: String,
    },
    AbortExec {
        prepare_id: String,
    },
    GetExecResult {
        prepare_id: String,
    },
    /// Resolve a request identity for a trusted long-lived runtime. The
    /// caller never supplies a UID/GID pair as the identity itself; the
    /// backend only forwards the authenticated client observation to the
    /// container-init broker.
    PrepareIdentity {
        cwd: PathBuf,
        #[serde(default)]
        inputs: BTreeMap<String, String>,
        #[serde(default)]
        environment: BTreeMap<String, String>,
        requested: IdentityRequest,
    },
    Status,
    Plan {
        snapshot_id: Option<String>,
        offset: usize,
    },
    Doctor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerMessage {
    pub version: u16,
    pub response: ServerResponse,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerResponse {
    Hello(HelloInfo),
    PreparedExec(PreparedHandoff),
    CommitResult(CommitResult),
    Identity(PreparedIdentity),
    Status(BackendStatus),
    PlanPage(PlanPage),
    Doctor(serde_json::Value),
    Error(BackendError),
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanPage {
    pub online: bool,
    pub profile: String,
    pub snapshot_id: String,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub total_actions: usize,
    pub actions: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum IdentityRequest {
    Peer { uid: u32, gid: u32 },
    Root,
    User { name: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedIdentity {
    pub uid: u32,
    pub gid: u32,
    pub user: String,
    pub home: PathBuf,
    #[serde(default)]
    pub supplemental_groups: Vec<u32>,
    pub run_as_root: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HelloInfo {
    pub state: BackendState,
    pub profile: String,
    pub snapshot_id: String,
    pub runtime_inputs: Vec<String>,
    pub environment_names: Vec<String>,
    pub limits: ProtocolLimits,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedHandoff {
    pub prepare_id: String,
    pub snapshot_id: String,
    pub command: HandoffCommand,
    /// Canonical path bound to `cwd_object` by one open directory descriptor.
    pub cwd: PathBuf,
    pub cwd_object: CwdObject,
    /// The complete login environment that must be applied to the handoff.
    pub login_environment: BTreeMap<String, String>,
    pub identity: ResolvedIdentity,
    pub supplemental_groups: Vec<u32>,
    pub root_service: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CwdObject {
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecTransactionState {
    Prepared,
    Committing,
    Committed,
    Aborted,
    Failed,
    OutcomeUnknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitResult {
    pub prepare_id: String,
    pub state: ExecTransactionState,
    pub receipt_summary: Option<ReceiptSummary>,
    pub error: Option<BackendError>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptSummary {
    pub succeeded: bool,
    pub action_count: usize,
    pub warning_count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendStatus {
    pub state: BackendState,
    pub profile: String,
    pub snapshot_id: String,
    pub backend_pid: u32,
    pub initial_child_pid: Option<u32>,
    pub started_unix_seconds: u64,
    pub active_connections: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendError {
    pub class: String,
    /// `true` guarantees the server has not started request side effects.
    pub retryable: bool,
    pub message: String,
    pub action_id: Option<String>,
    pub path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug)]
pub enum ProtocolError {
    Io(io::Error),
    InvalidFrame(String),
    UnsupportedVersion(u16),
    FrameTooLarge {
        length: usize,
        limit: usize,
    },
    LimitExceeded {
        class: &'static str,
        field: String,
        observed: usize,
        limit: usize,
    },
    IncompatibleLimits,
}

impl ProtocolError {
    pub fn invalid_frame(message: impl Into<String>) -> Self {
        Self::InvalidFrame(message.into())
    }

    pub fn as_io_error(&self) -> io::Error {
        match self {
            Self::Io(error) => io::Error::new(error.kind(), error.to_string()),
            Self::InvalidFrame(message) => {
                io::Error::new(io::ErrorKind::InvalidData, message.clone())
            }
            Self::UnsupportedVersion(version) => io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported protocol version {version}"),
            ),
            Self::FrameTooLarge { length, limit } => io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame length {length} exceeds the {limit} byte limit"),
            ),
            Self::LimitExceeded {
                class,
                field,
                observed,
                limit,
            } => io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{class}: {field} is {observed}; maximum is {limit}"),
            ),
            Self::IncompatibleLimits => io::Error::new(
                io::ErrorKind::InvalidData,
                "backend protocol limits are incompatible with this client build",
            ),
        }
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "protocol IO failed: {error}"),
            Self::InvalidFrame(message) => write!(formatter, "invalid protocol frame: {message}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported protocol version {version}")
            }
            Self::FrameTooLarge { length, limit } => {
                write!(
                    formatter,
                    "frame length {length} exceeds the {limit} byte limit"
                )
            }
            Self::LimitExceeded {
                class,
                field,
                observed,
                limit,
            } => write!(
                formatter,
                "{class}: {field} is {observed}; maximum is {limit}"
            ),
            Self::IncompatibleLimits => formatter
                .write_str("backend protocol limits are incompatible with this client build"),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ProtocolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug)]
pub struct FrameWriteError {
    pub error: ProtocolError,
    pub bytes_written: usize,
}

pub fn read_message<T: for<'de> Deserialize<'de>>(
    reader: &mut impl Read,
) -> Result<T, ProtocolError> {
    read_message_with_limit(reader, MAX_REQUEST_FRAME_BYTES)
}

pub fn read_message_with_limit<T: for<'de> Deserialize<'de>>(
    reader: &mut impl Read,
    limit: usize,
) -> Result<T, ProtocolError> {
    unix_frame::read_json_frame(reader, limit).map_err(map_frame_error)
}

pub fn read_message_until<T: for<'de> Deserialize<'de>>(
    stream: &mut UnixStream,
    deadline: Instant,
    limit: usize,
) -> Result<T, ProtocolError> {
    unix_frame::read_json_frame_until(stream, deadline, limit).map_err(map_frame_error)
}

pub fn read_message_until_with_budget<T: for<'de> Deserialize<'de>>(
    stream: &mut UnixStream,
    deadline: Instant,
    limit: usize,
    budget: &FrameBudget,
) -> Result<(T, FramePermit), ProtocolError> {
    unix_frame::read_json_frame_until_with_budget(stream, deadline, limit, budget)
        .map_err(map_frame_error)
}

pub fn write_message<T: Serialize>(
    writer: &mut impl Write,
    message: &T,
) -> Result<(), ProtocolError> {
    unix_frame::write_json_frame(writer, message, MAX_RESPONSE_FRAME_BYTES).map_err(map_frame_error)
}

pub fn write_message_until<T: Serialize>(
    stream: &mut UnixStream,
    message: &T,
    deadline: Instant,
    limit: usize,
) -> Result<usize, FrameWriteError> {
    unix_frame::write_json_frame_until(stream, message, deadline, limit).map_err(|error| {
        FrameWriteError {
            error: map_frame_error(error.error),
            bytes_written: error.bytes_written,
        }
    })
}

pub fn write_message_until_with_budget<T: Serialize>(
    stream: &mut UnixStream,
    message: &T,
    deadline: Instant,
    limit: usize,
    budget: &FrameBudget,
) -> Result<usize, FrameWriteError> {
    unix_frame::write_json_frame_until_with_budget(stream, message, deadline, limit, budget)
        .map_err(|error| FrameWriteError {
            error: map_frame_error(error.error),
            bytes_written: error.bytes_written,
        })
}

fn map_frame_error(error: unix_frame::FrameError) -> ProtocolError {
    match error {
        unix_frame::FrameError::Io(error) => ProtocolError::Io(error),
        unix_frame::FrameError::Json(error) => ProtocolError::invalid_frame(error.to_string()),
        unix_frame::FrameError::Empty => {
            ProtocolError::invalid_frame("frame length must be nonzero")
        }
        unix_frame::FrameError::TooLarge { length, limit } => {
            ProtocolError::FrameTooLarge { length, limit }
        }
        unix_frame::FrameError::LengthOverflow(_) => {
            ProtocolError::invalid_frame("frame length exceeds u32")
        }
    }
}

pub fn validate_message(message: &ClientMessage) -> Result<(), ProtocolError> {
    if message.version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(message.version));
    }
    let (argv, cwd, inputs, environment) = match &message.request {
        ClientRequest::PrepareExec {
            argv,
            cwd,
            inputs,
            environment,
            request_budget_ms,
        } => {
            if *request_budget_ms == 0 || *request_budget_ms > 300_000 {
                return Err(ProtocolError::invalid_frame(
                    "request budget must be between 1 and 300000 milliseconds",
                ));
            }
            (Some(argv.as_slice()), Some(cwd), inputs, environment)
        }
        ClientRequest::PrepareIdentity {
            cwd,
            inputs,
            environment,
            requested,
        } => {
            match requested {
                IdentityRequest::User { name }
                    if name.is_empty()
                        || name.len() > 32
                        || !name.chars().all(|character| {
                            character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                        }) =>
                {
                    return Err(ProtocolError::invalid_frame(
                        "identity user name is invalid",
                    ));
                }
                IdentityRequest::Peer { .. }
                | IdentityRequest::Root
                | IdentityRequest::User { .. } => {}
            }
            (None, Some(cwd), inputs, environment)
        }
        ClientRequest::Plan { snapshot_id, .. }
            if snapshot_id
                .as_ref()
                .is_some_and(|value| value.is_empty() || value.len() > 128) =>
        {
            return Err(ProtocolError::invalid_frame("plan snapshot id is invalid"));
        }
        _ => (None, None, &BTreeMap::new(), &BTreeMap::new()),
    };
    if let Some(cwd) = cwd {
        let cwd_bytes = cwd.as_os_str().as_encoded_bytes().len();
        if cwd_bytes > MAX_CWD_BYTES {
            return Err(limit_error(
                "argument_too_large",
                "cwd",
                cwd_bytes,
                MAX_CWD_BYTES,
            ));
        }
    }
    let prepare_id = match &message.request {
        ClientRequest::CommitExec { prepare_id }
        | ClientRequest::AbortExec { prepare_id }
        | ClientRequest::GetExecResult { prepare_id } => Some(prepare_id),
        _ => None,
    };
    if prepare_id.is_some_and(|prepare_id| {
        prepare_id.is_empty()
            || prepare_id.len() > 128
            || !prepare_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Err(ProtocolError::invalid_frame(
            "prepare_id must be a nonempty hexadecimal token no longer than 128 bytes",
        ));
    }
    if let Some(argv) = argv {
        validate_argv(argv)?;
    }
    validate_runtime_values(inputs, environment)?;
    Ok(())
}

pub fn validate_argv(argv: &[String]) -> Result<(), ProtocolError> {
    if argv.len() > MAX_ARGV_ITEMS {
        return Err(limit_error(
            "argv_too_large",
            "argv.items",
            argv.len(),
            MAX_ARGV_ITEMS,
        ));
    }
    let mut total = 0_usize;
    for (index, argument) in argv.iter().enumerate() {
        if argument.contains('\0') {
            return Err(ProtocolError::invalid_frame(format!(
                "argv[{index}] contains NUL"
            )));
        }
        if argument.len() > MAX_ARGV_ITEM_BYTES {
            return Err(limit_error(
                "argument_too_large",
                &format!("argv[{index}]"),
                argument.len(),
                MAX_ARGV_ITEM_BYTES,
            ));
        }
        total = total.checked_add(argument.len()).ok_or_else(|| {
            limit_error(
                "argv_too_large",
                "argv.total_bytes",
                usize::MAX,
                MAX_ARGV_TOTAL_BYTES,
            )
        })?;
    }
    if total > MAX_ARGV_TOTAL_BYTES {
        return Err(limit_error(
            "argv_too_large",
            "argv.total_bytes",
            total,
            MAX_ARGV_TOTAL_BYTES,
        ));
    }
    Ok(())
}

fn validate_runtime_values(
    inputs: &BTreeMap<String, String>,
    environment: &BTreeMap<String, String>,
) -> Result<(), ProtocolError> {
    if inputs.len() > MAX_RUNTIME_INPUTS {
        return Err(limit_error(
            "request_too_large",
            "inputs.items",
            inputs.len(),
            MAX_RUNTIME_INPUTS,
        ));
    }
    if environment.len() > MAX_ENVIRONMENT_NAMES {
        return Err(limit_error(
            "request_too_large",
            "environment.items",
            environment.len(),
            MAX_ENVIRONMENT_NAMES,
        ));
    }
    let mut value_bytes = 0_usize;
    for (name, value) in inputs.iter().chain(environment.iter()) {
        if name.is_empty()
            || name.len() > 256
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.')
        {
            return Err(ProtocolError::invalid_frame(format!(
                "runtime value name {name:?} is invalid"
            )));
        }
        if value.len() > MAX_RUNTIME_VALUE_BYTES {
            return Err(limit_error(
                "argument_too_large",
                &format!("runtime_values.{name}"),
                value.len(),
                MAX_RUNTIME_VALUE_BYTES,
            ));
        }
        value_bytes = value_bytes
            .checked_add(name.len())
            .and_then(|size| size.checked_add(value.len()))
            .ok_or_else(|| {
                limit_error(
                    "request_too_large",
                    "runtime_values.total_bytes",
                    usize::MAX,
                    MAX_RUNTIME_VALUES_TOTAL_BYTES,
                )
            })?;
    }
    if value_bytes > MAX_RUNTIME_VALUES_TOTAL_BYTES {
        return Err(limit_error(
            "request_too_large",
            "runtime_values.total_bytes",
            value_bytes,
            MAX_RUNTIME_VALUES_TOTAL_BYTES,
        ));
    }
    Ok(())
}

fn limit_error(class: &'static str, field: &str, observed: usize, limit: usize) -> ProtocolError {
    ProtocolError::LimitExceeded {
        class,
        field: field.to_owned(),
        observed,
        limit,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        read_message, validate_message, write_message, ClientMessage, ClientRequest,
        IdentityRequest, ProtocolError, ProtocolLimits, ServerMessage, ServerResponse,
        PROTOCOL_VERSION,
    };
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use std::path::PathBuf;

    fn message(request: ClientRequest) -> ClientMessage {
        ClientMessage {
            version: PROTOCOL_VERSION,
            request,
        }
    }

    #[test]
    fn v4_framed_prepare_messages_round_trip_without_request_ids() {
        let original = message(ClientRequest::PrepareExec {
            argv: vec!["echo".to_owned(), "hello".to_owned()],
            cwd: PathBuf::from("/workspace"),
            inputs: BTreeMap::new(),
            environment: BTreeMap::new(),
            request_budget_ms: 1000,
        });
        let mut frame = Vec::new();
        write_message(&mut frame, &original).unwrap();
        let decoded: ClientMessage = read_message(&mut Cursor::new(frame)).unwrap();
        assert_eq!(decoded, original);

        let malformed = br#"{"version":3,"request_id":"x","request":{"type":"hello"}}"#;
        let mut frame = (malformed.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(malformed);
        assert!(read_message::<ClientMessage>(&mut Cursor::new(frame)).is_err());
    }

    #[test]
    fn v4_identity_handshake_includes_negotiated_limits() {
        let limits = serde_json::to_value(ProtocolLimits::current()).unwrap();
        let hello = serde_json::to_value(message(ClientRequest::Hello)).unwrap();
        assert_eq!(
            hello,
            serde_json::json!({
                "version": 4,
                "request": { "type": "hello" }
            })
        );

        let prepare = message(ClientRequest::PrepareIdentity {
            cwd: PathBuf::from("/workspace"),
            inputs: BTreeMap::from([("HOST_UID".to_owned(), "1000".to_owned())]),
            environment: BTreeMap::from([("HOME".to_owned(), "/home/dev".to_owned())]),
            requested: IdentityRequest::Peer {
                uid: 1000,
                gid: 1000,
            },
        });
        assert_eq!(
            serde_json::to_value(prepare).unwrap(),
            serde_json::json!({
                "version": 4,
                "request": {
                    "type": "prepare_identity",
                    "cwd": "/workspace",
                    "inputs": { "HOST_UID": "1000" },
                    "environment": { "HOME": "/home/dev" },
                    "requested": { "kind": "peer", "uid": 1000, "gid": 1000 }
                }
            })
        );

        let hello: ServerMessage = serde_json::from_value(serde_json::json!({
            "version": 4,
            "response": {
                "type": "hello",
                "state": "ready",
                "profile": "nixos-docker",
                "snapshot_id": "snapshot",
                "runtime_inputs": ["HOST_UID"],
                "environment_names": ["HOME"],
                "limits": limits
            }
        }))
        .unwrap();
        assert!(matches!(hello.response, ServerResponse::Hello(_)));

        let identity: ServerMessage = serde_json::from_value(serde_json::json!({
            "version": 4,
            "response": {
                "type": "identity",
                "uid": 1000,
                "gid": 1000,
                "user": "dev",
                "home": "/home/dev",
                "supplemental_groups": [1000, 1001],
                "run_as_root": false
            }
        }))
        .unwrap();
        assert!(matches!(identity.response, ServerResponse::Identity(_)));
    }

    #[test]
    fn frame_size_utf8_truncation_version_and_request_fields_are_checked() {
        let too_large = ((super::MAX_REQUEST_FRAME_BYTES + 1) as u32).to_be_bytes();
        assert!(read_message::<ClientMessage>(&mut Cursor::new(too_large)).is_err());
        assert!(read_message::<ClientMessage>(&mut Cursor::new([0, 0])).is_err());

        let mut invalid_utf8 = 1_u32.to_be_bytes().to_vec();
        invalid_utf8.push(0xff);
        assert!(read_message::<ClientMessage>(&mut Cursor::new(invalid_utf8)).is_err());

        let mut invalid_version = message(ClientRequest::Hello);
        invalid_version.version = 1;
        assert!(matches!(
            validate_message(&invalid_version),
            Err(ProtocolError::UnsupportedVersion(1))
        ));

        let invalid_argv = message(ClientRequest::PrepareExec {
            argv: vec!["bad\0arg".to_owned()],
            cwd: PathBuf::from("/workspace"),
            inputs: BTreeMap::new(),
            environment: BTreeMap::new(),
            request_budget_ms: 1000,
        });
        assert!(validate_message(&invalid_argv).is_err());

        let identity = message(ClientRequest::PrepareIdentity {
            cwd: PathBuf::from("/workspace"),
            inputs: BTreeMap::new(),
            environment: BTreeMap::new(),
            requested: IdentityRequest::Peer {
                uid: 1000,
                gid: 1000,
            },
        });
        let mut frame = Vec::new();
        write_message(&mut frame, &identity).unwrap();
        let decoded: ClientMessage = read_message(&mut Cursor::new(frame)).unwrap();
        assert_eq!(decoded, identity);

        let invalid_user = message(ClientRequest::PrepareIdentity {
            cwd: PathBuf::from("/workspace"),
            inputs: BTreeMap::new(),
            environment: BTreeMap::new(),
            requested: IdentityRequest::User {
                name: "not/a-user".to_owned(),
            },
        });
        assert!(validate_message(&invalid_user).is_err());
    }

    #[test]
    fn argv_limits_count_utf8_bytes_and_accept_the_reported_regression_case() {
        let regression_argument = "x".repeat(37_543);
        assert!(super::validate_argv(&[regression_argument]).is_ok());

        let oversized_argument = "x".repeat(super::MAX_ARGV_ITEM_BYTES + 1);
        assert!(matches!(
            super::validate_argv(&[oversized_argument]),
            Err(ProtocolError::LimitExceeded {
                class: "argument_too_large",
                ..
            })
        ));

        let oversized_total = vec!["x".repeat(super::MAX_ARGV_ITEM_BYTES); 17];
        assert!(matches!(
            super::validate_argv(&oversized_total),
            Err(ProtocolError::LimitExceeded {
                class: "argv_too_large",
                ..
            })
        ));

        let oversized_count = vec![String::new(); super::MAX_ARGV_ITEMS + 1];
        assert!(matches!(
            super::validate_argv(&oversized_count),
            Err(ProtocolError::LimitExceeded {
                class: "argv_too_large",
                ..
            })
        ));
    }

    #[test]
    fn runtime_value_limits_measure_utf8_bytes() {
        let mut request = ClientRequest::PrepareIdentity {
            cwd: PathBuf::from("/workspace"),
            inputs: BTreeMap::from([(
                "INPUT".to_owned(),
                "é".repeat(super::MAX_RUNTIME_VALUE_BYTES / 2 + 1),
            )]),
            environment: BTreeMap::new(),
            requested: IdentityRequest::Root,
        };
        let mut message = message(request.clone());
        assert!(matches!(
            validate_message(&message),
            Err(ProtocolError::LimitExceeded {
                class: "argument_too_large",
                ..
            })
        ));
        if let ClientRequest::PrepareIdentity { inputs, .. } = &mut request {
            inputs.insert("INPUT".to_owned(), "é".repeat(100));
        }
        message.request = request;
        assert!(validate_message(&message).is_ok());
    }
}
