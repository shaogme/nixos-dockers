mod client;
mod protocol_types;

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Instant,
};
use unix_frame::{
    read_json_frame, read_json_frame_until, read_json_frame_until_with_budget, write_json_frame,
    write_json_frame_until, write_json_frame_until_with_budget, FrameBudget, FrameError,
};

pub use client::{IdentityBrokerClient, IdentityBrokerError};
pub use protocol_types::{
    BackendError, BackendState, BackendStatus, ClientMessage, ClientRequest, CommitResult,
    CwdObject, ExecTransactionState, FrameWriteError, HandoffCommand, HelloInfo, IdentityRequest,
    IdentitySource, PeerCredentials, PlanPage, PreparedHandoff, PreparedIdentity, ProtocolError,
    ProtocolLimits, ReceiptSummary, ResolvedIdentity, ServerMessage, ServerResponse,
    WorkspaceStatus, FRAME_TIMEOUT_MS, MAX_ARGV_ITEMS, MAX_ARGV_ITEM_BYTES, MAX_ARGV_TOTAL_BYTES,
    MAX_CWD_BYTES, MAX_ENVIRONMENT_NAMES, MAX_IN_FLIGHT_FRAME_BYTES, MAX_PLAN_PAGE_ACTIONS,
    MAX_REQUEST_FRAME_BYTES, MAX_RESPONSE_FRAME_BYTES, MAX_RUNTIME_INPUTS,
    MAX_RUNTIME_VALUES_TOTAL_BYTES, MAX_RUNTIME_VALUE_BYTES, PROTOCOL_VERSION,
};
pub use unix_frame::FramePermit;

pub fn max_supplementary_groups() -> usize {
    ProtocolLimits::max_supplementary_groups()
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
    read_json_frame(reader, limit).map_err(map_frame_error)
}

pub fn read_message_until<T: for<'de> Deserialize<'de>>(
    stream: &mut UnixStream,
    deadline: Instant,
    limit: usize,
) -> Result<T, ProtocolError> {
    read_json_frame_until(stream, deadline, limit).map_err(map_frame_error)
}

pub fn read_message_until_with_budget<T: for<'de> Deserialize<'de>>(
    stream: &mut UnixStream,
    deadline: Instant,
    limit: usize,
    budget: &FrameBudget,
) -> Result<(T, FramePermit), ProtocolError> {
    read_json_frame_until_with_budget(stream, deadline, limit, budget).map_err(map_frame_error)
}

pub fn write_message<T: Serialize>(
    writer: &mut impl Write,
    message: &T,
) -> Result<(), ProtocolError> {
    write_json_frame(writer, message, MAX_RESPONSE_FRAME_BYTES).map_err(map_frame_error)
}

pub fn write_message_until<T: Serialize>(
    stream: &mut UnixStream,
    message: &T,
    deadline: Instant,
    limit: usize,
) -> Result<usize, FrameWriteError> {
    write_json_frame_until(stream, message, deadline, limit).map_err(|error| FrameWriteError {
        error: map_frame_error(error.error),
        bytes_written: error.bytes_written,
    })
}

pub fn write_message_until_with_budget<T: Serialize>(
    stream: &mut UnixStream,
    message: &T,
    deadline: Instant,
    limit: usize,
    budget: &FrameBudget,
) -> Result<usize, FrameWriteError> {
    write_json_frame_until_with_budget(stream, message, deadline, limit, budget).map_err(|error| {
        FrameWriteError {
            error: map_frame_error(error.error),
            bytes_written: error.bytes_written,
        }
    })
}

fn map_frame_error(error: FrameError) -> ProtocolError {
    match error {
        FrameError::Io(error) => ProtocolError::Io(error),
        FrameError::Json(error) => ProtocolError::invalid_frame(error.to_string()),
        FrameError::Empty => ProtocolError::invalid_frame("frame length must be nonzero"),
        FrameError::TooLarge { length, limit } => ProtocolError::FrameTooLarge { length, limit },
        FrameError::LengthOverflow(_) => ProtocolError::invalid_frame("frame length exceeds u32"),
    }
}

struct RequestValidation<'a> {
    argv: Option<&'a [String]>,
    cwd: Option<&'a PathBuf>,
    inputs: Option<&'a BTreeMap<String, String>>,
    environment: Option<&'a BTreeMap<String, String>>,
}

pub fn validate_message(message: &ClientMessage) -> Result<(), ProtocolError> {
    if message.version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(message.version));
    }
    let fields = match &message.request {
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
            RequestValidation {
                argv: Some(argv),
                cwd: Some(cwd),
                inputs: Some(inputs),
                environment: Some(environment),
            }
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
            RequestValidation {
                argv: None,
                cwd: Some(cwd),
                inputs: Some(inputs),
                environment: Some(environment),
            }
        }
        ClientRequest::Plan { snapshot_id, .. }
            if snapshot_id
                .as_ref()
                .is_some_and(|value| value.is_empty() || value.len() > 128) =>
        {
            return Err(ProtocolError::invalid_frame("plan snapshot id is invalid"));
        }
        _ => RequestValidation {
            argv: None,
            cwd: None,
            inputs: None,
            environment: None,
        },
    };
    if let Some(cwd) = fields.cwd {
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
    if let Some(argv) = fields.argv {
        validate_argv(argv)?;
    }
    if let (Some(inputs), Some(environment)) = (fields.inputs, fields.environment) {
        validate_runtime_values(inputs, environment)?;
    }
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
        read_message, validate_argv, validate_message, write_message, ClientMessage, ClientRequest,
        IdentityRequest, ProtocolError, ProtocolLimits, ServerMessage, ServerResponse,
        MAX_ARGV_ITEMS, MAX_ARGV_ITEM_BYTES, MAX_REQUEST_FRAME_BYTES, MAX_RUNTIME_VALUE_BYTES,
        PROTOCOL_VERSION,
    };
    use serde_json::{from_value, json, to_value};
    use std::{collections::BTreeMap, io::Cursor, path::PathBuf};

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
        let limits = to_value(ProtocolLimits::current()).unwrap();
        let hello = to_value(message(ClientRequest::Hello)).unwrap();
        assert_eq!(
            hello,
            json!({
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
            to_value(prepare).unwrap(),
            json!({
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

        let hello: ServerMessage = from_value(json!({
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

        let identity: ServerMessage = from_value(json!({
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
        let too_large = ((MAX_REQUEST_FRAME_BYTES + 1) as u32).to_be_bytes();
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
        assert!(validate_argv(&[regression_argument]).is_ok());

        let oversized_argument = "x".repeat(MAX_ARGV_ITEM_BYTES + 1);
        assert!(matches!(
            validate_argv(&[oversized_argument]),
            Err(ProtocolError::LimitExceeded {
                class: "argument_too_large",
                ..
            })
        ));

        let oversized_total = vec!["x".repeat(MAX_ARGV_ITEM_BYTES); 17];
        assert!(matches!(
            validate_argv(&oversized_total),
            Err(ProtocolError::LimitExceeded {
                class: "argv_too_large",
                ..
            })
        ));

        let oversized_count = vec![String::new(); MAX_ARGV_ITEMS + 1];
        assert!(matches!(
            validate_argv(&oversized_count),
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
                "é".repeat(MAX_RUNTIME_VALUE_BYTES / 2 + 1),
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
