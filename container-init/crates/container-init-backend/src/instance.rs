use container_init_posix::PosixLock;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum InstanceClaim {
    Acquired(InstanceLock),
    Occupied,
}

#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
    _lock: PosixLock,
}

impl InstanceLock {
    pub fn try_acquire(path: impl Into<PathBuf>) -> io::Result<InstanceClaim> {
        let path = path.into();
        validate_lock_path(&path)?;
        let mut lock = match PosixLock::try_acquire(&path) {
            Ok(lock) => lock,
            Err(error) if is_lock_contended(&error) => return Ok(InstanceClaim::Occupied),
            Err(error) => return Err(error),
        };
        set_cloexec(lock.file_mut().as_raw_fd())?;
        let file = lock.file_mut();
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "pid={}", std::process::id())?;
        file.sync_all()?;
        Ok(InstanceClaim::Acquired(Self { path, _lock: lock }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn allow_peer_group(&mut self, gid: u32) -> io::Result<()> {
        if unsafe { libc::geteuid() } != 0 {
            return Ok(());
        }
        let fd = self._lock.file_mut().as_raw_fd();
        if unsafe { libc::fchown(fd, u32::MAX, gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fchmod(fd, 0o660) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn validate_lock_path(path: &Path) -> io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend instance lock must be a regular file, not a symlink",
        ));
    }
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "backend instance lock is not owned by this user",
        ));
    }
    if metadata.mode() & 0o002 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "backend instance lock is world-writable",
        ));
    }
    Ok(())
}

fn set_cloexec(fd: std::os::fd::RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn is_lock_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
        || error.raw_os_error() == Some(libc::EWOULDBLOCK)
        || error.raw_os_error() == Some(libc::EAGAIN)
}

#[cfg(test)]
mod tests {
    use super::{InstanceClaim, InstanceLock};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn flock_selects_one_instance_and_releases_on_drop() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("backend.lock");
        let InstanceClaim::Acquired(lock) = InstanceLock::try_acquire(&path).unwrap() else {
            panic!("first instance did not acquire lock");
        };
        assert!(matches!(
            InstanceLock::try_acquire(&path).unwrap(),
            InstanceClaim::Occupied
        ));
        assert!(fs::read_to_string(&path).unwrap().contains("pid="));
        drop(lock);
        assert!(matches!(
            InstanceLock::try_acquire(&path).unwrap(),
            InstanceClaim::Acquired(_)
        ));
    }

    #[test]
    fn symlink_instance_lock_is_rejected_without_touching_target() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("target");
        fs::write(&target, "preserve").unwrap();
        let link = temp.path().join("backend.lock");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(InstanceLock::try_acquire(&link).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "preserve");
    }
}
