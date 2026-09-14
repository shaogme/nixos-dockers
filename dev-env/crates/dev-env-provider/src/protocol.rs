use crate::context::ProviderContext;
use crate::error::{ProviderRuntimeError, ProviderRuntimeErrorKind};
use dev_env_model::ProviderReceipt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolOperation {
    Detect,
    Prepare,
    Shellenv,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProtocolRequest {
    pub protocol: u32,
    pub op: ProtocolOperation,
    pub context: ProviderContext,
    pub config: dev_env_model::ValueTree,
    #[serde(default)]
    pub argv: Vec<String>,
}

impl ProtocolRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedProtocol {
                found: self.protocol,
                expected: PROTOCOL_VERSION,
            });
        }
        if self.context.shell.is_empty() {
            return Err(ProtocolError::InvalidRequest {
                field: "context.shell".to_owned(),
            });
        }
        if self.argv.iter().any(|argument| argument.contains('\0')) {
            return Err(ProtocolError::InvalidRequest {
                field: "argv".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ProtocolDiagnostic {
    MissingExecutable,
    NotApplicable,
    CommandFailed {
        status: Option<i32>,
        timed_out: bool,
    },
    OutputRejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ProtocolError {
    UnsupportedProtocol {
        found: u32,
        expected: u32,
    },
    InvalidRequest {
        field: String,
    },
    ProviderUnavailable,
    CommandFailed {
        status: Option<i32>,
        timed_out: bool,
    },
    OutputRejected,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedProtocol { found, expected } => {
                write!(
                    formatter,
                    "protocol {found} is unsupported; expected {expected}"
                )
            }
            Self::InvalidRequest { field } => {
                write!(formatter, "invalid provider request field {field:?}")
            }
            Self::ProviderUnavailable => formatter.write_str("provider is unavailable"),
            Self::CommandFailed { status, timed_out } => {
                write!(
                    formatter,
                    "provider command failed with status {status:?} (timed_out: {timed_out})"
                )
            }
            Self::OutputRejected => formatter.write_str("provider output was rejected"),
        }
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ProtocolResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ProtocolDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ProviderReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

#[derive(Debug)]
pub enum ProtocolCodecError {
    Json { source: serde_json::Error },
    Protocol { source: ProtocolError },
}

impl std::fmt::Display for ProtocolCodecError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json { source } => write!(formatter, "provider protocol JSON failed: {source}"),
            Self::Protocol { source } => {
                write!(formatter, "provider protocol request failed: {source}")
            }
        }
    }
}

impl std::error::Error for ProtocolCodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json { source } => Some(source),
            Self::Protocol { source } => Some(source),
        }
    }
}

pub fn decode_request(input: &[u8]) -> Result<ProtocolRequest, ProtocolCodecError> {
    let request: ProtocolRequest =
        serde_json::from_slice(input).map_err(|source| ProtocolCodecError::Json { source })?;
    request
        .validate()
        .map_err(|source| ProtocolCodecError::Protocol { source })?;
    Ok(request)
}

pub fn encode_response(response: &ProtocolResponse) -> Result<Vec<u8>, ProtocolCodecError> {
    serde_json::to_vec(response).map_err(|source| ProtocolCodecError::Json { source })
}

pub fn protocol_error(error: &ProviderRuntimeError) -> ProtocolError {
    match error.kind() {
        ProviderRuntimeErrorKind::MissingExecutable => ProtocolError::ProviderUnavailable,
        ProviderRuntimeErrorKind::CommandFailed => {
            if let ProviderRuntimeError::CommandFailed { output, .. } = error {
                ProtocolError::CommandFailed {
                    status: output.status,
                    timed_out: output.timed_out,
                }
            } else {
                unreachable!("error kind and value must agree")
            }
        }
        ProviderRuntimeErrorKind::OutputRejected | ProviderRuntimeErrorKind::OutputUtf8 => {
            ProtocolError::OutputRejected
        }
        ProviderRuntimeErrorKind::Model
        | ProviderRuntimeErrorKind::Detection
        | ProviderRuntimeErrorKind::Template
        | ProviderRuntimeErrorKind::Command
        | ProviderRuntimeErrorKind::Lock
        | ProviderRuntimeErrorKind::Fingerprint => ProtocolError::InvalidRequest {
            field: "provider runtime".to_owned(),
        },
    }
}
