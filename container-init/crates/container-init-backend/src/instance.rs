use std::ffi::CString;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub enum InstanceClaim {
    Acquired(InstanceLock),
    Occupied,
}

#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
    file: File,
}

impl InstanceLock {
    pub fn try_acquire(path: impl Into<PathBuf>) -> io::Result<InstanceClaim> {
        let path = path.into();
        validate_normalized_path(&path)?;
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "lock path has no parent")
        })?;
        let name = path.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "lock path has no filename")
        })?;
        let name = CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "lock filename contains NUL")
        })?;
        let directory = open_directory_chain(parent)?;
        validate_lock_directory(&directory, unsafe { libc::geteuid() })?;
        validate_directory_path(parent, &directory)?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if is_lock_contended(&error) {
                return Ok(InstanceClaim::Occupied);
            }
            return Err(error);
        }
        validate_lock_file(&file)?;
        let opened = file.metadata()?;
        let current = stat_at(directory.as_raw_fd(), &name)?;
        if current.st_mode & libc::S_IFMT != libc::S_IFREG
            || current.st_dev != opened.dev()
            || current.st_ino != opened.ino()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend lock path changed while acquiring the lock",
            ));
        }
        validate_directory_path(parent, &directory)?;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "pid={}", std::process::id())?;
        file.sync_all()?;
        Ok(InstanceClaim::Acquired(Self { path, file }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn allow_peer_group(&mut self, gid: u32) -> io::Result<()> {
        if unsafe { libc::geteuid() } != 0 {
            return Ok(());
        }
        let fd = self.file.as_raw_fd();
        if unsafe { libc::fchown(fd, u32::MAX, gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fchmod(fd, 0o660) } != 0 {
            return Err(io::Error::last_os_error());
        }
        validate_lock_file(&self.file)
    }
}

fn validate_normalized_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend lock path must be absolute and normalized",
        ));
    }
    Ok(())
}

fn open_directory_chain(path: &Path) -> io::Result<File> {
    validate_normalized_path(path)?;
    let root = CString::new("/").expect("literal has no NUL");
    let fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut directory = unsafe { File::from_raw_fd(fd) };
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let name = CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "directory name contains NUL")
        })?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

fn validate_lock_directory(directory: &File, owner_uid: u32) -> io::Result<()> {
    let metadata = directory.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend lock parent is not a real directory",
        ));
    }
    if metadata.uid() != owner_uid || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "backend lock parent has an unsafe owner or mode",
        ));
    }
    Ok(())
}

fn validate_directory_path(path: &Path, directory: &File) -> io::Result<()> {
    let path_metadata = std::fs::symlink_metadata(path)?;
    let fd_metadata = directory.metadata()?;
    if !path_metadata.file_type().is_dir()
        || path_metadata.dev() != fd_metadata.dev()
        || path_metadata.ino() != fd_metadata.ino()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend lock directory changed during the operation",
        ));
    }
    Ok(())
}

fn validate_lock_file(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend instance lock must be a regular file",
        ));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
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

fn stat_at(directory_fd: std::os::fd::RawFd, name: &CString) -> io::Result<libc::stat> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
            directory_fd,
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(stat)
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
