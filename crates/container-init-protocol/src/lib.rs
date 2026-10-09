use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Instant;

mod client;

pub use client::{IdentityBrokerClient, IdentityBrokerError};

pub const PROTOCOL_VERSION: u16 = 2;
pub const MAX_REQUEST_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_RESPONSE_FRAME_BYTES: usize = 1024 * 1024;

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
    Exec {
        argv: Vec<String>,
        cwd: PathBuf,
        #[serde(default)]
        inputs: BTreeMap<String, String>,
        #[serde(default)]
        environment: BTreeMap<String, String>,
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
    Prepared(PreparedHandoff),
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedHandoff {
    pub command: HandoffCommand,
    /// Canonical request working directory selected by the backend.
    pub cwd: PathBuf,
    /// The complete login environment that must be applied to the handoff.
    pub login_environment: BTreeMap<String, String>,
    pub identity: ResolvedIdentity,
    pub supplemental_groups: Vec<u32>,
    pub root_service: bool,
    pub receipt_summary: ReceiptSummary,
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
    FrameTooLarge { length: usize, limit: usize },
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
        ClientRequest::Exec {
            argv,
            cwd,
            inputs,
            environment,
        } => (Some(argv.as_slice()), Some(cwd), inputs, environment),
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
        if cwd.as_os_str().as_encoded_bytes().len() > 4096 {
            return Err(ProtocolError::invalid_frame(
                "cwd exceeds the 4096 byte limit",
            ));
        }
    }
    if let Some(argv) = argv {
        if argv.len() > 256 {
            return Err(ProtocolError::invalid_frame(format!(
                "argv contains {} arguments; maximum is 256",
                argv.len()
            )));
        }
        for (index, argument) in argv.iter().enumerate() {
            if argument.contains('\0') {
                return Err(ProtocolError::invalid_frame(format!(
                    "argv[{index}] contains NUL"
                )));
            }
            if argument.len() > 16 * 1024 {
                return Err(ProtocolError::invalid_frame(format!(
                    "argv[{index}] is {} bytes; maximum is 16384 bytes",
                    argument.len()
                )));
            }
        }
    }
    if inputs.len() > 64 || environment.len() > 128 {
        return Err(ProtocolError::invalid_frame("too many runtime values"));
    }
    let value_bytes = inputs
        .iter()
        .chain(environment.iter())
        .try_fold(0_usize, |size, (name, value)| {
            if name.is_empty()
                || name.len() > 256
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.')
                || value.len() > 16 * 1024
            {
                return None;
            }
            size.checked_add(name.len())?.checked_add(value.len())
        })
        .ok_or_else(|| ProtocolError::invalid_frame("invalid or oversized runtime values"))?;
    if value_bytes > 64 * 1024 {
        return Err(ProtocolError::invalid_frame(
            "runtime values exceed the 64 KiB limit",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        read_message, validate_message, write_message, ClientMessage, ClientRequest,
        IdentityRequest, ProtocolError, ServerMessage, ServerResponse, PROTOCOL_VERSION,
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
    fn v2_framed_messages_round_trip_without_request_ids() {
        let original = message(ClientRequest::Exec {
            argv: vec!["echo".to_owned(), "hello".to_owned()],
            cwd: PathBuf::from("/workspace"),
            inputs: BTreeMap::new(),
            environment: BTreeMap::new(),
        });
        let mut frame = Vec::new();
        write_message(&mut frame, &original).unwrap();
        let decoded: ClientMessage = read_message(&mut Cursor::new(frame)).unwrap();
        assert_eq!(decoded, original);

        let malformed = br#"{"version":2,"request_id":"x","request":{"type":"hello"}}"#;
        let mut frame = (malformed.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(malformed);
        assert!(read_message::<ClientMessage>(&mut Cursor::new(frame)).is_err());
    }

    #[test]
    fn v2_identity_handshake_keeps_hello_and_prepare_wire_shapes() {
        let hello = serde_json::to_value(message(ClientRequest::Hello)).unwrap();
        assert_eq!(
            hello,
            serde_json::json!({
                "version": 2,
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
                "version": 2,
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
            "version": 2,
            "response": {
                "type": "hello",
                "state": "ready",
                "profile": "nixos-docker",
                "snapshot_id": "snapshot",
                "runtime_inputs": ["HOST_UID"],
                "environment_names": ["HOME"]
            }
        }))
        .unwrap();
        assert!(matches!(hello.response, ServerResponse::Hello(_)));

        let identity: ServerMessage = serde_json::from_value(serde_json::json!({
            "version": 2,
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

        let invalid_argv = message(ClientRequest::Exec {
            argv: vec!["bad\0arg".to_owned()],
            cwd: PathBuf::from("/workspace"),
            inputs: BTreeMap::new(),
            environment: BTreeMap::new(),
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
}
