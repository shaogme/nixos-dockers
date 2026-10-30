use super::{
    identity::{BackendIdentity, IdentityRequestOptions},
    inspection::BackendInspection,
    runtime::{BackendRuntime, RuntimeErrors},
    transaction::{PrepareExecOptions, TransactionService},
};
use container_init_protocol::{
    validate_message, write_message, BackendError, ClientMessage, ClientRequest, HelloInfo,
    PeerCredentials, ProtocolError, ProtocolLimits, ServerMessage, ServerResponse,
    MAX_RESPONSE_FRAME_BYTES, PROTOCOL_VERSION,
};
use serde_json::from_slice;
use std::{io, os::unix::net::UnixStream, time::Duration};

pub(super) struct ConnectionService;

impl ConnectionService {
    pub(super) fn decode(payload: &[u8]) -> Result<ClientMessage, ProtocolError> {
        let message =
            from_slice(payload).map_err(|error| ProtocolError::invalid_frame(error.to_string()))?;
        validate_message(&message)?;
        Ok(message)
    }

    pub(super) fn dispatch(
        runtime: &mut BackendRuntime,
        peer: PeerCredentials,
        message: ClientMessage,
        hello_completed: bool,
    ) -> (ServerResponse, bool) {
        if hello_completed {
            return (prepare_response(runtime, peer, message.request), false);
        }

        match message.request {
            ClientRequest::Status => (ServerResponse::Status(runtime.status()), false),
            ClientRequest::Plan {
                snapshot_id,
                offset,
            } => (
                BackendInspection::plan_page(runtime, snapshot_id.as_deref(), offset)
                    .unwrap_or_else(ServerResponse::Error),
                false,
            ),
            ClientRequest::Doctor => (
                ServerResponse::Doctor(BackendInspection::doctor(runtime)),
                false,
            ),
            ClientRequest::CommitExec { prepare_id } => (
                TransactionService::commit(runtime, peer, &prepare_id),
                false,
            ),
            ClientRequest::AbortExec { prepare_id } => {
                (TransactionService::abort(runtime, peer, &prepare_id), false)
            }
            ClientRequest::GetExecResult { prepare_id } => (
                TransactionService::result(runtime, peer, &prepare_id),
                false,
            ),
            ClientRequest::Hello => (
                ServerResponse::Hello(HelloInfo {
                    state: runtime.status().state,
                    profile: runtime.profile().to_owned(),
                    snapshot_id: runtime.snapshot_id().to_owned(),
                    runtime_inputs: runtime.runtime_inputs(),
                    environment_names: runtime.environment_names(),
                    limits: ProtocolLimits::current(),
                }),
                true,
            ),
            ClientRequest::PrepareExec { .. } | ClientRequest::PrepareIdentity { .. } => (
                ServerResponse::Error(RuntimeErrors::backend(
                    "protocol",
                    false,
                    "execution requests require hello as the first message on this connection",
                )),
                false,
            ),
        }
    }

    pub(super) fn encode(response: ServerResponse) -> Vec<u8> {
        let message = ServerMessage {
            version: PROTOCOL_VERSION,
            response,
        };
        let mut frame = Vec::new();
        if write_message(&mut frame, &message).is_ok() {
            return frame;
        }
        let fallback = ServerMessage {
            version: PROTOCOL_VERSION,
            response: ServerResponse::Error(RuntimeErrors::backend(
                "response_too_large",
                false,
                "backend response exceeded the response frame limit",
            )),
        };
        let mut frame = Vec::with_capacity(MAX_RESPONSE_FRAME_BYTES.min(512));
        let _ = write_message(&mut frame, &fallback);
        frame
    }

    pub(super) fn protocol_error_response(error: ProtocolError) -> BackendError {
        protocol_error_response(error)
    }

    pub(super) fn set_cloexec(stream: &UnixStream) -> io::Result<()> {
        BackendIdentity::set_cloexec(stream)
    }

    pub(super) fn is_authorized(runtime: &BackendRuntime, peer: PeerCredentials) -> bool {
        authorized_peer(runtime, peer)
    }

    #[cfg(test)]
    pub(super) fn peer_uid_authorized(startup_uid: u32, peer: PeerCredentials) -> bool {
        authorized_peer_uid(startup_uid, peer)
    }
}

fn prepare_response(
    runtime: &mut BackendRuntime,
    peer: PeerCredentials,
    request: ClientRequest,
) -> ServerResponse {
    match request {
        ClientRequest::PrepareExec {
            argv,
            cwd,
            inputs,
            environment,
            request_budget_ms,
        } => match TransactionService::prepare(
            runtime,
            peer,
            PrepareExecOptions {
                argv,
                cwd,
                inputs,
                environment,
                request_budget: Duration::from_millis(request_budget_ms),
            },
        ) {
            Ok(prepared) => ServerResponse::PreparedExec(prepared),
            Err(error) => ServerResponse::Error(error),
        },
        ClientRequest::PrepareIdentity {
            cwd,
            inputs,
            environment,
            requested,
        } => match BackendIdentity::prepare(
            runtime,
            peer,
            IdentityRequestOptions {
                requested,
                cwd,
                inputs,
                environment,
            },
        ) {
            Ok(identity) => ServerResponse::Identity(identity),
            Err(error) => ServerResponse::Error(error),
        },
        _ => ServerResponse::Error(RuntimeErrors::backend(
            "protocol",
            false,
            "hello must be followed by exactly one prepare_exec or prepare_identity request",
        )),
    }
}

fn authorized_peer(runtime: &BackendRuntime, peer: PeerCredentials) -> bool {
    authorized_peer_uid(runtime.startup_identity().uid, peer)
}

fn authorized_peer_uid(startup_uid: u32, peer: PeerCredentials) -> bool {
    peer.pid > 0 && (peer.uid == 0 || peer.uid == startup_uid)
}

fn protocol_error_response(error: ProtocolError) -> BackendError {
    match error {
        ProtocolError::UnsupportedVersion(version) => RuntimeErrors::backend(
            "unsupported_version",
            false,
            &format!("protocol version {version} is not supported; expected {PROTOCOL_VERSION}"),
        ),
        ProtocolError::FrameTooLarge { length, limit } => RuntimeErrors::backend(
            "request_too_large",
            false,
            &format!("request frame is {length} bytes; maximum is {limit} bytes"),
        ),
        ProtocolError::LimitExceeded {
            class,
            field,
            observed,
            limit,
        } => RuntimeErrors::backend(
            class,
            false,
            &format!("{field} is {observed}; maximum is {limit}"),
        ),
        other => RuntimeErrors::backend("protocol", false, &other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionService;
    use container_init_protocol::{
        read_message, BackendError, ServerMessage, ServerResponse, MAX_RESPONSE_FRAME_BYTES,
    };
    use std::io::Cursor;

    #[test]
    fn oversized_response_is_replaced_by_a_bounded_error_frame() {
        let response = ServerResponse::Error(BackendError {
            class: "test".to_owned(),
            retryable: false,
            message: "x".repeat(MAX_RESPONSE_FRAME_BYTES + 1),
            action_id: None,
            path: None,
        });
        let frame = ConnectionService::encode(response);
        assert!(frame.len() <= MAX_RESPONSE_FRAME_BYTES + 4);

        let decoded: ServerMessage = read_message(&mut Cursor::new(frame)).unwrap();
        assert!(matches!(
            decoded.response,
            ServerResponse::Error(error) if error.class == "response_too_large"
        ));
    }
}
