use crate::{PeerCredentials, ProtocolError};
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::path::{Component, PathBuf};

pub fn ensure_socket_directory(
    path: &Path,
    owner_uid: u32,
    group_gid: u32,
    mode: u32,
) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend socket directory must be an absolute normalized path",
        ));
    }
    create_directories_without_symlinks(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend socket parent is not a real directory",
        ));
    }
    if metadata.uid() != owner_uid && unsafe { libc::geteuid() } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "backend socket parent is not owned by this user",
        ));
    }
    set_owner(path, owner_uid, group_gid)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn create_directories_without_symlinks(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut current = PathBuf::from("/");
    for component in path.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "backend socket path contains a non-directory component",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                builder.create(&current)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub fn cleanup_stale_socket(path: &Path) -> io::Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to remove a non-socket backend path",
        ));
    }
    fs::remove_file(path)?;
    Ok(true)
}

pub fn bind_socket(
    path: &Path,
    owner_uid: u32,
    group_gid: u32,
    mode: u32,
) -> io::Result<UnixListener> {
    let listener = UnixListener::bind(path)?;
    if let Err(error) = set_socket_metadata(path, owner_uid, group_gid, mode) {
        drop(listener);
        let _ = fs::remove_file(path);
        return Err(error);
    }
    set_cloexec(listener.as_raw_fd())?;
    Ok(listener)
}

pub fn validate_socket_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend socket path must be an absolute normalized path",
        ));
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend path is not a Unix socket",
        ));
    }
    if metadata.mode() & 0o002 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "backend socket is world-writable",
        ));
    }
    Ok(())
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

fn set_socket_metadata(path: &Path, owner_uid: u32, group_gid: u32, mode: u32) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bound backend path is not a Unix socket",
        ));
    }
    set_owner(path, owner_uid, group_gid)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn set_owner(path: &Path, owner_uid: u32, group_gid: u32) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let result = unsafe { libc::chown(path.as_ptr(), owner_uid, group_gid) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
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
        bind_socket, cleanup_stale_socket, ensure_socket_directory, peer_credentials,
        validate_socket_path,
    };
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn stale_cleanup_only_removes_socket_inodes() {
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
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert!(cleanup_stale_socket(&socket).unwrap());
        assert!(!socket.exists());
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
