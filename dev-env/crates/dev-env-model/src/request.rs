use crate::identity::{IdentityRequest, IdentityRequestError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Operations that can ask the backend for a materialized environment.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestMode {
    Exec,
    Shell,
    LoginShell,
    Shim,
    Print,
}

/// Per-request facts accepted by a backend. Configuration and executable
/// policy are intentionally absent: those come from ConfigSnapshot.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestContext {
    pub request_id: String,
    pub mode: RequestMode,
    pub cwd: PathBuf,
    pub shell: String,
    /// Arguments that belong to the selected shell invocation. They are
    /// request data and do not alter the snapshot or provider policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shell_args: Vec<String>,
    #[serde(default)]
    pub requested_identity: IdentityRequest,
    #[serde(default)]
    pub typed_runtime_inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub filtered_ambient_environment: BTreeMap<String, String>,
}

/// The protocol uses the name PrepareRequest for the same context shape.
pub type PrepareRequest = RequestContext;

impl RequestContext {
    pub fn new(
        request_id: impl Into<String>,
        mode: RequestMode,
        cwd: impl Into<PathBuf>,
        shell: impl Into<String>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            mode,
            cwd: cwd.into(),
            shell: shell.into(),
            shell_args: Vec::new(),
            requested_identity: IdentityRequest::Peer,
            typed_runtime_inputs: BTreeMap::new(),
            filtered_ambient_environment: BTreeMap::new(),
        }
    }

    pub fn with_identity(mut self, identity: IdentityRequest) -> Self {
        self.requested_identity = identity;
        self
    }

    pub fn with_shell_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.shell_args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_runtime_input(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.typed_runtime_inputs.insert(name.into(), value.into());
        self
    }

    pub fn with_ambient_environment(mut self, environment: BTreeMap<String, String>) -> Self {
        self.filtered_ambient_environment = environment;
        self
    }

    pub fn validate(&self) -> Result<(), RequestValidationError> {
        if self.request_id.is_empty()
            || self.request_id.len() > 128
            || !self
                .request_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(RequestValidationError::InvalidRequestId);
        }
        validate_absolute(&self.cwd)?;
        if self.shell.is_empty() || self.shell.contains('\0') {
            return Err(RequestValidationError::InvalidShell);
        }
        if self.shell_args.len() > 128 || self.shell_args.iter().any(|arg| arg.contains('\0')) {
            return Err(RequestValidationError::TooManyShellArguments);
        }
        if self.shell_args.iter().map(String::len).sum::<usize>() > 64 * 1024 {
            return Err(RequestValidationError::TooManyShellArguments);
        }
        self.requested_identity
            .validate()
            .map_err(RequestValidationError::Identity)?;
        if self.typed_runtime_inputs.len() > 64 {
            return Err(RequestValidationError::TooManyRuntimeInputs);
        }
        validate_values(&self.typed_runtime_inputs)?;
        validate_values(&self.filtered_ambient_environment)?;
        Ok(())
    }
}

#[derive(Debug)]
pub enum RequestValidationError {
    InvalidRequestId,
    InvalidCwd(PathBuf),
    InvalidShell,
    Identity(IdentityRequestError),
    TooManyRuntimeInputs,
    TooManyShellArguments,
    InvalidEnvironmentName(String),
    NulValue(String),
}

impl std::fmt::Display for RequestValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequestId => formatter.write_str("request id is invalid"),
            Self::InvalidCwd(path) => {
                write!(
                    formatter,
                    "request cwd must be absolute: {}",
                    path.display()
                )
            }
            Self::InvalidShell => formatter.write_str("request shell is invalid"),
            Self::Identity(source) => source.fmt(formatter),
            Self::TooManyRuntimeInputs => formatter.write_str("too many runtime inputs"),
            Self::TooManyShellArguments => formatter.write_str("too many shell arguments"),
            Self::InvalidEnvironmentName(name) => {
                write!(formatter, "request environment name {name:?} is invalid")
            }
            Self::NulValue(name) => write!(formatter, "request value {name:?} contains NUL"),
        }
    }
}

impl std::error::Error for RequestValidationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Identity(source) => Some(source),
            _ => None,
        }
    }
}

fn validate_absolute(path: &Path) -> Result<(), RequestValidationError> {
    if path.as_os_str().is_empty() || !path.is_absolute() || path.to_string_lossy().contains('\0') {
        return Err(RequestValidationError::InvalidCwd(path.to_path_buf()));
    }
    Ok(())
}

fn validate_values(values: &BTreeMap<String, String>) -> Result<(), RequestValidationError> {
    let total_bytes = values.iter().try_fold(0_usize, |total, (name, value)| {
        let mut bytes = name.bytes();
        let valid_name = matches!(bytes.next(), Some(byte) if byte.is_ascii_uppercase() || byte == b'_')
            && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
        if !valid_name {
            return Err(RequestValidationError::InvalidEnvironmentName(name.clone()));
        }
        if value.contains('\0') {
            return Err(RequestValidationError::NulValue(name.clone()));
        }
        total
            .checked_add(name.len())
            .and_then(|size| size.checked_add(value.len()))
            .ok_or_else(|| RequestValidationError::NulValue(name.clone()))
    })?;
    if total_bytes > 64 * 1024 {
        return Err(RequestValidationError::TooManyRuntimeInputs);
    }
    Ok(())
}
