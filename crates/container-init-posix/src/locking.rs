use libc::{
    access, flock, EAGAIN, EINTR, EWOULDBLOCK, LOCK_EX, LOCK_NB, LOCK_UN, O_CLOEXEC, O_NOFOLLOW,
    W_OK,
};
use std::{
    ffi::CString,
    fs::{metadata, File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::Path,
    thread,
    time::{Duration, Instant},
};

pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// An advisory lock backed by a POSIX file descriptor with timeout and retry support.
#[derive(Debug)]
pub struct PosixLock {
    file: File,
}

impl PosixLock {
    pub(crate) fn default_lock_timeout() -> Duration {
        DEFAULT_LOCK_TIMEOUT
    }

    pub(crate) fn default_poll_interval() -> Duration {
        DEFAULT_POLL_INTERVAL
    }

    /// Acquire an exclusive lock with the default timeout and polling interval.
    pub fn acquire(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::acquire_with_timeout(path, Self::default_lock_timeout())
    }

    /// Try to acquire an exclusive lock immediately without waiting.
    pub fn try_acquire(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::acquire_with_retry(path, Duration::ZERO, Duration::ZERO)
    }

    /// Acquire an exclusive lock with a specific timeout, using the default polling interval.
    pub fn acquire_with_timeout(path: impl AsRef<Path>, timeout: Duration) -> io::Result<Self> {
        Self::acquire_with_retry(path, timeout, Self::default_poll_interval())
    }

    /// Acquire an exclusive lock with a specific timeout and polling interval.
    pub fn acquire_with_retry(
        path: impl AsRef<Path>,
        timeout: Duration,
        poll_interval: Duration,
    ) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(O_CLOEXEC | O_NOFOLLOW)
            .open(path)?;

        let started = Instant::now();
        loop {
            let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
            if result == 0 {
                return Ok(Self { file });
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(EINTR) {
                if started.elapsed() >= timeout {
                    return Err(err);
                }
                continue;
            }
            if err.raw_os_error() != Some(EWOULDBLOCK) && err.raw_os_error() != Some(EAGAIN) {
                return Err(err);
            }
            if started.elapsed() >= timeout {
                return Err(err);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            let sleep_duration = if poll_interval.is_zero() {
                remaining
            } else {
                poll_interval.min(remaining)
            };
            if sleep_duration.is_zero() {
                return Err(err);
            }
            thread::sleep(sleep_duration);
        }
    }

    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// Check write access using the effective credentials of the current process.
    /// For a path that does not exist, the nearest existing parent is checked.
    pub(crate) fn is_writable(path: &Path) -> bool {
        let bytes = path.as_os_str().as_bytes();
        let Ok(c_path) = CString::new(bytes) else {
            return false;
        };
        (unsafe { access(c_path.as_ptr(), W_OK) == 0 })
            || path
                .parent()
                .is_some_and(|parent| metadata(path).is_err() && Self::is_writable(parent))
    }
}

impl Drop for PosixLock {
    fn drop(&mut self) {
        let _ = unsafe { flock(self.file.as_raw_fd(), LOCK_UN) };
    }
}
