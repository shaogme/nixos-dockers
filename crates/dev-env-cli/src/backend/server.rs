mod coordinator;
mod initial_runtime;

use {
    crate::{
        BackendInitial, BackendSupport, BackendTransport, CliError, CliOptions, ConfigApi,
        ConnectionRequestError, IdentityBroker, CONNECTION_TIMEOUT, ENDPOINT_DISPLAY,
    },
    coordinator::{BackendStateConfig, CoordinatorEvent, RunSummary},
    dev_env_core::Materializer,
    dev_env_model::{
        BackendError, BackendProtocolError, BackendRequest, BackendRequestMessage, BackendResponse,
        BackendResponseMessage, ConfigSnapshot, Generation, IdentityPeer, RequestContext,
    },
    dev_env_provider::{ProcessExecutor, ProviderProcessSupervisor},
    std::{
        io,
        os::unix::net::UnixStream,
        process::ExitStatus,
        sync::{
            mpsc::{self, Sender},
            Arc,
        },
        thread,
        time::{Duration, Instant},
    },
};

#[cfg(test)]
use {
    crate::BACKEND_ABSTRACT_ENDPOINT,
    dev_env_model::DEVENV_BACKEND_PROTOCOL_VERSION,
    libc::{geteuid, sa_family_t},
    std::{io::Read, mem::size_of, process::id, slice::from_raw_parts},
};

#[derive(Clone)]
pub(super) struct BackendRuntime {
    sender: Sender<CoordinatorEvent>,
    allowed_peer_uid: u32,
}

pub(super) struct BackendServer;

impl BackendServer {
    pub(super) fn run(initial: BackendInitial, options: CliOptions) -> Result<i32, CliError> {
        run(initial, options)
    }
}

fn run(initial: BackendInitial, options: CliOptions) -> Result<i32, CliError> {
    let allowed_peer_uid = BackendSupport::configured_allowed_peer_uid()?;
    let listener = match BackendTransport::bind() {
        Ok(listener) => listener,
        Err(source) if source.kind() == io::ErrorKind::AddrInUse => {
            if let Ok(response) = BackendSupport::request(BackendRequest::Status) {
                BackendSupport::print_response(response)?;
                return Ok(0);
            }
            return Err(CliError::Backend(format!(
                "BACKEND-INSTANCE-CONFLICT: abstract endpoint {ENDPOINT_DISPLAY} is occupied but unavailable"
            )));
        }
        Err(source) => {
            return Err(CliError::Backend(format!(
                "could not bind abstract backend endpoint {ENDPOINT_DISPLAY}: {source}"
            )))
        }
    };
    listener.set_nonblocking(true).map_err(|source| {
        CliError::Backend(format!(
            "could not configure abstract backend endpoint: {source}"
        ))
    })?;

    let loaded = ConfigApi::load(&options)?;
    let snapshot = BackendSupport::make_snapshot(&loaded, Generation::INITIAL)?;
    let process_supervisor = ProviderProcessSupervisor::new();
    let process_executor = process_supervisor.executor();
    let materializer = Materializer::try_new_with_process_executor(
        loaded.config().clone(),
        process_executor.clone(),
    )
    .map_err(CliError::Core)?;
    IdentityBroker::check().map_err(|error| CliError::Backend(error.to_string()))?;

    let runtime = BackendRuntime::start(
        options,
        snapshot,
        materializer,
        process_executor,
        allowed_peer_uid,
    );
    BackendSupport::install_signal_handlers();
    let initial_runtime = Arc::new(runtime.clone());
    thread::spawn(move || initial_runtime::InitialRuntime::run(initial_runtime, initial));

    while !runtime.is_stopping() && !BackendSupport::signal_requested() {
        match listener.accept() {
            Ok((stream, _)) => {
                if !runtime.connection_opened() {
                    continue;
                }
                let _ = stream.set_read_timeout(Some(CONNECTION_TIMEOUT));
                let _ = stream.set_write_timeout(Some(CONNECTION_TIMEOUT));
                let runtime = runtime.clone();
                thread::spawn(move || {
                    serve_connection(runtime.clone(), stream);
                    runtime.connection_closed();
                });
            }
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                runtime.fail(format!("backend listener failed: {source}"));
                break;
            }
        }
    }
    if BackendSupport::signal_requested() {
        runtime.request_stop();
    }
    let shutdown_deadline = Instant::now() + CONNECTION_TIMEOUT;
    while runtime.active_connections() > 0 && Instant::now() < shutdown_deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let summary = runtime.shutdown();
    match summary.failure {
        Some(failure) if failure.class == "DEVENV-E-PROVIDER" => {
            Err(CliError::BackendProvider(failure.message))
        }
        Some(failure) => Err(CliError::Backend(failure.message)),
        None => Ok(summary.exit_code),
    }
}

impl BackendRuntime {
    fn start(
        options: CliOptions,
        snapshot: ConfigSnapshot,
        materializer: Materializer,
        process_executor: ProcessExecutor,
        allowed_peer_uid: u32,
    ) -> Self {
        let (sender, receiver) = mpsc::channel();
        let state = coordinator::BackendState::new(BackendStateConfig {
            sender: sender.clone(),
            options,
            snapshot,
            materializer,
            process_executor,
            allowed_peer_uid,
        });
        thread::Builder::new()
            .name("dev-env-backend-coordinator".to_owned())
            .spawn(move || state.run(receiver))
            .expect("could not spawn backend coordinator");
        Self {
            sender,
            allowed_peer_uid,
        }
    }

    pub(super) fn current_snapshot(&self) -> Arc<ConfigSnapshot> {
        let (reply, response) = mpsc::sync_channel(1);
        let _ = self.sender.send(CoordinatorEvent::CurrentSnapshot(reply));
        response.recv().expect("backend coordinator stopped")
    }

    pub(super) fn prepare_initial(&self, context: &RequestContext) -> BackendResponseMessage {
        let request = BackendRequestMessage::new(
            context.request_id(),
            BackendRequest::Prepare {
                context: context.clone(),
            },
        );
        let response = self.request(
            request,
            BackendSupport::peer_credentials_from_current_thread(),
        );
        self.request_finished();
        response
    }

    pub(super) fn fail(&self, message: String) {
        self.fail_with_class("DEVENV-E-BACKEND", message);
    }

    pub(super) fn fail_with_class(&self, class: impl Into<String>, message: String) {
        let _ = self.sender.send(CoordinatorEvent::Fail {
            class: class.into(),
            message,
        });
    }

    pub(super) fn mark_initial_running(&self, pid: u32) {
        let _ = self.sender.send(CoordinatorEvent::InitialRunning(pid));
    }

    pub(super) fn mark_initial_exited(&self, status: ExitStatus) {
        let _ = self.sender.send(CoordinatorEvent::InitialExited(
            BackendSupport::exit_status_code(status),
        ));
    }

    pub(super) fn finish_initial_runtime(&self) {
        let _ = self.sender.send(CoordinatorEvent::InitialFinished);
    }

    fn connection_opened(&self) -> bool {
        let (reply, response) = mpsc::sync_channel(1);
        let _ = self.sender.send(CoordinatorEvent::ConnectionOpened(reply));
        response.recv().unwrap_or(false)
    }

    fn connection_closed(&self) {
        let _ = self.sender.send(CoordinatorEvent::ConnectionClosed);
    }

    fn request(
        &self,
        request: BackendRequestMessage,
        peer: IdentityPeer,
    ) -> BackendResponseMessage {
        let (reply, response) = mpsc::sync_channel(1);
        if self
            .sender
            .send(CoordinatorEvent::Request {
                request,
                peer,
                reply,
            })
            .is_err()
        {
            return BackendResponseMessage::new(
                "coordinator-stopped",
                BackendResponse::Error(BackendError::new(
                    "backend_stopped",
                    true,
                    "backend coordinator has stopped",
                )),
            );
        }
        response.recv().expect("backend coordinator stopped")
    }

    fn request_finished(&self) {
        let _ = self.sender.send(CoordinatorEvent::RequestFinished);
    }

    fn authentication_rejected(
        &self,
        request_id: String,
        peer: IdentityPeer,
    ) -> BackendResponseMessage {
        let (reply, response) = mpsc::sync_channel(1);
        let _ = self.sender.send(CoordinatorEvent::AuthenticationRejected {
            request_id,
            peer,
            reply,
        });
        response.recv().expect("backend coordinator stopped")
    }

    fn connection_protocol_error(&self, version: u16) -> BackendResponseMessage {
        let (reply, response) = mpsc::sync_channel(1);
        let _ = self
            .sender
            .send(CoordinatorEvent::ProtocolRejected { version, reply });
        response.recv().expect("backend coordinator stopped")
    }

    fn is_stopping(&self) -> bool {
        let (reply, response) = mpsc::sync_channel(1);
        let _ = self.sender.send(CoordinatorEvent::CheckStopping(reply));
        response.recv().unwrap_or(true)
    }

    fn active_connections(&self) -> usize {
        let (reply, response) = mpsc::sync_channel(1);
        let _ = self.sender.send(CoordinatorEvent::ActiveConnections(reply));
        response.recv().unwrap_or(0)
    }

    fn request_stop(&self) {
        let _ = self.sender.send(CoordinatorEvent::RequestStop);
    }

    fn shutdown(&self) -> RunSummary {
        let (reply, response) = mpsc::sync_channel(1);
        let _ = self.sender.send(CoordinatorEvent::Shutdown(reply));
        response.recv().expect("backend coordinator stopped")
    }
}

fn serve_connection(runtime: BackendRuntime, mut stream: UnixStream) {
    let (request, peer) =
        match BackendSupport::read_authenticated_request(&mut stream, runtime.allowed_peer_uid) {
            Ok(request) => request,
            Err(ConnectionRequestError::Unauthorized(peer)) => {
                let response =
                    runtime.authentication_rejected("peer-authentication".to_owned(), peer);
                let _ = response.write_to(&mut stream);
                return;
            }
            Err(ConnectionRequestError::PeerCredentials(_source)) => return,
            Err(ConnectionRequestError::Protocol(BackendProtocolError::UnsupportedVersion(
                version,
            ))) => {
                let response = runtime.connection_protocol_error(version);
                let _ = response.write_to(&mut stream);
                return;
            }
            Err(ConnectionRequestError::Protocol(_source)) => return,
        };
    let response = runtime.request(request, peer);
    let _ = response.write_to(&mut stream);
    runtime.request_finished();
}

#[cfg(test)]
struct ReadCounter {
    reads: usize,
}

#[cfg(test)]
impl Read for ReadCounter {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reads += 1;
        buffer[0] = b'!';
        Ok(1)
    }
}

#[cfg(test)]
#[test]
fn abstract_endpoint_has_linux_abstract_namespace_encoding() {
    let (address, length) = BackendTransport::sockaddr_for(BACKEND_ABSTRACT_ENDPOINT).unwrap();
    let encoded = unsafe {
        from_raw_parts(
            address.sun_path.as_ptr().cast::<u8>(),
            BACKEND_ABSTRACT_ENDPOINT.len(),
        )
    };
    assert_eq!(encoded, BACKEND_ABSTRACT_ENDPOINT);
    assert_eq!(
        length as usize,
        size_of::<sa_family_t>() + BACKEND_ABSTRACT_ENDPOINT.len()
    );
    assert_eq!(DEVENV_BACKEND_PROTOCOL_VERSION, 5);
}

#[cfg(test)]
#[test]
fn abstract_transport_connects_without_a_filesystem_socket() {
    let endpoint = format!("\0dev-env-test-{}", id());
    let listener = BackendTransport::bind_endpoint(endpoint.as_bytes()).unwrap();
    let client = BackendTransport::connect_endpoint(endpoint.as_bytes()).unwrap();
    let (server, _) = listener.accept().unwrap();
    assert!(listener.local_addr().unwrap().as_pathname().is_none());
    assert_eq!(
        BackendSupport::peer_credentials(&server).unwrap().uid,
        unsafe { geteuid() }
    );
    drop(client);
}

#[cfg(test)]
#[test]
fn uid_policy_does_not_depend_on_group_membership() {
    let peer = IdentityPeer {
        pid: 12,
        uid: 1000,
        gid: 4242,
    };
    assert!(BackendSupport::peer_is_authorized(peer, 1000));
    assert!(!BackendSupport::peer_is_authorized(peer, 1001));
    assert!(BackendSupport::peer_is_authorized(
        IdentityPeer { uid: 0, ..peer },
        1001
    ));
}

#[cfg(test)]
#[test]
fn unauthorized_peer_is_rejected_before_frame_read() {
    let peer = IdentityPeer {
        pid: 12,
        uid: 1000,
        gid: 4242,
    };
    let mut reader = ReadCounter { reads: 0 };
    let result = BackendSupport::read_request_for_peer(&mut reader, peer, 1001);
    assert!(matches!(result, Err(ConnectionRequestError::Unauthorized(found)) if found == peer));
    assert_eq!(reader.reads, 0);
}

#[cfg(test)]
#[test]
fn legacy_protocol_version_gets_a_typed_mismatch() {
    let response = BackendSupport::protocol_mismatch_response(4);
    assert_eq!(response.protocol(), DEVENV_BACKEND_PROTOCOL_VERSION);
    assert!(matches!(
        response.response(),
        BackendResponse::Error(error)
            if error.class() == "BACKEND-PROTOCOL-MISMATCH"
                && error.message().contains("protocol v4")
    ));
}

#[cfg(test)]
#[test]
fn exit_status_codes_are_preserved() {
    use std::os::unix::process::ExitStatusExt;

    let status = ExitStatus::from_raw(7 << 8);
    assert_eq!(BackendSupport::exit_status_code(status), 7);
}
