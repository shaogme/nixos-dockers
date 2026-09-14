use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// A non-blocking advisory lock backed by a POSIX file descriptor.
#[derive(Debug)]
pub struct PosixLock {
    file: File,
}

impl PosixLock {
    pub fn acquire(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { file })
    }

    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }
}

impl Drop for PosixLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Check write access using the effective credentials of the current process.
/// For a path that does not exist, the nearest existing parent is checked.
pub fn is_writable(path: &Path) -> bool {
    let bytes = path.as_os_str().as_bytes();
    let Ok(c_path) = std::ffi::CString::new(bytes) else {
        return false;
    };
    (unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 })
        || path
            .parent()
            .is_some_and(|parent| std::fs::metadata(path).is_err() && is_writable(parent))
}
