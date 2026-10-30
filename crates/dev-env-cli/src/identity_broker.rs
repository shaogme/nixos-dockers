//! Adapter from the shared container-init identity protocol to dev-env's
//! effective identity model.

use {
    container_init_protocol::{
        AbstractEndpoint, IdentityBrokerClient, IdentityRequest as BrokerIdentityRequest,
        DEFAULT_CONTAINER_INIT_BACKEND_ENDPOINT,
    },
    dev_env_model::{
        EffectiveIdentity, IdentityPeer, IdentityRequest, IdentitySource, WorkspaceStatus,
    },
    libc::geteuid,
    std::{
        collections::BTreeMap,
        env,
        error::Error,
        fmt::{Display, Formatter, Result as FmtResult},
        path::Path,
        time::Duration,
    },
};

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
    pub fn check() -> Result<(), BrokerError> {
        configured_client()?
            .hello()
            .map(|_| ())
            .map_err(|error| BrokerError::new(error.to_string()))
    }

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
        let identity = configured_client()?
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

fn configured_client() -> Result<IdentityBrokerClient, BrokerError> {
    let endpoint_name = match env::var("CONTAINER_INIT_BACKEND_ENDPOINT") {
        Ok(name) => name,
        Err(env::VarError::NotPresent) => DEFAULT_CONTAINER_INIT_BACKEND_ENDPOINT.to_owned(),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(BrokerError::new(
                "CONTAINER_INIT_BACKEND_ENDPOINT must be valid UTF-8",
            ));
        }
    };
    let endpoint = AbstractEndpoint::new(endpoint_name)
        .map_err(|error| BrokerError::new(error.to_string()))?;
    let peer_uid = match env::var("CONTAINER_INIT_BACKEND_PEER_UID") {
        Ok(value) => value.parse::<u32>().map_err(|_| {
            BrokerError::new("CONTAINER_INIT_BACKEND_PEER_UID must be an unsigned integer")
        })?,
        Err(env::VarError::NotPresent) => unsafe { geteuid() },
        Err(env::VarError::NotUnicode(_)) => {
            return Err(BrokerError::new(
                "CONTAINER_INIT_BACKEND_PEER_UID must be valid UTF-8",
            ));
        }
    };
    Ok(IdentityBrokerClient::new(
        endpoint,
        peer_uid,
        DEFAULT_TIMEOUT,
    ))
}
