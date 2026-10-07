//! Adapter from the shared container-init identity protocol to dev-env's
//! effective identity model.

use dev_env_model::{
    EffectiveIdentity, IdentityPeer, IdentityRequest, IdentitySource, WorkspaceStatus,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_SOCKET: &str = "/run/container-init/backend.sock";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct BrokerError(pub String);

impl std::fmt::Display for BrokerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BrokerError {}

pub fn resolve(
    request: &IdentityRequest,
    peer: IdentityPeer,
    cwd: &Path,
    inputs: BTreeMap<String, String>,
    environment: BTreeMap<String, String>,
) -> Result<EffectiveIdentity, BrokerError> {
    let requested = match request {
        IdentityRequest::Peer => container_init_protocol::IdentityRequest::Peer {
            uid: peer.uid,
            gid: peer.gid,
        },
        IdentityRequest::Root => container_init_protocol::IdentityRequest::Root,
        IdentityRequest::User { name } => {
            container_init_protocol::IdentityRequest::User { name: name.clone() }
        }
    };
    let socket = std::env::var_os("DEVENV_IDENTITY_BROKER_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET));
    let identity = container_init_protocol::IdentityBrokerClient::new(socket, DEFAULT_TIMEOUT)
        .resolve(requested, cwd, inputs, environment)
        .map_err(|error| BrokerError(error.to_string()))?;
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
