use super::{
    identity::{BackendIdentity, IdentityRequestOptions},
    inspection::BackendInspection,
    runtime::{BackendRuntime, RuntimeErrors},
    transaction::{PrepareExecOptions, TransactionService},
};
use container_init_protocol::{
    read_message_until_with_budget, validate_message, write_message_until_with_budget,
    BackendError, ClientMessage, ClientRequest, HelloInfo, PeerCredentials, ProtocolError,
    ProtocolLimits, ServerMessage, ServerResponse, FRAME_TIMEOUT_MS, MAX_REQUEST_FRAME_BYTES,
    MAX_RESPONSE_FRAME_BYTES, PROTOCOL_VERSION,
};
use std::{
    io,
    os::unix::net::UnixStream,
    sync::Arc,
    time::{Duration, Instant},
};
use unix_frame::FrameBudget;

const FRAME_TIMEOUT: Duration = Duration::from_millis(FRAME_TIMEOUT_MS);

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
        budget: &FrameBudget,
    ) -> io::Result<()> {
        write_response(stream, response, budget)
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
    let Some((first, _first_permit)) =
        read_client_message(stream, accepted_at + FRAME_TIMEOUT, runtime.frame_budget())
            .transpose_or_response(stream, runtime.frame_budget())?
    else {
        return Ok(());
    };

    match first.request {
        ClientRequest::Status => write_response(
            stream,
            ServerResponse::Status(runtime.status()),
            runtime.frame_budget(),
        ),
        ClientRequest::Plan {
            snapshot_id,
            offset,
        } => {
            let response = BackendInspection::plan_page(&runtime, snapshot_id.as_deref(), offset)
                .unwrap_or_else(ServerResponse::Error);
            write_response(stream, response, runtime.frame_budget())
        }
        ClientRequest::Doctor => write_response(
            stream,
            ServerResponse::Doctor(BackendInspection::doctor(&runtime)),
            runtime.frame_budget(),
        ),
        ClientRequest::CommitExec { prepare_id } => write_response(
            stream,
            TransactionService::commit(&runtime, peer, &prepare_id),
            runtime.frame_budget(),
        ),
        ClientRequest::AbortExec { prepare_id } => write_response(
            stream,
            TransactionService::abort(&runtime, peer, &prepare_id),
            runtime.frame_budget(),
        ),
        ClientRequest::GetExecResult { prepare_id } => write_response(
            stream,
            TransactionService::result(&runtime, peer, &prepare_id),
            runtime.frame_budget(),
        ),
        ClientRequest::Hello => {
            let hello = ServerResponse::Hello(HelloInfo {
                state: runtime.status().state,
                profile: runtime.profile().to_owned(),
                snapshot_id: runtime.snapshot_id().to_owned(),
                runtime_inputs: runtime.runtime_inputs(),
                environment_names: runtime.environment_names(),
                limits: ProtocolLimits::current(),
            });
            write_response(stream, hello, runtime.frame_budget())?;
            let Some((second, _request_permit)) = read_client_message(
                stream,
                Instant::now() + FRAME_TIMEOUT,
                runtime.frame_budget(),
            )
            .transpose_or_response(stream, runtime.frame_budget())?
            else {
                return Ok(());
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
                    &runtime,
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
            };
            write_response(stream, response, runtime.frame_budget())
        }
        ClientRequest::PrepareExec { .. } | ClientRequest::PrepareIdentity { .. } => {
            write_response(
                stream,
                ServerResponse::Error(RuntimeErrors::backend(
                    "protocol",
                    false,
                    "execution requests require hello as the first message on this connection",
                )),
                runtime.frame_budget(),
            )
        }
    }
}

trait ReadResultExt<T> {
    fn transpose_or_response(
        self,
        stream: &mut UnixStream,
        budget: &FrameBudget,
    ) -> io::Result<Option<T>>;
}

impl<T> ReadResultExt<T> for Result<Option<T>, ProtocolError> {
    fn transpose_or_response(
        self,
        stream: &mut UnixStream,
        budget: &FrameBudget,
    ) -> io::Result<Option<T>> {
        match self {
            Ok(value) => Ok(value),
            Err(error) => {
                write_response(
                    stream,
                    ServerResponse::Error(protocol_error_response(error)),
                    budget,
                )?;
                Ok(None)
            }
        }
    }
}

fn read_client_message(
    stream: &mut UnixStream,
    deadline: Instant,
    budget: &FrameBudget,
) -> Result<Option<(ClientMessage, container_init_protocol::FramePermit)>, ProtocolError> {
    let (message, permit) =
        match read_message_until_with_budget(stream, deadline, MAX_REQUEST_FRAME_BYTES, budget) {
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
    Ok(Some((message, permit)))
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

fn authorized_peer(runtime: &BackendRuntime, peer: PeerCredentials) -> bool {
    authorized_peer_uid(runtime.startup_identity().uid, peer)
}

fn authorized_peer_uid(startup_uid: u32, peer: PeerCredentials) -> bool {
    peer.pid > 0 && (peer.uid == 0 || peer.uid == startup_uid)
}

fn write_response(
    stream: &mut UnixStream,
    response: ServerResponse,
    budget: &FrameBudget,
) -> io::Result<()> {
    let message = ServerMessage {
        version: PROTOCOL_VERSION,
        response,
    };
    match write_message_until_with_budget(
        stream,
        &message,
        Instant::now() + FRAME_TIMEOUT,
        MAX_RESPONSE_FRAME_BYTES,
        budget,
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
                write_message_until_with_budget(
                    stream,
                    &fallback,
                    Instant::now() + FRAME_TIMEOUT,
                    MAX_RESPONSE_FRAME_BYTES,
                    budget,
                )
                .map(|_| ())
                .map_err(|error| error.error.as_io_error())
            }
            error => Err(error.as_io_error()),
        },
    }
}
