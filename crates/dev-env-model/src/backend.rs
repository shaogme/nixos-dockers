use crate::identity::EffectiveIdentity;
use crate::materialized::MaterializedEnv;
use crate::request::{RequestContext, RequestValidationError};
use crate::snapshot::Generation;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::PathBuf;

/// The dev-env backend protocol is deliberately independent from the
/// container-init protocol number.
pub const DEVENV_BACKEND_PROTOCOL_VERSION: u16 = 3;
pub const BACKEND_PROTOCOL_VERSION: u16 = DEVENV_BACKEND_PROTOCOL_VERSION;
pub const DEVENV_BACKEND_ERROR_VERSION: u16 = 2;
pub const MAX_BACKEND_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendState {
    Starting,
    Ready,
    Reloading,
    Stopping,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendRequestMessage {
    pub protocol: u16,
    pub request_id: String,
    pub request: BackendRequest,
}

pub type BackendRequestEnvelope = BackendRequestMessage;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendRequest {
    Hello,
    Prepare {
        context: RequestContext,
    },
    Status,
    Plan,
    Explain {
        path: Option<String>,
    },
    Doctor,
    Trust {
        target: TrustTarget,
    },
    Reload {
        #[serde(default)]
        wait: bool,
    },
    Stop,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendResponseMessage {
    pub protocol: u16,
    pub request_id: String,
    pub response: BackendResponse,
}

pub type BackendResponseEnvelope = BackendResponseMessage;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendResponse {
    Hello(HelloInfo),
    Prepared(Box<PreparedResponse>),
    Status(BackendStatus),
    Plan(serde_json::Value),
    Explain(serde_json::Value),
    Doctor(serde_json::Value),
    Trusted {
        generation: Generation,
    },
    Reloaded {
        generation: Generation,
        config_fingerprint: [u8; 32],
    },
    Stopped,
    Error(BackendError),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HelloInfo {
    pub state: BackendState,
    pub generation: Generation,
    pub config_fingerprint: [u8; 32],
    #[serde(default)]
    pub runtime_inputs: Vec<String>,
    #[serde(default)]
    pub environment_names: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedResponse {
    pub generation: Generation,
    pub config_fingerprint: [u8; 32],
    pub cwd: PathBuf,
    pub shell_invocation: ShellInvocation,
    pub materialized_environment: MaterializedEnv,
    /// Identity resolved by the container-init broker for this request.
    /// Clients normally already run as this peer; the backend uses these
    /// fields for the initial service handoff and auditability.
    #[serde(default = "EffectiveIdentity::root")]
    pub identity: EffectiveIdentity,
    #[serde(default)]
    pub supplemental_groups: Vec<u32>,
    #[serde(default)]
    pub diagnostics: Vec<ProviderDiagnosticSummary>,
    pub cache_hit: bool,
    pub receipt_summary: ReceiptSummary,
}

pub type PrepareResponse = PreparedResponse;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShellInvocation {
    pub executable: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDiagnosticSummary {
    pub provider: String,
    pub class: String,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub status: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default)]
    pub orphaned_children: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
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
    pub generation: Generation,
    pub config_fingerprint: [u8; 32],
    pub backend_pid: u32,
    pub active_requests: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendError {
    pub error_version: u16,
    pub class: String,
    pub retryable: bool,
    pub message: String,
    #[serde(default)]
    pub generation: Option<Generation>,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TrustTarget {
    Path { path: PathBuf },
    Sha256 { digest: [u8; 32] },
}

impl TrustTarget {
    pub fn validate(&self) -> Result<(), BackendProtocolError> {
        match self {
            Self::Path { path }
                if !path.as_os_str().is_empty()
                    && path.is_absolute()
                    && !path.to_string_lossy().contains('\0') =>
            {
                Ok(())
            }
            Self::Path { .. } => Err(BackendProtocolError::InvalidRequest(
                "trust path must be absolute".to_owned(),
            )),
            Self::Sha256 { .. } => Ok(()),
        }
    }
}

impl BackendRequestMessage {
    pub fn new(request_id: impl Into<String>, request: BackendRequest) -> Self {
        Self {
            protocol: DEVENV_BACKEND_PROTOCOL_VERSION,
            request_id: request_id.into(),
            request,
        }
    }

    pub fn validate(&self) -> Result<(), BackendProtocolError> {
        if self.protocol != DEVENV_BACKEND_PROTOCOL_VERSION {
            return Err(BackendProtocolError::UnsupportedVersion(self.protocol));
        }
        if self.request_id.is_empty()
            || self.request_id.len() > 128
            || !self
                .request_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(BackendProtocolError::InvalidRequest(
                "request id is invalid".to_owned(),
            ));
        }
        match &self.request {
            BackendRequest::Prepare { context } => {
                if context.request_id != self.request_id {
                    return Err(BackendProtocolError::InvalidRequest(
                        "prepare context request id does not match envelope".to_owned(),
                    ));
                }
                context
                    .validate()
                    .map_err(|source| BackendProtocolError::InvalidContext { source })
            }
            BackendRequest::Explain { path } => {
                if path.as_deref().is_some_and(|path| path.contains('\0')) {
                    Err(BackendProtocolError::InvalidRequest(
                        "explain path contains NUL".to_owned(),
                    ))
                } else {
                    Ok(())
                }
            }
            BackendRequest::Trust { target } => target.validate(),
            BackendRequest::Hello
            | BackendRequest::Status
            | BackendRequest::Plan
            | BackendRequest::Doctor
            | BackendRequest::Reload { .. }
            | BackendRequest::Stop => Ok(()),
        }
    }
}

impl BackendResponseMessage {
    pub fn new(request_id: impl Into<String>, response: BackendResponse) -> Self {
        Self {
            protocol: DEVENV_BACKEND_PROTOCOL_VERSION,
            request_id: request_id.into(),
            response,
        }
    }

    pub fn validate(&self) -> Result<(), BackendProtocolError> {
        if self.protocol != DEVENV_BACKEND_PROTOCOL_VERSION {
            return Err(BackendProtocolError::UnsupportedVersion(self.protocol));
        }
        if self.request_id.is_empty()
            || self.request_id.len() > 128
            || !self
                .request_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(BackendProtocolError::InvalidRequest(
                "response request id is invalid".to_owned(),
            ));
        }
        if let BackendResponse::Error(error) = &self.response {
            error.validate()?;
        }
        Ok(())
    }
}

impl BackendError {
    fn validate(&self) -> Result<(), BackendProtocolError> {
        if self.error_version != DEVENV_BACKEND_ERROR_VERSION {
            return Err(BackendProtocolError::UnsupportedErrorVersion(
                self.error_version,
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum BackendProtocolError {
    Json(serde_json::Error),
    Io(io::Error),
    UnsupportedVersion(u16),
    UnsupportedErrorVersion(u16),
    InvalidRequest(String),
    InvalidContext { source: RequestValidationError },
    FrameTooLarge(usize),
}

impl std::fmt::Display for BackendProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(source) => write!(formatter, "backend protocol JSON failed: {source}"),
            Self::Io(source) => write!(formatter, "backend protocol IO failed: {source}"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported dev-env backend protocol version {version}"
                )
            }
            Self::UnsupportedErrorVersion(version) => {
                write!(
                    formatter,
                    "unsupported dev-env backend error version {version}"
                )
            }
            Self::InvalidRequest(message) => {
                write!(formatter, "invalid backend request: {message}")
            }
            Self::InvalidContext { source } => {
                write!(formatter, "invalid prepare context: {source}")
            }
            Self::FrameTooLarge(size) => {
                write!(
                    formatter,
                    "backend frame has {size} bytes, maximum is {MAX_BACKEND_FRAME_BYTES}"
                )
            }
        }
    }
}

impl std::error::Error for BackendProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(source) => Some(source),
            Self::Io(source) => Some(source),
            Self::InvalidContext { source } => Some(source),
            Self::UnsupportedVersion(_)
            | Self::UnsupportedErrorVersion(_)
            | Self::InvalidRequest(_)
            | Self::FrameTooLarge(_) => None,
        }
    }
}

pub fn encode_backend_request(
    message: &BackendRequestMessage,
) -> Result<Vec<u8>, BackendProtocolError> {
    message.validate()?;
    encode_json(message)
}

pub fn decode_backend_request(
    payload: &[u8],
) -> Result<BackendRequestMessage, BackendProtocolError> {
    ensure_frame_size(payload)?;
    let message: BackendRequestMessage =
        serde_json::from_slice(payload).map_err(BackendProtocolError::Json)?;
    message.validate()?;
    Ok(message)
}

pub fn encode_backend_response(
    message: &BackendResponseMessage,
) -> Result<Vec<u8>, BackendProtocolError> {
    message.validate()?;
    encode_json(message)
}

pub fn decode_backend_response(
    payload: &[u8],
) -> Result<BackendResponseMessage, BackendProtocolError> {
    ensure_frame_size(payload)?;
    let message: BackendResponseMessage =
        serde_json::from_slice(payload).map_err(BackendProtocolError::Json)?;
    message.validate()?;
    Ok(message)
}

fn encode_json<T: Serialize>(value: &T) -> Result<Vec<u8>, BackendProtocolError> {
    let payload = serde_json::to_vec(value).map_err(BackendProtocolError::Json)?;
    if payload.len() > MAX_BACKEND_FRAME_BYTES {
        return Err(BackendProtocolError::FrameTooLarge(payload.len()));
    }
    Ok(payload)
}

/// Write one JSON envelope using the backend's four-byte big-endian frame.
pub fn write_backend_request(
    writer: &mut impl Write,
    message: &BackendRequestMessage,
) -> Result<(), BackendProtocolError> {
    write_frame(writer, &encode_backend_request(message)?)
}

pub fn read_backend_request(
    reader: &mut impl Read,
) -> Result<BackendRequestMessage, BackendProtocolError> {
    decode_backend_request(&read_frame(reader)?)
}

pub fn write_backend_response(
    writer: &mut impl Write,
    message: &BackendResponseMessage,
) -> Result<(), BackendProtocolError> {
    write_frame(writer, &encode_backend_response(message)?)
}

pub fn read_backend_response(
    reader: &mut impl Read,
) -> Result<BackendResponseMessage, BackendProtocolError> {
    decode_backend_response(&read_frame(reader)?)
}

fn write_frame(writer: &mut impl Write, payload: &[u8]) -> Result<(), BackendProtocolError> {
    unix_frame::write_payload_frame(writer, payload, MAX_BACKEND_FRAME_BYTES)
        .map_err(map_frame_error)
}

fn read_frame(reader: &mut impl Read) -> Result<Vec<u8>, BackendProtocolError> {
    unix_frame::read_payload_frame(reader, MAX_BACKEND_FRAME_BYTES).map_err(map_frame_error)
}

fn map_frame_error(error: unix_frame::FrameError) -> BackendProtocolError {
    match error {
        unix_frame::FrameError::Io(error) => BackendProtocolError::Io(error),
        unix_frame::FrameError::Json(error) => BackendProtocolError::Json(error),
        unix_frame::FrameError::Empty => BackendProtocolError::FrameTooLarge(0),
        unix_frame::FrameError::TooLarge { length, .. }
        | unix_frame::FrameError::LengthOverflow(length) => {
            BackendProtocolError::FrameTooLarge(length)
        }
    }
}

fn ensure_frame_size(payload: &[u8]) -> Result<(), BackendProtocolError> {
    if payload.is_empty() || payload.len() > MAX_BACKEND_FRAME_BYTES {
        return Err(BackendProtocolError::FrameTooLarge(payload.len()));
    }
    Ok(())
}
