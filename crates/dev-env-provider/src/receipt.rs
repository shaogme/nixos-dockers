use dev_env_model::ProviderConfig;
use serde_json::{to_vec, Error as SerdeError};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fmt::{Display, Formatter, Result as FmtResult},
    fs::{canonicalize, read},
    io,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub enum FingerprintError {
    Serialize { source: SerdeError },
    Canonicalize { path: PathBuf, source: io::Error },
    Read { path: PathBuf, source: io::Error },
}

impl Display for FingerprintError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Serialize { source } => {
                write!(formatter, "could not serialize fingerprint input: {source}")
            }
            Self::Canonicalize { path, source } => {
                write!(
                    formatter,
                    "could not canonicalize {}: {source}",
                    path.display()
                )
            }
            Self::Read { path, source } => {
                write!(formatter, "could not read {}: {source}", path.display())
            }
        }
    }
}

impl Error for FingerprintError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Serialize { source } => Some(source),
            Self::Canonicalize { source, .. } | Self::Read { source, .. } => Some(source),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProviderFingerprints;

impl ProviderFingerprints {
    pub fn fingerprint_bytes(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    pub fn provider_config(config: &ProviderConfig) -> Result<[u8; 32], FingerprintError> {
        let encoded = to_vec(config).map_err(|source| FingerprintError::Serialize { source })?;
        Ok(Self::fingerprint_bytes(&encoded))
    }

    /// Hash the canonical workspace path and the contents of files used for
    /// detection. The caller supplies the matched paths so unrelated workspace
    /// files and secrets are not copied into a receipt.
    pub fn workspace(
        workspace: &Path,
        matched_files: &[PathBuf],
    ) -> Result<[u8; 32], FingerprintError> {
        let canonical =
            canonicalize(workspace).map_err(|source| FingerprintError::Canonicalize {
                path: workspace.to_path_buf(),
                source,
            })?;
        let mut hasher = Sha256::new();
        hasher.update(canonical.to_string_lossy().as_bytes());
        let mut files = matched_files.to_vec();
        files.sort();
        files.dedup();
        for path in files {
            let relative = path.strip_prefix(workspace).unwrap_or(&path);
            hasher.update([0]);
            hasher.update(relative.to_string_lossy().as_bytes());
            let contents = read(&path).map_err(|source| FingerprintError::Read {
                path: path.clone(),
                source,
            })?;
            hasher.update((contents.len() as u64).to_le_bytes());
            hasher.update(contents);
        }
        Ok(hasher.finalize().into())
    }
}
