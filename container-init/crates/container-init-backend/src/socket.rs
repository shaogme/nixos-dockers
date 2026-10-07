use crate::{PeerCredentials, ProtocolError};
use std::ffi::CString;
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};

pub fn ensure_socket_directory(
    path: &Path,
    owner_uid: u32,
    group_gid: u32,
    mode: u32,
) -> io::Result<()> {
    validate_normalized_path(path, "backend socket directory")?;
    let directory = open_directory_chain(path, true)?;
    let fd = directory.as_raw_fd();
    if unsafe { libc::fchown(fd, owner_uid, group_gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fchmod(fd, mode) } != 0 {
        return Err(io::Error::last_os_error());
    }
    validate_directory(&directory, owner_uid)?;
    validate_directory_path(path, &directory)
}

pub fn cleanup_stale_socket(path: &Path) -> io::Result<bool> {
    let (parent_path, name) = split_socket_path(path)?;
    let directory = open_directory_chain(parent_path, false)?;
    validate_directory(&directory, unsafe { libc::geteuid() })?;
    validate_directory_path(parent_path, &directory)?;
    let initial = match stat_at(directory.as_raw_fd(), &name) {
        Ok(stat) => stat,
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(false),
        Err(error) => return Err(error),
    };
    validate_socket_stat(&initial, unsafe { libc::geteuid() })?;
    unlink_socket_if_same(parent_path, &directory, &name, &initial, unsafe {
        libc::geteuid()
    })?;
    Ok(true)
}

pub fn bind_socket(
    path: &Path,
    owner_uid: u32,
    group_gid: u32,
    mode: u32,
) -> io::Result<UnixListener> {
    let (parent_path, name) = split_socket_path(path)?;
    let directory = open_directory_chain(parent_path, false)?;
    validate_directory(&directory, owner_uid)?;
    validate_directory_path(parent_path, &directory)?;
    match stat_at(directory.as_raw_fd(), &name) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "backend socket path already exists",
            ));
        }
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
        Err(error) => return Err(error),
    }

    // Linux has no bindat(2). Resolving this short procfs path anchors pathname
    // lookup at the directory descriptor checked above; we still verify the
    // requested directory and socket inode after bind and metadata changes.
    let anchored_path = anchored_path(directory.as_raw_fd(), name.as_bytes());
    let listener = UnixListener::bind(&anchored_path)?;
    let socket_stat = match stat_at(directory.as_raw_fd(), &name) {
        Ok(stat) => stat,
        Err(error) => {
            drop(listener);
            return Err(error);
        }
    };
    if let Err(error) = validate_socket_stat(&socket_stat, owner_uid)
        .and_then(|()| validate_directory_path(parent_path, &directory))
        .and_then(|()| {
            set_socket_metadata_at(directory.as_raw_fd(), &name, owner_uid, group_gid, mode)
        })
        .and_then(|()| {
            let current = stat_at(directory.as_raw_fd(), &name)?;
            if same_inode(&current, &socket_stat)
                && current.st_mode & libc::S_IFMT == libc::S_IFSOCK
                && current.st_uid == owner_uid
                && current.st_gid == group_gid
                && current.st_mode & 0o777 == mode & 0o777
            {
                validate_directory_path(parent_path, &directory)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "backend socket inode changed while setting permissions",
                ))
            }
        })
    {
        drop(listener);
        let _ = unlink_socket_if_same(parent_path, &directory, &name, &socket_stat, owner_uid);
        return Err(error);
    }
    set_cloexec(listener.as_raw_fd())?;
    Ok(listener)
}

pub fn validate_socket_path(path: &Path) -> io::Result<()> {
    let (parent_path, name) = split_socket_path(path)?;
    let directory = open_directory_chain(parent_path, false)?;
    let parent = directory.metadata()?;
    if parent.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "backend socket parent is group or world writable",
        ));
    }
    let stat = stat_at(directory.as_raw_fd(), &name)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend path is not a Unix socket",
        ));
    }
    if stat.st_uid != parent.uid() || stat.st_mode & 0o002 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "backend socket owner or mode is unsafe",
        ));
    }
    validate_directory_path(parent_path, &directory)
}

pub fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials, ProtocolError> {
    #[cfg(target_os = "linux")]
    {
        let fd = stream.as_raw_fd();
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        if result != 0 {
            return Err(ProtocolError::Io(io::Error::last_os_error()));
        }
        if length as usize != std::mem::size_of::<libc::ucred>() {
            return Err(ProtocolError::invalid_frame(
                "kernel returned malformed peer credentials",
            ));
        }
        Ok(PeerCredentials {
            pid: credentials.pid as u32,
            uid: credentials.uid,
            gid: credentials.gid,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        Err(ProtocolError::invalid_frame(
            "peer credential checks require Linux SO_PEERCRED",
        ))
    }
}

fn set_socket_metadata_at(
    directory_fd: std::os::fd::RawFd,
    name: &CString,
    owner_uid: u32,
    group_gid: u32,
    mode: u32,
) -> io::Result<()> {
    if unsafe {
        libc::fchownat(
            directory_fd,
            name.as_ptr(),
            owner_uid,
            group_gid,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // chmod has no portable no-follow *at variant. The checked parent is
    // private to the runtime owner and the socket inode is rechecked after.
    let proc_path = anchored_path(directory_fd, name.as_bytes());
    fs::set_permissions(&proc_path, fs::Permissions::from_mode(mode))
}

fn unlink_socket_if_same(
    directory_path: &Path,
    directory: &File,
    name: &CString,
    expected: &libc::stat,
    owner_uid: u32,
) -> io::Result<()> {
    let current = stat_at(directory.as_raw_fd(), name)?;
    validate_socket_stat(&current, owner_uid)?;
    if !same_inode(&current, expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend socket inode changed before cleanup",
        ));
    }
    validate_directory_path(directory_path, directory)?;
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn validate_socket_stat(stat: &libc::stat, owner_uid: u32) -> io::Result<()> {
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to remove a non-socket backend path",
        ));
    }
    if stat.st_uid != owner_uid || stat.st_mode & 0o002 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "backend socket has an unsafe owner or mode",
        ));
    }
    Ok(())
}

fn same_inode(left: &libc::stat, right: &libc::stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
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

fn split_socket_path(path: &Path) -> io::Result<(&Path, CString)> {
    validate_normalized_path(path, "backend socket path")?;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend socket path has no parent",
        )
    })?;
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend socket path has no filename",
        )
    })?;
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket name contains NUL"))?;
    Ok((parent, name))
}

fn validate_normalized_path(path: &Path, description: &str) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{description} must be an absolute normalized path"),
        ));
    }
    Ok(())
}

fn open_directory_chain(path: &Path, create: bool) -> io::Result<File> {
    validate_normalized_path(path, "backend socket directory")?;
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
        let mut fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 && create && io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EEXIST) {
                    return Err(error);
                }
            }
            fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
        }
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}

fn validate_directory(directory: &File, owner_uid: u32) -> io::Result<()> {
    let metadata = directory.metadata()?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend socket parent is not a real directory",
        ));
    }
    if metadata.uid() != owner_uid || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "backend socket parent has an unsafe owner or mode",
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
            "backend runtime directory changed during the operation",
        ));
    }
    Ok(())
}

fn anchored_path(directory_fd: std::os::fd::RawFd, name: &[u8]) -> PathBuf {
    let mut bytes = format!("/proc/self/fd/{directory_fd}/").into_bytes();
    bytes.extend_from_slice(name);
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

fn set_cloexec(fd: std::os::fd::RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bind_socket, cleanup_stale_socket, ensure_socket_directory, peer_credentials, stat_at,
        unlink_socket_if_same, validate_socket_path,
    };
    use std::ffi::CString;
    use std::fs;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn stale_cleanup_only_removes_owned_socket_inodes() {
        let temp = TempDir::new().unwrap();
        let plain = temp.path().join("plain");
        fs::write(&plain, "keep").unwrap();
        assert!(cleanup_stale_socket(&plain).is_err());
        assert_eq!(fs::read_to_string(&plain).unwrap(), "keep");

        let target = temp.path().join("target");
        fs::write(&target, "keep").unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(cleanup_stale_socket(&link).is_err());
        assert!(target.exists());

        let socket = temp.path().join("socket");
        let listener = UnixListener::bind(&socket).unwrap();
        assert!(cleanup_stale_socket(&socket).unwrap());
        assert!(!socket.exists());
        drop(listener);
    }

    #[test]
    fn cleanup_refuses_to_unlink_a_replacement_inode() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("socket");
        let listener = UnixListener::bind(&socket).unwrap();
        let parent = fs::File::open(temp.path()).unwrap();
        let name = CString::new("socket").unwrap();
        let stale = stat_at(parent.as_raw_fd(), &name).unwrap();
        fs::remove_file(&socket).unwrap();
        fs::write(&socket, "preserve replacement").unwrap();
        assert!(
            unlink_socket_if_same(temp.path(), &parent, &name, &stale, unsafe {
                libc::geteuid()
            })
            .is_err()
        );
        assert_eq!(fs::read_to_string(&socket).unwrap(), "preserve replacement");
        drop(listener);
    }

    #[test]
    fn socket_permissions_peer_credentials_and_type_are_checked() {
        let temp = TempDir::new().unwrap();
        let directory = temp.path().join("runtime");
        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };
        ensure_socket_directory(&directory, uid, gid, 0o700).unwrap();
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let socket = directory.join("backend.sock");
        let listener = bind_socket(&socket, uid, gid, 0o600).unwrap();
        validate_socket_path(&socket).unwrap();
        let client = UnixStream::connect(&socket).unwrap();
        let (server, _) = listener.accept().unwrap();
        let credentials = peer_credentials(&server).unwrap();
        assert_eq!(credentials.uid, unsafe { libc::geteuid() });
        assert!(credentials.pid > 0);
        drop(server);
        drop(client);
        drop(listener);

        let normal_file = directory.join("not-socket");
        fs::write(&normal_file, "x").unwrap();
        assert!(validate_socket_path(Path::new(&normal_file)).is_err());
    }

    #[test]
    fn socket_directory_rejects_symlink_components() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("target");
        let link = temp.path().join("link");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let nested = link.join("runtime");
        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };
        assert!(ensure_socket_directory(&nested, uid, gid, 0o700).is_err());
        assert!(!nested.exists());
    }
}
