use crate::{
    identity::EffectiveIdentity,
    materialized::MaterializedEnv,
    request::{RequestContext, RequestValidationError},
    snapshot::Generation,
};
use serde::{Deserialize, Serialize};
use serde_json::{from_slice, to_vec, Error as JsonError, Value};
use std::{
    error::Error as StdError,
    fmt::{Display, Formatter, Result as FmtResult},
    io::{self, Read, Write},
    path::PathBuf,
};
use unix_frame::{read_payload_frame, write_payload_frame, FrameError};

/// The dev-env backend protocol is deliberately independent from the
/// container-init protocol number.
pub const DEVENV_BACKEND_PROTOCOL_VERSION: u16 = 5;
pub const BACKEND_PROTOCOL_VERSION: u16 = DEVENV_BACKEND_PROTOCOL_VERSION;
pub const DEVENV_BACKEND_ERROR_VERSION: u16 = 2;
pub const MAX_BACKEND_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub enum BackendProtocolError {
    Json(JsonError),
    Io(io::Error),
    UnsupportedVersion(u16),
    UnsupportedErrorVersion(u16),
    InvalidRequest(String),
    InvalidContext { source: RequestValidationError },
    FrameTooLarge(usize),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneState {
    Starting,
    Serving,
    Reloading,
    Stopping,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InitialRuntimeState {
    Preparing,
    Running,
    Exited,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TrustTarget {
    Path { path: PathBuf },
    Sha256 { digest: [u8; 32] },
}

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
pub struct BackendRequestMessage {
    protocol: u16,
    request_id: String,
    request: BackendRequest,
}

pub type BackendRequestEnvelope = BackendRequestMessage;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HelloInfo {
    pub control_plane: ControlPlaneState,
    pub initial_runtime: InitialRuntimeState,
    pub startup_phase: Option<String>,
    pub initial_runtime_exit_code: Option<i32>,
    pub initial_runtime_error: Option<String>,
    pub generation: Generation,
    pub config_fingerprint: [u8; 32],
    #[serde(default)]
    pub runtime_inputs: Vec<String>,
    #[serde(default)]
    pub environment_names: Vec<String>,
}

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
    pub control_plane: ControlPlaneState,
    pub initial_runtime: InitialRuntimeState,
    pub startup_phase: Option<String>,
    pub initial_runtime_exit_code: Option<i32>,
    pub initial_runtime_error: Option<String>,
    pub generation: Generation,
    pub config_fingerprint: [u8; 32],
    pub backend_pid: u32,
    pub active_requests: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendError {
    error_version: u16,
    class: String,
    retryable: bool,
    message: String,
    #[serde(default)]
    generation: Option<Generation>,
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default)]
    operation: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
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
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendResponse {
    Hello(HelloInfo),
    Prepared(Box<PreparedResponse>),
    Status(BackendStatus),
    Plan(Value),
    Explain(Value),
    Doctor(Value),
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
pub struct BackendResponseMessage {
    protocol: u16,
    request_id: String,
    response: BackendResponse,
}

pub type BackendResponseEnvelope = BackendResponseMessage;

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
                if context.request_id() != self.request_id {
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
    pub fn new(class: impl Into<String>, retryable: bool, message: impl Into<String>) -> Self {
        Self {
            error_version: DEVENV_BACKEND_ERROR_VERSION,
            class: class.into(),
            retryable,
            message: message.into(),
            generation: None,
            provider_id: None,
            operation: None,
            request_id: None,
        }
    }

    pub fn with_error_version(mut self, error_version: u16) -> Self {
        self.error_version = error_version;
        self
    }

    pub fn with_generation(mut self, generation: Generation) -> Self {
        self.generation = Some(generation);
        self
    }

    pub fn with_provider(mut self, provider_id: impl Into<String>) -> Self {
        self.provider_id = Some(provider_id.into());
        self
    }

    pub fn with_operation(mut self, operation: impl Into<String>) -> Self {
        self.operation = Some(operation.into());
        self
    }

    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    fn validate(&self) -> Result<(), BackendProtocolError> {
        if self.error_version != DEVENV_BACKEND_ERROR_VERSION {
            return Err(BackendProtocolError::UnsupportedErrorVersion(
                self.error_version,
            ));
        }
        Ok(())
    }
}

impl Display for BackendProtocolError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
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

impl StdError for BackendProtocolError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
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

impl BackendRequestMessage {
    pub fn encode(&self) -> Result<Vec<u8>, BackendProtocolError> {
        self.validate()?;
        encode_json(self)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, BackendProtocolError> {
        ensure_frame_size(payload)?;
        let message: Self = from_slice(payload).map_err(BackendProtocolError::Json)?;
        message.validate()?;
        Ok(message)
    }

    pub fn write_to(&self, writer: &mut impl Write) -> Result<(), BackendProtocolError> {
        write_frame(writer, &self.encode()?)
    }

    pub fn read_from(reader: &mut impl Read) -> Result<Self, BackendProtocolError> {
        Self::decode(&read_frame(reader)?)
    }
}

impl BackendResponseMessage {
    pub fn encode(&self) -> Result<Vec<u8>, BackendProtocolError> {
        self.validate()?;
        encode_json(self)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, BackendProtocolError> {
        ensure_frame_size(payload)?;
        let message: Self = from_slice(payload).map_err(BackendProtocolError::Json)?;
        message.validate()?;
        Ok(message)
    }

    pub fn write_to(&self, writer: &mut impl Write) -> Result<(), BackendProtocolError> {
        write_frame(writer, &self.encode()?)
    }

    pub fn read_from(reader: &mut impl Read) -> Result<Self, BackendProtocolError> {
        Self::decode(&read_frame(reader)?)
    }
}

fn encode_json<T: Serialize>(value: &T) -> Result<Vec<u8>, BackendProtocolError> {
    let payload = to_vec(value).map_err(BackendProtocolError::Json)?;
    if payload.len() > MAX_BACKEND_FRAME_BYTES {
        return Err(BackendProtocolError::FrameTooLarge(payload.len()));
    }
    Ok(payload)
}

fn write_frame(writer: &mut impl Write, payload: &[u8]) -> Result<(), BackendProtocolError> {
    write_payload_frame(writer, payload, MAX_BACKEND_FRAME_BYTES).map_err(map_frame_error)
}

fn read_frame(reader: &mut impl Read) -> Result<Vec<u8>, BackendProtocolError> {
    read_payload_frame(reader, MAX_BACKEND_FRAME_BYTES).map_err(map_frame_error)
}

fn map_frame_error(error: FrameError) -> BackendProtocolError {
    match error {
        FrameError::Io(error) => BackendProtocolError::Io(error),
        FrameError::Json(error) => BackendProtocolError::Json(error),
        FrameError::Empty => BackendProtocolError::FrameTooLarge(0),
        FrameError::TooLarge { length, .. } | FrameError::LengthOverflow(length) => {
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

impl BackendRequestMessage {
    pub fn protocol(&self) -> u16 {
        self.protocol
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn request(&self) -> &BackendRequest {
        &self.request
    }
}

impl BackendResponseMessage {
    pub fn protocol(&self) -> u16 {
        self.protocol
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn response(&self) -> &BackendResponse {
        &self.response
    }
}

impl BackendError {
    pub fn error_version(&self) -> u16 {
        self.error_version
    }

    pub fn class(&self) -> &str {
        &self.class
    }

    pub fn retryable(&self) -> bool {
        self.retryable
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn generation(&self) -> Option<Generation> {
        self.generation
    }

    pub fn provider_id(&self) -> Option<&str> {
        self.provider_id.as_deref()
    }

    pub fn operation(&self) -> Option<&str> {
        self.operation.as_deref()
    }

    pub fn request_id(&self) -> Option<&str> {
        self.request_id.as_deref()
    }
}
