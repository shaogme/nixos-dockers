use crate::error::CoreError;
use container_init_posix::PosixLock;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum LockOperation {
    Acquire,
    Truncate,
    Seek,
    Write,
    Sync,
}

impl std::fmt::Display for LockOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Acquire => "acquire",
            Self::Truncate => "truncate",
            Self::Seek => "seek",
            Self::Write => "write",
            Self::Sync => "sync",
        };
        formatter.write_str(name)
    }
}

/// A non-blocking, process-scoped bootstrap lock.
///
/// The caller chooses the lock location so the CLI can use a runtime directory
/// or a persistent workspace-specific location according to its policy.
#[derive(Debug)]
pub struct BootstrapLock {
    path: PathBuf,
    _lock: PosixLock,
}

impl BootstrapLock {
    pub fn acquire(path: impl Into<PathBuf>) -> Result<Self, CoreError> {
        let path = path.into();
        let mut lock = PosixLock::acquire(&path).map_err(|source| CoreError::Lock {
            path: path.clone(),
            operation: LockOperation::Acquire,
            source,
        })?;
        let file = lock.file_mut();
        file.set_len(0).map_err(|source| CoreError::Lock {
            path: path.clone(),
            operation: LockOperation::Truncate,
            source,
        })?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| CoreError::Lock {
                path: path.clone(),
                operation: LockOperation::Seek,
                source,
            })?;
        writeln!(file, "pid={}", std::process::id()).map_err(|source| CoreError::Lock {
            path: path.clone(),
            operation: LockOperation::Write,
            source,
        })?;
        file.sync_all().map_err(|source| CoreError::Lock {
            path: path.clone(),
            operation: LockOperation::Sync,
            source,
        })?;
        Ok(Self { path, _lock: lock })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
