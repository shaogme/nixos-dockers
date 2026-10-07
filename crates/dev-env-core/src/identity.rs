use dev_env_model::{EffectiveIdentity, IdentityPeer, IdentityRequest};

/// Boundary between dev-env and container-init identity resolution.
///
/// This trait intentionally exposes only a request and authenticated peer
/// credentials. UID/GID namespace mapping, supplementary group lookup, and
/// policy checks belong to the container-init implementation.
pub trait IdentityBroker: Send + Sync {
    fn resolve(
        &self,
        request: &IdentityRequest,
        peer: IdentityPeer,
    ) -> Result<EffectiveIdentity, IdentityBrokerError>;

    fn prepare_identity(
        &self,
        request: &IdentityRequest,
        peer: IdentityPeer,
    ) -> Result<EffectiveIdentity, IdentityBrokerError> {
        self.resolve(request, peer)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentityBrokerError {
    Unauthorized,
    Unavailable,
    InvalidRequest(String),
    Resolution(String),
}

impl std::fmt::Display for IdentityBrokerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => formatter.write_str("identity request is not authorized"),
            Self::Unavailable => formatter.write_str("identity broker is unavailable"),
            Self::InvalidRequest(message) => {
                write!(formatter, "invalid identity request: {message}")
            }
            Self::Resolution(message) => write!(formatter, "identity resolution failed: {message}"),
        }
    }
}

impl std::error::Error for IdentityBrokerError {}
