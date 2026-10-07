//! Minimal client for the read-only container-init identity handshake.
//!
//! The dev-env backend is the long-lived root process, but it does not own
//! UID/GID namespace policy.  It asks container-init to resolve each request
//! and only uses the returned identity snapshot.

use dev_env_model::{
    EffectiveIdentity, IdentityPeer, IdentityRequest, IdentitySource, WorkspaceStatus,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PROTOCOL_VERSION: u16 = 2;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const DEFAULT_SOCKET: &str = "/run/container-init/backend.sock";

#[derive(Debug)]
pub struct BrokerError(pub String);

impl std::fmt::Display for BrokerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BrokerError {}

#[derive(Clone, Debug, Serialize)]
struct ClientMessage {
    version: u16,
    request: ClientRequest,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientRequest {
    Hello,
    PrepareIdentity {
        cwd: PathBuf,
        inputs: BTreeMap<String, String>,
        environment: BTreeMap<String, String>,
        requested: WireIdentityRequest,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum WireIdentityRequest {
    Peer { uid: u32, gid: u32 },
    Root,
    User { name: String },
}

#[derive(Debug, Deserialize)]
struct ServerMessage {
    version: u16,
    response: ServerResponse,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerResponse {
    Hello(BrokerHello),
    Identity(PreparedIdentity),
    Error(BrokerErrorPayload),
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct BrokerHello {
    runtime_inputs: Vec<String>,
    environment_names: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BrokerErrorPayload {
    class: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct PreparedIdentity {
    uid: u32,
    gid: u32,
    user: String,
    home: PathBuf,
    #[serde(default)]
    supplemental_groups: Vec<u32>,
    run_as_root: bool,
}

pub fn resolve(
    request: &IdentityRequest,
    peer: IdentityPeer,
    cwd: &Path,
    inputs: BTreeMap<String, String>,
    environment: BTreeMap<String, String>,
) -> Result<EffectiveIdentity, BrokerError> {
    let requested = match request {
        IdentityRequest::Peer => WireIdentityRequest::Peer {
            uid: peer.uid,
            gid: peer.gid,
        },
        IdentityRequest::Root => WireIdentityRequest::Root,
        IdentityRequest::User { name } => WireIdentityRequest::User { name: name.clone() },
    };
    let socket = std::env::var_os("DEVENV_IDENTITY_BROKER_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET));
    if !socket.is_absolute() || socket.to_string_lossy().contains('\0') {
        return Err(BrokerError(
            "identity broker socket must be absolute".to_owned(),
        ));
    }
    let mut stream = UnixStream::connect(&socket)
        .map_err(|error| BrokerError(format!("identity broker unavailable: {error}")))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(5))))
        .map_err(|error| BrokerError(format!("identity broker socket setup failed: {error}")))?;
    write_message(
        &mut stream,
        &ClientMessage {
            version: PROTOCOL_VERSION,
            request: ClientRequest::Hello,
        },
    )?;
    let hello = expect_hello(&mut stream)?;
    // dev-env and container-init have separate runtime input schemas. Only
    // values advertised by the bootstrap broker may cross this boundary;
    // provider inputs such as DEVBOX_AUTO_INIT remain local to dev-env.
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
    )?;
    let response: ServerMessage = read_message(&mut stream)?;
    let _ = stream.shutdown(Shutdown::Both);
    if response.version != PROTOCOL_VERSION {
        return Err(BrokerError(
            "identity broker returned an unsupported protocol version".to_owned(),
        ));
    }
    let identity = match response.response {
        ServerResponse::Identity(identity) => identity,
        ServerResponse::Error(error) => {
            return Err(BrokerError(format!("{}: {}", error.class, error.message)))
        }
        ServerResponse::Hello(_) | ServerResponse::Other => {
            return Err(BrokerError(
                "identity broker returned an unexpected response".to_owned(),
            ))
        }
    };
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
        return Err(BrokerError(
            "identity broker returned an invalid identity".to_owned(),
        ));
    }
    let source = if identity.uid == 0 {
        IdentitySource::Root
    } else {
        IdentitySource::Broker
    };
    Ok(EffectiveIdentity {
        uid: identity.uid,
        gid: identity.gid,
        user: identity.user,
        home: identity.home,
        supplementary_groups: identity.supplemental_groups,
        run_as_root: identity.run_as_root,
        source,
        workspace: WorkspaceStatus::Unavailable,
    })
}

fn expect_hello(stream: &mut UnixStream) -> Result<BrokerHello, BrokerError> {
    let response: ServerMessage = read_message(stream)?;
    if response.version != PROTOCOL_VERSION {
        return Err(BrokerError(
            "identity broker returned an unsupported protocol version".to_owned(),
        ));
    }
    match response.response {
        ServerResponse::Hello(hello) => Ok(hello),
        ServerResponse::Error(error) => {
            Err(BrokerError(format!("{}: {}", error.class, error.message)))
        }
        ServerResponse::Identity(_) | ServerResponse::Other => {
            Err(BrokerError("identity broker hello failed".to_owned()))
        }
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

fn write_message<T: Serialize>(stream: &mut UnixStream, message: &T) -> Result<(), BrokerError> {
    let payload = serde_json::to_vec(message).map_err(|error| BrokerError(error.to_string()))?;
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
        return Err(BrokerError("identity broker frame is oversized".to_owned()));
    }
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .and_then(|_| stream.write_all(&payload))
        .and_then(|_| stream.flush())
        .map_err(|error| BrokerError(format!("identity broker write failed: {error}")))
}

fn read_message<T: for<'de> Deserialize<'de>>(stream: &mut UnixStream) -> Result<T, BrokerError> {
    let mut header = [0_u8; 4];
    stream
        .read_exact(&mut header)
        .map_err(|error| BrokerError(format!("identity broker read failed: {error}")))?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(BrokerError("identity broker frame is invalid".to_owned()));
    }
    let mut payload = vec![0_u8; length];
    stream
        .read_exact(&mut payload)
        .map_err(|error| BrokerError(format!("identity broker read failed: {error}")))?;
    serde_json::from_slice(&payload)
        .map_err(|error| BrokerError(format!("identity broker response is invalid: {error}")))
}

impl From<io::Error> for BrokerError {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{ClientMessage, ClientRequest, ServerMessage, ServerResponse, PROTOCOL_VERSION};

    #[test]
    fn wire_messages_match_container_init_protocol_v2() {
        let hello = serde_json::to_value(ClientMessage {
            version: PROTOCOL_VERSION,
            request: ClientRequest::Hello,
        })
        .unwrap();
        assert_eq!(
            hello,
            serde_json::json!({
                "version": 2,
                "request": { "type": "hello" }
            })
        );

        let response: ServerMessage = serde_json::from_value(serde_json::json!({
            "version": 2,
            "response": {
                "type": "hello",
                "state": "ready",
                "profile": "nixos-docker",
                "snapshot_id": "snapshot",
                "runtime_inputs": ["HOST_UID"],
                "environment_names": ["HOST_UID"]
            }
        }))
        .unwrap();
        assert_eq!(response.version, 2);
        assert!(matches!(response.response, ServerResponse::Hello(_)));
    }
}
