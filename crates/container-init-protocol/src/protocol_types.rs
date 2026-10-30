use libc::{sysconf, _SC_NGROUPS_MAX};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    error::Error,
    fmt::{Display, Formatter, Result as FmtResult},
    io::{self, ErrorKind},
    path::PathBuf,
};

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolLimits {
    request_frame_bytes: usize,
    response_frame_bytes: usize,
    argv_items: usize,
    argv_item_bytes: usize,
    argv_total_bytes: usize,
    runtime_inputs: usize,
    environment_names: usize,
    runtime_value_bytes: usize,
    runtime_values_total_bytes: usize,
    cwd_bytes: usize,
    plan_page_actions: usize,
    supplementary_groups: usize,
    frame_timeout_ms: u64,
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
            supplementary_groups: Self::max_supplementary_groups(),
            frame_timeout_ms: FRAME_TIMEOUT_MS,
        }
    }

    pub fn request_frame_bytes(&self) -> usize {
        self.request_frame_bytes
    }

    pub fn response_frame_bytes(&self) -> usize {
        self.response_frame_bytes
    }

    pub fn argv_items(&self) -> usize {
        self.argv_items
    }

    pub fn argv_item_bytes(&self) -> usize {
        self.argv_item_bytes
    }

    pub fn argv_total_bytes(&self) -> usize {
        self.argv_total_bytes
    }

    pub fn runtime_inputs(&self) -> usize {
        self.runtime_inputs
    }

    pub fn environment_names(&self) -> usize {
        self.environment_names
    }

    pub fn runtime_value_bytes(&self) -> usize {
        self.runtime_value_bytes
    }

    pub fn runtime_values_total_bytes(&self) -> usize {
        self.runtime_values_total_bytes
    }

    pub fn cwd_bytes(&self) -> usize {
        self.cwd_bytes
    }

    pub fn plan_page_actions(&self) -> usize {
        self.plan_page_actions
    }

    pub fn supplementary_groups(&self) -> usize {
        self.supplementary_groups
    }

    pub fn frame_timeout_ms(&self) -> u64 {
        self.frame_timeout_ms
    }

    pub fn max_supplementary_groups() -> usize {
        let platform_limit = unsafe { sysconf(_SC_NGROUPS_MAX) };
        if platform_limit > 0 {
            (platform_limit as usize).min(65_536)
        } else {
            65_536
        }
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
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum IdentityRequest {
    Peer { uid: u32, gid: u32 },
    Root,
    User { name: String },
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
pub struct ClientMessage {
    pub version: u16,
    pub request: ClientRequest,
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
    pub actions: Vec<Value>,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CwdObject {
    pub device: u64,
    pub inode: u64,
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
pub struct ReceiptSummary {
    pub succeeded: bool,
    pub action_count: usize,
    pub warning_count: usize,
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
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerResponse {
    Hello(HelloInfo),
    PreparedExec(PreparedHandoff),
    CommitResult(CommitResult),
    Identity(PreparedIdentity),
    Status(BackendStatus),
    PlanPage(PlanPage),
    Doctor(Value),
    Error(BackendError),
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerMessage {
    pub version: u16,
    pub response: ServerResponse,
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
            Self::InvalidFrame(message) => io::Error::new(ErrorKind::InvalidData, message.clone()),
            Self::UnsupportedVersion(version) => io::Error::new(
                ErrorKind::InvalidData,
                format!("unsupported protocol version {version}"),
            ),
            Self::FrameTooLarge { length, limit } => io::Error::new(
                ErrorKind::InvalidData,
                format!("frame length {length} exceeds the {limit} byte limit"),
            ),
            Self::LimitExceeded {
                class,
                field,
                observed,
                limit,
            } => io::Error::new(
                ErrorKind::InvalidData,
                format!("{class}: {field} is {observed}; maximum is {limit}"),
            ),
            Self::IncompatibleLimits => io::Error::new(
                ErrorKind::InvalidData,
                "backend protocol limits are incompatible with this client build",
            ),
        }
    }
}

impl Display for ProtocolError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
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

impl Error for ProtocolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
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
