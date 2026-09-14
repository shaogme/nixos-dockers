use dev_env_model::ProviderConfig;
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum FingerprintError {
    Serialize { source: serde_json::Error },
    Canonicalize { path: PathBuf, source: io::Error },
    Read { path: PathBuf, source: io::Error },
}

impl std::fmt::Display for FingerprintError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

impl std::error::Error for FingerprintError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serialize { source } => Some(source),
            Self::Canonicalize { source, .. } | Self::Read { source, .. } => Some(source),
        }
    }
}

pub fn fingerprint_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub fn provider_config_fingerprint(config: &ProviderConfig) -> Result<[u8; 32], FingerprintError> {
    let encoded =
        serde_json::to_vec(config).map_err(|source| FingerprintError::Serialize { source })?;
    Ok(fingerprint_bytes(&encoded))
}

/// Hash the canonical workspace path and the contents of files used for
/// detection.  The caller supplies the matched paths so unrelated workspace
/// files and secrets are not copied into a receipt.
pub fn workspace_fingerprint(
    workspace: &Path,
    matched_files: &[PathBuf],
) -> Result<[u8; 32], FingerprintError> {
    let canonical =
        std::fs::canonicalize(workspace).map_err(|source| FingerprintError::Canonicalize {
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
        let contents = std::fs::read(&path).map_err(|source| FingerprintError::Read {
            path: path.clone(),
            source,
        })?;
        hasher.update((contents.len() as u64).to_le_bytes());
        hasher.update(contents);
    }
    Ok(hasher.finalize().into())
}
