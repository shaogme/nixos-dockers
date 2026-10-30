use crate::PeerCredentials;
use libc::{c_int, sockaddr_un, socklen_t};
use std::{
    io,
    os::unix::net::{UnixListener, UnixStream},
    str::from_utf8,
    time::Instant,
};

#[cfg(target_os = "linux")]
use libc::{
    bind, c_char, close, connect, fcntl, getsockopt, listen, poll, pollfd, sa_family_t, socket,
    ucred, AF_UNIX, EAGAIN, EALREADY, EBADF, EINPROGRESS, EINTR, F_GETFL, F_SETFL, O_NONBLOCK,
    POLLNVAL, POLLOUT, SOCK_CLOEXEC, SOCK_NONBLOCK, SOCK_STREAM, SOL_SOCKET, SO_ERROR, SO_PEERCRED,
};

#[cfg(target_os = "linux")]
use std::{
    mem::{size_of, size_of_val, zeroed},
    os::fd::{AsRawFd, FromRawFd},
    ptr::copy_nonoverlapping,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AbstractEndpoint {
    name: String,
}

impl AbstractEndpoint {
    pub fn new(name: impl Into<String>) -> io::Result<Self> {
        let name = name.into();
        validate_name(&name)?;
        Ok(Self { name })
    }

    pub fn from_display(value: &str) -> io::Result<Self> {
        let name = value.strip_prefix('@').ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "abstract endpoint display form must start with @",
            )
        })?;
        Self::new(name)
    }

    pub fn from_abstract_bytes(value: &[u8]) -> io::Result<Self> {
        let name = value.strip_prefix(&[0]).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "abstract endpoint address must start with NUL",
            )
        })?;
        let name = from_utf8(name).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "abstract endpoint name must be valid UTF-8",
            )
        })?;
        Self::new(name)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn display(&self) -> String {
        format!("@{}", self.name)
    }

    pub fn sockaddr(&self) -> io::Result<(sockaddr_un, socklen_t)> {
        #[cfg(target_os = "linux")]
        {
            let mut address: sockaddr_un = unsafe { zeroed() };
            let path_capacity = size_of_val(&address.sun_path);
            let address_name = self.name.as_bytes();
            if address_name.len() + 1 > path_capacity {
                return Err(invalid_name(
                    "abstract endpoint name exceeds the Linux socket limit",
                ));
            }
            address.sun_family = AF_UNIX as sa_family_t;
            unsafe {
                copy_nonoverlapping(
                    address_name.as_ptr().cast::<c_char>(),
                    address.sun_path.as_mut_ptr().add(1),
                    address_name.len(),
                );
            }
            let address_length = (size_of::<sa_family_t>() + address_name.len() + 1) as socklen_t;
            Ok((address, address_length))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "abstract Unix sockets require Linux",
            ))
        }
    }

    pub fn bind(&self, backlog: c_int) -> io::Result<UnixListener> {
        #[cfg(target_os = "linux")]
        {
            let (address, address_length) = self.sockaddr()?;
            let descriptor = unsafe { socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0) };
            if descriptor < 0 {
                return Err(io::Error::last_os_error());
            }
            if unsafe {
                bind(
                    descriptor,
                    (&address as *const sockaddr_un).cast(),
                    address_length,
                )
            } != 0
            {
                return close_with_error(descriptor);
            }
            if unsafe { listen(descriptor, backlog) } != 0 {
                return close_with_error(descriptor);
            }
            Ok(unsafe { UnixListener::from_raw_fd(descriptor) })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = backlog;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "abstract Unix sockets require Linux",
            ))
        }
    }

    pub fn connect_until(&self, deadline: Instant) -> io::Result<UnixStream> {
        if deadline <= Instant::now() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "abstract endpoint connection exceeded its deadline",
            ));
        }
        #[cfg(target_os = "linux")]
        {
            let (address, address_length) = self.sockaddr()?;
            let descriptor =
                unsafe { socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0) };
            if descriptor < 0 {
                return Err(io::Error::last_os_error());
            }
            let stream = unsafe { UnixStream::from_raw_fd(descriptor) };
            let result = unsafe {
                connect(
                    stream.as_raw_fd(),
                    (&address as *const sockaddr_un).cast(),
                    address_length,
                )
            };
            if result != 0 {
                let error = io::Error::last_os_error();
                match error.raw_os_error() {
                    Some(EINPROGRESS) | Some(EALREADY) | Some(EAGAIN) | Some(EINTR) => {
                        wait_connect(stream.as_raw_fd(), deadline)?;
                    }
                    _ => return Err(error),
                }
            }
            let flags = unsafe { fcntl(stream.as_raw_fd(), F_GETFL) };
            if flags < 0 || unsafe { fcntl(stream.as_raw_fd(), F_SETFL, flags & !O_NONBLOCK) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(stream)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = deadline;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "abstract Unix sockets require Linux",
            ))
        }
    }

    pub fn peer_credentials(stream: &UnixStream) -> io::Result<PeerCredentials> {
        #[cfg(target_os = "linux")]
        {
            let mut credentials: ucred = unsafe { zeroed() };
            let mut length = size_of::<ucred>() as socklen_t;
            if unsafe {
                getsockopt(
                    stream.as_raw_fd(),
                    SOL_SOCKET,
                    SO_PEERCRED,
                    (&mut credentials as *mut ucred).cast(),
                    &mut length,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            if length as usize != size_of::<ucred>() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
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
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "peer credential checks require Linux SO_PEERCRED",
            ))
        }
    }
}

fn validate_name(name: &str) -> io::Result<()> {
    if name.is_empty() {
        return Err(invalid_name("abstract endpoint name must not be empty"));
    }
    if name.as_bytes().contains(&0) {
        return Err(invalid_name("abstract endpoint name must not contain NUL"));
    }
    #[cfg(target_os = "linux")]
    {
        let address: sockaddr_un = unsafe { zeroed() };
        if name.len() + 1 > size_of_val(&address.sun_path) {
            return Err(invalid_name(
                "abstract endpoint name exceeds the Linux socket limit",
            ));
        }
    }
    Ok(())
}

fn invalid_name(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(target_os = "linux")]
fn close_with_error<T>(descriptor: c_int) -> io::Result<T> {
    let error = io::Error::last_os_error();
    unsafe { close(descriptor) };
    Err(error)
}

#[cfg(target_os = "linux")]
fn wait_connect(descriptor: c_int, deadline: Instant) -> io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "abstract endpoint connection exceeded its deadline",
            ));
        }
        let millis = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut poll_descriptor = pollfd {
            fd: descriptor,
            events: POLLOUT,
            revents: 0,
        };
        let result = unsafe { poll(&mut poll_descriptor, 1, millis) };
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "abstract endpoint connection exceeded its deadline",
            ));
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if poll_descriptor.revents & POLLNVAL != 0 {
            return Err(io::Error::from_raw_os_error(EBADF));
        }
        let mut socket_error = 0;
        let mut length = size_of_val(&socket_error) as socklen_t;
        if unsafe {
            getsockopt(
                descriptor,
                SOL_SOCKET,
                SO_ERROR,
                (&mut socket_error as *mut c_int).cast(),
                &mut length,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if socket_error == 0 {
            return Ok(());
        }
        return Err(io::Error::from_raw_os_error(socket_error));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::AbstractEndpoint;
    use libc::{geteuid, sa_family_t};
    use std::{
        mem::{size_of, size_of_val},
        os::unix::net::UnixStream,
        process::id,
        sync::atomic::{AtomicUsize, Ordering},
        thread,
        time::{Duration, Instant},
    };

    static ENDPOINT_SEQUENCE: AtomicUsize = AtomicUsize::new(1);

    fn unique_endpoint() -> AbstractEndpoint {
        AbstractEndpoint::new(format!(
            "container-init-protocol-test.{}.{}",
            id(),
            ENDPOINT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
        .unwrap()
    }

    #[test]
    fn sockaddr_uses_the_exact_abstract_address_length() {
        let endpoint = AbstractEndpoint::new("container-init.backend.v4").unwrap();
        let (address, length) = endpoint.sockaddr().unwrap();
        assert_eq!(address.sun_path[0], 0);
        assert_eq!(
            length as usize,
            size_of::<sa_family_t>() + "container-init.backend.v4".len() + 1
        );
        assert_eq!(size_of_val(&address.sun_path), 108);
    }

    #[test]
    fn endpoint_names_and_display_forms_are_validated() {
        assert!(AbstractEndpoint::new("").is_err());
        assert!(AbstractEndpoint::new("invalid\0name").is_err());
        assert!(AbstractEndpoint::new("x".repeat(108)).is_err());
        assert!(AbstractEndpoint::from_display("container-init.backend.v4").is_err());
        assert_eq!(
            AbstractEndpoint::from_display("@container-init.backend.v4")
                .unwrap()
                .display(),
            "@container-init.backend.v4"
        );
    }

    #[test]
    fn abstract_endpoint_connects_and_reports_peer_credentials() {
        let endpoint = unique_endpoint();
        let listener = endpoint.bind(4).unwrap();
        let client_endpoint = endpoint.clone();
        let client = thread::spawn(move || {
            client_endpoint
                .connect_until(Instant::now() + Duration::from_secs(1))
                .unwrap()
        });
        let (server, _) = listener.accept().unwrap();
        let client = client.join().unwrap();
        let credentials = AbstractEndpoint::peer_credentials(&server).unwrap();
        assert_eq!(credentials.uid, unsafe { geteuid() });
        assert!(credentials.pid > 0);
        drop(client);
        drop(server);
        drop(listener);
    }

    #[test]
    fn abstract_endpoint_is_never_created_as_a_pathname() {
        let endpoint = unique_endpoint();
        let listener = endpoint.bind(4).unwrap();
        assert!(UnixStream::connect(format!("/tmp/{}", endpoint.name())).is_err());
        drop(listener);
    }
}
