use super::{
    identity::{BackendIdentity, IdentityRequestOptions},
    inspection::BackendInspection,
    runtime::{BackendRuntime, RuntimeErrors},
    transaction::{PrepareExecOptions, TransactionService},
};
use container_init_protocol::{
    read_message_until, validate_message, write_message_until, BackendError, ClientMessage,
    ClientRequest, HelloInfo, PeerCredentials, ProtocolError, ServerMessage, ServerResponse,
    MAX_REQUEST_FRAME_BYTES, MAX_RESPONSE_FRAME_BYTES, PROTOCOL_VERSION,
};
use std::{
    io,
    os::unix::net::UnixStream,
    sync::Arc,
    time::{Duration, Instant},
};

const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct ConnectionService;

impl ConnectionService {
    pub(super) fn serve(
        stream: &mut UnixStream,
        runtime: Arc<BackendRuntime>,
        peer: PeerCredentials,
        accepted_at: Instant,
    ) -> io::Result<()> {
        serve_connection(stream, runtime, peer, accepted_at)
    }

    pub(super) fn write_response(
        stream: &mut UnixStream,
        response: ServerResponse,
    ) -> io::Result<()> {
        write_response(stream, response)
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

fn serve_connection(
    stream: &mut UnixStream,
    runtime: Arc<BackendRuntime>,
    peer: PeerCredentials,
    accepted_at: Instant,
) -> io::Result<()> {
    let first = match read_client_message(stream, accepted_at + FRAME_TIMEOUT) {
        Ok(Some(message)) => message,
        Ok(None) => return Ok(()),
        Err(error) => {
            return write_response(
                stream,
                ServerResponse::Error(protocol_error_response(error)),
            )
        }
    };
    match first.request {
        ClientRequest::Status => write_response(stream, ServerResponse::Status(runtime.status())),
        ClientRequest::Plan {
            snapshot_id,
            offset,
        } => {
            let response = BackendInspection::plan_page(&runtime, snapshot_id.as_deref(), offset)
                .unwrap_or_else(ServerResponse::Error);
            write_response(stream, response)
        }
        ClientRequest::Doctor => write_response(
            stream,
            ServerResponse::Doctor(BackendInspection::doctor(&runtime)),
        ),
        ClientRequest::CommitExec { prepare_id } => write_response(
            stream,
            TransactionService::commit(&runtime, peer, &prepare_id),
        ),
        ClientRequest::AbortExec { prepare_id } => write_response(
            stream,
            TransactionService::abort(&runtime, peer, &prepare_id),
        ),
        ClientRequest::GetExecResult { prepare_id } => write_response(
            stream,
            TransactionService::result(&runtime, peer, &prepare_id),
        ),
        ClientRequest::Hello => {
            let hello = ServerResponse::Hello(HelloInfo {
                state: runtime.status().state,
                profile: runtime.profile().to_owned(),
                snapshot_id: runtime.snapshot_id().to_owned(),
                runtime_inputs: runtime.runtime_inputs(),
                environment_names: runtime.environment_names(),
            });
            write_response(stream, hello)?;
            let second = match read_client_message(stream, Instant::now() + FRAME_TIMEOUT) {
                Ok(Some(message)) => message,
                Ok(None) => return Ok(()),
                Err(error) => {
                    return write_response(
                        stream,
                        ServerResponse::Error(protocol_error_response(error)),
                    )
                }
            };
            let response = match second.request {
                ClientRequest::PrepareExec {
                    argv,
                    cwd,
                    inputs,
                    environment,
                    request_budget_ms,
                } => match TransactionService::prepare(
                    &runtime,
                    peer,
                    PrepareExecOptions {
                        argv, cwd, inputs, environment,
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
                } => match BackendIdentity::prepare(&runtime, peer, IdentityRequestOptions { requested, cwd, inputs, environment }) {
                    Ok(identity) => ServerResponse::Identity(identity),
                    Err(error) => ServerResponse::Error(error),
                },
                ClientRequest::UnsupportedV2Exec { .. } => ServerResponse::Error(RuntimeErrors::backend(
                    "unsupported_version",
                    false,
                    "the v2 exec operation was removed; use protocol version 3 PrepareExec",
                )),
                _ => ServerResponse::Error(RuntimeErrors::backend(
                    "protocol",
                    false,
                    "hello must be followed by exactly one prepare_exec or prepare_identity request",
                )),
            };
            write_response(stream, response)
        }
        ClientRequest::PrepareExec { .. }
        | ClientRequest::UnsupportedV2Exec { .. }
        | ClientRequest::PrepareIdentity { .. } => write_response(
            stream,
            ServerResponse::Error(RuntimeErrors::backend(
                "protocol",
                false,
                "execution requests require hello as the first message on this connection",
            )),
        ),
    }
}

fn read_client_message(
    stream: &mut UnixStream,
    deadline: Instant,
) -> Result<Option<ClientMessage>, ProtocolError> {
    let message = match read_message_until(stream, deadline, MAX_REQUEST_FRAME_BYTES) {
        Ok(message) => message,
        Err(ProtocolError::Io(error))
            if matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::TimedOut
            ) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    validate_message(&message)?;
    Ok(Some(message))
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
            &format!("request frame length {length} exceeds the {limit} byte limit"),
        ),
        other => RuntimeErrors::backend("protocol", false, &other.to_string()),
    }
}

fn authorized_peer(runtime: &BackendRuntime, peer: PeerCredentials) -> bool {
    authorized_peer_uid(runtime.startup_identity().uid, peer)
}

fn authorized_peer_uid(startup_uid: u32, peer: PeerCredentials) -> bool {
    peer.pid > 0 && (peer.uid == 0 || peer.uid == startup_uid)
}

fn write_response(stream: &mut UnixStream, response: ServerResponse) -> io::Result<()> {
    let message = ServerMessage {
        version: PROTOCOL_VERSION,
        response,
    };
    match write_message_until(
        stream,
        &message,
        Instant::now() + FRAME_TIMEOUT,
        MAX_RESPONSE_FRAME_BYTES,
    ) {
        Ok(_) => Ok(()),
        Err(failure) => match failure.error {
            ProtocolError::FrameTooLarge { .. } if failure.bytes_written == 0 => {
                let fallback = ServerMessage {
                    version: PROTOCOL_VERSION,
                    response: ServerResponse::Error(RuntimeErrors::backend(
                        "response_too_large",
                        false,
                        "backend response exceeded the response frame limit",
                    )),
                };
                write_message_until(
                    stream,
                    &fallback,
                    Instant::now() + FRAME_TIMEOUT,
                    MAX_RESPONSE_FRAME_BYTES,
                )
                .map(|_| ())
                .map_err(|error| error.error.as_io_error())
            }
            error => Err(error.as_io_error()),
        },
    }
}
