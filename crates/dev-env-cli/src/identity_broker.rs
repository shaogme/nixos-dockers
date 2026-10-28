//! Adapter from the shared container-init identity protocol to dev-env's
//! effective identity model.

use {
    container_init_protocol::{IdentityBrokerClient, IdentityRequest as BrokerIdentityRequest},
    dev_env_model::{
        EffectiveIdentity, IdentityPeer, IdentityRequest, IdentitySource, WorkspaceStatus,
    },
    std::{
        collections::BTreeMap,
        env::var_os,
        error::Error,
        fmt::{Display, Formatter, Result as FmtResult},
        path::{Path, PathBuf},
        time::Duration,
    },
};

const DEFAULT_SOCKET: &str = "/run/container-init/backend.sock";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct BrokerError(String);

pub struct IdentityBroker;

impl Display for BrokerError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.0)
    }
}

impl Error for BrokerError {}

impl BrokerError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl IdentityBroker {
    pub fn resolve(
        request: &IdentityRequest,
        peer: IdentityPeer,
        cwd: &Path,
        inputs: BTreeMap<String, String>,
        environment: BTreeMap<String, String>,
    ) -> Result<EffectiveIdentity, BrokerError> {
        let requested = match request {
            IdentityRequest::Peer => BrokerIdentityRequest::Peer {
                uid: peer.uid,
                gid: peer.gid,
            },
            IdentityRequest::Root => BrokerIdentityRequest::Root,
            IdentityRequest::User { name } => BrokerIdentityRequest::User { name: name.clone() },
        };
        let socket = var_os("DEVENV_IDENTITY_BROKER_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET));
        let identity = IdentityBrokerClient::new(socket, DEFAULT_TIMEOUT)
            .resolve(requested, cwd, inputs, environment)
            .map_err(|error| BrokerError::new(error.to_string()))?;
        let source = if identity.uid == 0 {
            IdentitySource::Root
        } else {
            IdentitySource::Broker
        };
        Ok(EffectiveIdentity::new(
            identity.uid,
            identity.gid,
            identity.user,
            identity.home,
            source,
        )
        .with_supplementary_groups(identity.supplemental_groups)
        .with_run_as_root(identity.run_as_root)
        .with_workspace(WorkspaceStatus::Unavailable))
    }
}
