use container_init_core::{HandoffCommand, ResolvedIdentity};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

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
    pub request_id: String,
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
    Status,
    Plan,
    Doctor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerMessage {
    pub version: u16,
    pub request_id: String,
    pub response: ServerResponse,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerResponse {
    Hello(HelloInfo),
    Prepared(PreparedHandoff),
    Status(BackendStatus),
    Plan(serde_json::Value),
    Doctor(serde_json::Value),
    Error(BackendError),
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
    pub active_requests: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendError {
    pub class: String,
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
    InvalidRequestId,
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
            Self::InvalidRequestId => {
                io::Error::new(io::ErrorKind::InvalidData, "invalid request id")
            }
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
            Self::InvalidRequestId => formatter.write_str("invalid request id"),
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

pub fn read_message<T: for<'de> Deserialize<'de>>(
    reader: &mut impl Read,
) -> Result<T, ProtocolError> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(ProtocolError::invalid_frame(format!(
            "frame length {length} is outside the allowed range"
        )));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload)
        .map_err(|error| ProtocolError::invalid_frame(error.to_string()))
}

pub fn write_message<T: Serialize>(
    writer: &mut impl Write,
    message: &T,
) -> Result<(), ProtocolError> {
    let payload = serde_json::to_vec(message)
        .map_err(|error| ProtocolError::invalid_frame(error.to_string()))?;
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::invalid_frame(format!(
            "frame length {} is outside the allowed range",
            payload.len()
        )));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| ProtocolError::invalid_frame("frame length exceeds u32"))?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

pub fn validate_message(message: &ClientMessage) -> Result<(), ProtocolError> {
    if message.version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(message.version));
    }
    if message.request_id.is_empty()
        || message.request_id.len() > 128
        || !message
            .request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ProtocolError::InvalidRequestId);
    }
    if let ClientRequest::Exec {
        argv,
        inputs,
        environment,
        ..
    } = &message.request
    {
        if argv.iter().any(|argument| argument.contains('\0')) {
            return Err(ProtocolError::invalid_frame("argv contains a NUL byte"));
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
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        read_message, validate_message, write_message, ClientMessage, ClientRequest, ProtocolError,
        PROTOCOL_VERSION,
    };
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use std::path::PathBuf;

    fn message(request: ClientRequest) -> ClientMessage {
        ClientMessage {
            version: PROTOCOL_VERSION,
            request_id: "test-1".to_owned(),
            request,
        }
    }

    #[test]
    fn framed_messages_round_trip_and_reject_unknown_fields() {
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

        let malformed =
            br#"{"version":1,"request_id":"x","unknown":true,"request":{"type":"hello"}}"#;
        let mut frame = (malformed.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(malformed);
        assert!(read_message::<ClientMessage>(&mut Cursor::new(frame)).is_err());
    }

    #[test]
    fn frame_size_utf8_truncation_version_and_argv_are_checked() {
        let too_large = ((super::MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
        assert!(read_message::<ClientMessage>(&mut Cursor::new(too_large)).is_err());
        assert!(read_message::<ClientMessage>(&mut Cursor::new([0, 0])).is_err());

        let mut invalid_utf8 = 1_u32.to_be_bytes().to_vec();
        invalid_utf8.push(0xff);
        assert!(read_message::<ClientMessage>(&mut Cursor::new(invalid_utf8)).is_err());

        let mut invalid_version = message(ClientRequest::Hello);
        invalid_version.version += 1;
        assert!(matches!(
            validate_message(&invalid_version),
            Err(ProtocolError::UnsupportedVersion(_))
        ));

        let invalid_argv = message(ClientRequest::Exec {
            argv: vec!["bad\0arg".to_owned()],
            cwd: PathBuf::from("/workspace"),
            inputs: BTreeMap::new(),
            environment: BTreeMap::new(),
        });
        assert!(validate_message(&invalid_argv).is_err());
    }
}
