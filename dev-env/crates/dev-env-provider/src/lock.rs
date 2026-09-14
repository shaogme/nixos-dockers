use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum LockError {
    CreateDirectory { path: PathBuf, source: io::Error },
    Open { path: PathBuf, source: io::Error },
    TryLock { path: PathBuf, source: io::Error },
    Timeout { path: PathBuf, timeout: Duration },
}

impl std::fmt::Display for LockError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateDirectory { path, source } => {
                write!(
                    formatter,
                    "could not create lock directory {}: {source}",
                    path.display()
                )
            }
            Self::Open { path, source } => {
                write!(
                    formatter,
                    "could not open lock {}: {source}",
                    path.display()
                )
            }
            Self::TryLock { path, source } => {
                write!(formatter, "could not lock {}: {source}", path.display())
            }
            Self::Timeout { path, timeout } => write!(
                formatter,
                "timed out after {}ms waiting for {}",
                timeout.as_millis(),
                path.display()
            ),
        }
    }
}

impl std::error::Error for LockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDirectory { source, .. }
            | Self::Open { source, .. }
            | Self::TryLock { source, .. } => Some(source),
            Self::Timeout { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockManager {
    directory: PathBuf,
    poll_interval: Duration,
}

impl LockManager {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            poll_interval: Duration::from_millis(10),
        }
    }

    pub fn from_environment() -> Self {
        let directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .map(|path| path.join("dev-env/locks"))
            .unwrap_or_else(|| PathBuf::from("/run/dev-env/locks"));
        Self::new(directory)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval.max(Duration::from_millis(1));
        self
    }

    pub fn acquire(
        &self,
        key: [u8; 32],
        timeout: Duration,
    ) -> Result<ProviderLockGuard, LockError> {
        fs::create_dir_all(&self.directory).map_err(|source| LockError::CreateDirectory {
            path: self.directory.clone(),
            source,
        })?;
        let path = self.directory.join(format_hex(&key)).with_extension("lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| LockError::Open {
                path: path.clone(),
                source,
            })?;
        let started = Instant::now();
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => {
                    return Ok(ProviderLockGuard { _file: file, path });
                }
                Err(source) if source.kind() == io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= timeout {
                        return Err(LockError::Timeout { path, timeout });
                    }
                    thread::sleep(self.poll_interval);
                }
                Err(source) => return Err(LockError::TryLock { path, source }),
            }
        }
    }
}

#[derive(Debug)]
pub struct ProviderLockGuard {
    _file: File,
    path: PathBuf,
}

impl ProviderLockGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn lock_key(workspace: &Path, user_id: u32, provider_id: &str) -> [u8; 32] {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(workspace.display().to_string().as_bytes());
    hasher.update([0]);
    hasher.update(user_id.to_le_bytes());
    hasher.update([0]);
    hasher.update(provider_id.as_bytes());
    hasher.finalize().into()
}

fn format_hex(bytes: &[u8; 32]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from_digit((byte >> 4) as u32, 16).unwrap());
        output.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap());
    }
    output
}
