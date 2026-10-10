use libc::{
    fchmod, fchown, flock, fstatat, geteuid, open, openat, stat, AT_SYMLINK_NOFOLLOW, EAGAIN,
    EWOULDBLOCK, LOCK_EX, LOCK_NB, O_CLOEXEC, O_CREAT, O_DIRECTORY, O_NOFOLLOW, O_RDONLY, O_RDWR,
    S_IFMT, S_IFREG,
};
use std::{
    ffi::CString,
    fs::{self, File},
    io::{self, Seek, SeekFrom, Write},
    mem,
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path, PathBuf},
    process,
};

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
        validate_lock_directory(&directory, unsafe { geteuid() })?;
        validate_directory_path(parent, &directory)?;
        let fd = unsafe {
            openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                O_RDWR | O_CREAT | O_CLOEXEC | O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
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
        if current.st_mode & S_IFMT != S_IFREG
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
        writeln!(file, "pid={}", process::id())?;
        file.sync_all()?;
        Ok(InstanceClaim::Acquired(Self { path, file }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn allow_peer_group(&mut self, gid: u32) -> io::Result<()> {
        if unsafe { geteuid() } != 0 {
            return Ok(());
        }
        let fd = self.file.as_raw_fd();
        if unsafe { fchown(fd, u32::MAX, gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { fchmod(fd, 0o660) } != 0 {
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
        open(
            root.as_ptr(),
            O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW,
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
            openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW,
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
    let path_metadata = fs::symlink_metadata(path)?;
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
    if metadata.uid() != unsafe { geteuid() } {
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

fn stat_at(directory_fd: RawFd, name: &CString) -> io::Result<stat> {
    let mut stat: stat = unsafe { mem::zeroed() };
    if unsafe { fstatat(directory_fd, name.as_ptr(), &mut stat, AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat)
}

fn is_lock_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
        || error.raw_os_error() == Some(EWOULDBLOCK)
        || error.raw_os_error() == Some(EAGAIN)
}

#[cfg(test)]
mod tests {
    use super::{InstanceClaim, InstanceLock};
    use std::{fs, os::unix::fs::symlink};
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
        symlink(&target, &link).unwrap();

        assert!(InstanceLock::try_acquire(&link).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "preserve");
    }
}
