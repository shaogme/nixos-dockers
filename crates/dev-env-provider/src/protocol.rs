use crate::{
    context::ProviderContext,
    error::{ProviderRuntimeError, ProviderRuntimeErrorKind},
};
use dev_env_model::{ProviderReceipt, ValueTree};
use serde::{Deserialize, Serialize};
use serde_json::{from_slice, to_vec, Error as JsonError};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt::{Display, Formatter, Result as FmtResult},
};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolOperation {
    Detect,
    Prepare,
    Shellenv,
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProtocolRequest {
    protocol: u32,
    op: ProtocolOperation,
    context: ProviderContext,
    config: ValueTree,
    #[serde(default)]
    argv: Vec<String>,
}

impl ProtocolRequest {
    pub fn new(
        protocol: u32,
        op: ProtocolOperation,
        context: ProviderContext,
        config: ValueTree,
        argv: Vec<String>,
    ) -> Self {
        Self {
            protocol,
            op,
            context,
            config,
            argv,
        }
    }

    pub fn with_protocol(mut self, protocol: u32) -> Self {
        self.protocol = protocol;
        self
    }

    pub fn protocol(&self) -> u32 {
        self.protocol
    }

    pub fn operation(&self) -> ProtocolOperation {
        self.op
    }

    pub fn context(&self) -> &ProviderContext {
        &self.context
    }

    pub fn context_mut(&mut self) -> &mut ProviderContext {
        &mut self.context
    }

    pub fn config(&self) -> &ValueTree {
        &self.config
    }

    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedProtocol {
                found: self.protocol,
                expected: PROTOCOL_VERSION,
            });
        }
        if self.context.shell().is_empty() {
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

impl Display for ProtocolError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
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

impl Error for ProtocolError {}

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
    Json { source: JsonError },
    Protocol { source: ProtocolError },
}

impl Display for ProtocolCodecError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Json { source } => write!(formatter, "provider protocol JSON failed: {source}"),
            Self::Protocol { source } => {
                write!(formatter, "provider protocol request failed: {source}")
            }
        }
    }
}

impl Error for ProtocolCodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json { source } => Some(source),
            Self::Protocol { source } => Some(source),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProtocolCodec;

impl ProtocolCodec {
    pub fn decode_request(input: &[u8]) -> Result<ProtocolRequest, ProtocolCodecError> {
        let request: ProtocolRequest =
            from_slice(input).map_err(|source| ProtocolCodecError::Json { source })?;
        request
            .validate()
            .map_err(|source| ProtocolCodecError::Protocol { source })?;
        Ok(request)
    }

    pub fn encode_response(response: &ProtocolResponse) -> Result<Vec<u8>, ProtocolCodecError> {
        to_vec(response).map_err(|source| ProtocolCodecError::Json { source })
    }

    pub fn protocol_error(error: &ProviderRuntimeError) -> ProtocolError {
        match error.kind() {
            ProviderRuntimeErrorKind::MissingExecutable => ProtocolError::ProviderUnavailable,
            ProviderRuntimeErrorKind::CommandFailed => {
                if let ProviderRuntimeError::CommandFailed { output, .. } = error {
                    ProtocolError::CommandFailed {
                        status: output.status(),
                        timed_out: output.timed_out(),
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
            | ProviderRuntimeErrorKind::Fingerprint => ProtocolError::InvalidRequest {
                field: "provider runtime".to_owned(),
            },
        }
    }
}
