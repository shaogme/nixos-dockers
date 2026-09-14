use crate::error::TrustError;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Add a configuration file hash to the per-user trust store, or register an
/// already computed SHA-256 hash.  The trust command intentionally stores
/// only hashes, never the file contents or a rendered configuration error.
pub fn trust(target: &Path) -> Result<String, TrustError> {
    let digest = if target.is_file() {
        let contents = fs::read(target).map_err(|source| TrustError::HashFile {
            path: target.to_path_buf(),
            source,
        })?;
        dev_env_provider::fingerprint_bytes(&contents)
    } else {
        let value = target.to_string_lossy();
        parse_hash(&value).ok_or_else(|| TrustError::InvalidHash {
            value: target.to_path_buf(),
        })?
    };
    let hash = hex_digest(digest);
    let store = trust_store_path();
    let existing = match fs::read_to_string(&store) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(TrustError::ReadStore {
                path: store,
                source,
            })
        }
    };
    if !existing.lines().any(|line| line.trim() == hash) {
        if let Some(parent) = store.parent() {
            fs::create_dir_all(parent).map_err(|source| TrustError::WriteStore {
                path: store.clone(),
                source,
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&store)
            .map_err(|source| TrustError::WriteStore {
                path: store.clone(),
                source,
            })?;
        writeln!(file, "{hash}").map_err(|source| TrustError::WriteStore {
            path: store.clone(),
            source,
        })?;
    }
    Ok(hash)
}

fn trust_store_path() -> PathBuf {
    if let Some(path) = std::env::var_os("DEVENV_TRUST_FILE") {
        return PathBuf::from(path);
    }
    if let Some(root) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(root).join("dev-env").join("trusted-hashes");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("dev-env")
            .join("trusted-hashes");
    }
    PathBuf::from(".dev-env-trusted-hashes")
}

fn parse_hash(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut result = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        result[index] = (hex_value(chunk[0])? << 4) | hex_value(chunk[1])?;
    }
    Some(result)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
