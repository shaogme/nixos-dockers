use crate::{
    read_message, write_message, ClientMessage, ClientRequest, IdentityRequest, PreparedIdentity,
    ServerMessage, ServerResponse, PROTOCOL_VERSION,
};
use std::collections::{BTreeMap, BTreeSet};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug)]
pub struct IdentityBrokerError(pub String);

impl std::fmt::Display for IdentityBrokerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for IdentityBrokerError {}

#[derive(Clone, Debug)]
pub struct IdentityBrokerClient {
    socket: PathBuf,
    timeout: Duration,
}

impl IdentityBrokerClient {
    pub fn new(socket: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            socket: socket.into(),
            timeout,
        }
    }

    pub fn resolve(
        &self,
        requested: IdentityRequest,
        cwd: &Path,
        inputs: BTreeMap<String, String>,
        environment: BTreeMap<String, String>,
    ) -> Result<PreparedIdentity, IdentityBrokerError> {
        if !self.socket.is_absolute() || self.socket.to_string_lossy().contains('\0') {
            return Err(IdentityBrokerError(
                "identity broker socket must be absolute".to_owned(),
            ));
        }
        let mut stream = UnixStream::connect(&self.socket).map_err(|error| {
            IdentityBrokerError(format!("identity broker unavailable: {error}"))
        })?;
        stream
            .set_read_timeout(Some(self.timeout))
            .and_then(|_| stream.set_write_timeout(Some(self.timeout)))
            .map_err(|error| {
                IdentityBrokerError(format!("identity broker socket setup failed: {error}"))
            })?;

        write_message(
            &mut stream,
            &ClientMessage {
                version: PROTOCOL_VERSION,
                request: ClientRequest::Hello,
            },
        )
        .map_err(|error| IdentityBrokerError(format!("identity broker write failed: {error}")))?;
        let hello = expect_hello(&mut stream)?;

        let allowed_inputs = hello.runtime_inputs.into_iter().collect::<BTreeSet<_>>();
        let allowed_environment = hello.environment_names.into_iter().collect::<BTreeSet<_>>();
        let inputs = restrict_to_allowed(inputs, &allowed_inputs);
        let environment = restrict_to_allowed(environment, &allowed_environment);
        write_message(
            &mut stream,
            &ClientMessage {
                version: PROTOCOL_VERSION,
                request: ClientRequest::PrepareIdentity {
                    cwd: cwd.to_path_buf(),
                    inputs,
                    environment,
                    requested,
                },
            },
        )
        .map_err(|error| IdentityBrokerError(format!("identity broker write failed: {error}")))?;
        let response = read_message::<ServerMessage>(&mut stream).map_err(|error| {
            IdentityBrokerError(format!("identity broker response is invalid: {error}"))
        })?;
        let _ = stream.shutdown(Shutdown::Both);
        if response.version != PROTOCOL_VERSION {
            return Err(IdentityBrokerError(
                "identity broker returned an unsupported protocol version".to_owned(),
            ));
        }
        let identity = match response.response {
            ServerResponse::Identity(identity) => identity,
            ServerResponse::Error(error) => {
                return Err(IdentityBrokerError(format!(
                    "{}: {}",
                    error.class, error.message
                )))
            }
            ServerResponse::Hello(_)
            | ServerResponse::PreparedExec(_)
            | ServerResponse::CommitResult(_)
            | ServerResponse::Status(_)
            | ServerResponse::PlanPage(_)
            | ServerResponse::Doctor(_)
            | ServerResponse::Other => {
                return Err(IdentityBrokerError(
                    "identity broker returned an unexpected response".to_owned(),
                ))
            }
        };
        validate_identity(&identity)?;
        Ok(identity)
    }
}

fn expect_hello(stream: &mut UnixStream) -> Result<crate::HelloInfo, IdentityBrokerError> {
    let response = read_message::<ServerMessage>(stream).map_err(|error| {
        IdentityBrokerError(format!("identity broker response is invalid: {error}"))
    })?;
    if response.version != PROTOCOL_VERSION {
        return Err(IdentityBrokerError(
            "identity broker returned an unsupported protocol version".to_owned(),
        ));
    }
    match response.response {
        ServerResponse::Hello(hello) => Ok(hello),
        ServerResponse::Error(error) => Err(IdentityBrokerError(format!(
            "{}: {}",
            error.class, error.message
        ))),
        ServerResponse::Identity(_)
        | ServerResponse::PreparedExec(_)
        | ServerResponse::CommitResult(_)
        | ServerResponse::Status(_)
        | ServerResponse::PlanPage(_)
        | ServerResponse::Doctor(_)
        | ServerResponse::Other => Err(IdentityBrokerError(
            "identity broker hello failed".to_owned(),
        )),
    }
}

fn restrict_to_allowed(
    values: BTreeMap<String, String>,
    allowed: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    values
        .into_iter()
        .filter(|(name, _)| allowed.contains(name))
        .collect()
}

fn validate_identity(identity: &PreparedIdentity) -> Result<(), IdentityBrokerError> {
    if identity.user.is_empty()
        || identity.user.contains('\0')
        || !identity.home.is_absolute()
        || identity
            .supplemental_groups
            .windows(2)
            .any(|groups| groups[0] >= groups[1])
        || (identity.uid == 0 && identity.gid != 0)
        || (identity.run_as_root && identity.uid != 0)
    {
        return Err(IdentityBrokerError(
            "identity broker returned an invalid identity".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::IdentityBrokerClient;
    use crate::{
        read_message, write_message, BackendState, ClientMessage, ClientRequest, IdentityRequest,
        PreparedIdentity, ServerMessage, ServerResponse, PROTOCOL_VERSION,
    };
    use std::collections::BTreeMap;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn identity_client_filters_runtime_values_and_decodes_the_shared_response() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket = PathBuf::from(format!(
            "/tmp/container-init-protocol-{}-{nonce}.sock",
            std::process::id()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let hello: ClientMessage = read_message(&mut stream).unwrap();
            assert!(matches!(hello.request, ClientRequest::Hello));
            write_message(
                &mut stream,
                &ServerMessage {
                    version: PROTOCOL_VERSION,
                    response: ServerResponse::Hello(crate::HelloInfo {
                        state: BackendState::Ready,
                        profile: "profile".to_owned(),
                        snapshot_id: "snapshot".to_owned(),
                        runtime_inputs: vec!["HOST_UID".to_owned()],
                        environment_names: vec!["HOME".to_owned()],
                    }),
                },
            )
            .unwrap();
            let prepare: ClientMessage = read_message(&mut stream).unwrap();
            match prepare.request {
                ClientRequest::PrepareIdentity {
                    inputs,
                    environment,
                    ..
                } => {
                    assert_eq!(
                        inputs,
                        BTreeMap::from([("HOST_UID".to_owned(), "1000".to_owned())])
                    );
                    assert_eq!(
                        environment,
                        BTreeMap::from([("HOME".to_owned(), "/home/dev".to_owned())])
                    );
                }
                request => panic!("unexpected request: {request:?}"),
            }
            write_message(
                &mut stream,
                &ServerMessage {
                    version: PROTOCOL_VERSION,
                    response: ServerResponse::Identity(PreparedIdentity {
                        uid: 1000,
                        gid: 1000,
                        user: "dev".to_owned(),
                        home: PathBuf::from("/home/dev"),
                        supplemental_groups: vec![1000, 1001],
                        run_as_root: false,
                    }),
                },
            )
            .unwrap();
        });

        let client = IdentityBrokerClient::new(&socket, Duration::from_secs(2));
        let identity = client
            .resolve(
                IdentityRequest::Peer {
                    uid: 1000,
                    gid: 1000,
                },
                std::path::Path::new("/workspace"),
                BTreeMap::from([
                    ("HOST_UID".to_owned(), "1000".to_owned()),
                    ("DEVBOX_AUTO_INIT".to_owned(), "1".to_owned()),
                ]),
                BTreeMap::from([
                    ("HOME".to_owned(), "/home/dev".to_owned()),
                    ("PATH".to_owned(), "/bin".to_owned()),
                ]),
            )
            .unwrap();
        server.join().unwrap();
        let _ = std::fs::remove_file(socket);
        assert_eq!(identity.uid, 1000);
        assert_eq!(identity.user, "dev");
    }
}
