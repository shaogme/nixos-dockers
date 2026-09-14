use dev_env_model::ResolvedConfig;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum FingerprintError {
    Serialize { source: serde_json::Error },
}

impl fmt::Display for FingerprintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialize { source } => {
                write!(
                    formatter,
                    "could not serialize resolved configuration: {source}"
                )
            }
        }
    }
}

impl Error for FingerprintError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Serialize { source } => Some(source),
        }
    }
}

pub(crate) fn config_fingerprint(config: &ResolvedConfig) -> Result<[u8; 32], FingerprintError> {
    let encoded =
        serde_json::to_vec(config).map_err(|source| FingerprintError::Serialize { source })?;
    Ok(dev_env_provider::fingerprint_bytes(&encoded))
}
