use super::{
    connection::ConnectionService,
    identity::BackendIdentity,
    runtime::{BackendRuntime, RuntimeErrors},
};
use container_init_bootstrap_model::BootstrapConfig;
use container_init_core::{HandoffCommand, ResolvedIdentity};
use container_init_protocol::{
    AbstractEndpoint, BackendError, PeerCredentials, ServerResponse, FRAME_TIMEOUT_MS,
    MAX_REQUEST_FRAME_BYTES,
};
use libc::{
    c_int, c_void, getegid, geteuid, kill, nfds_t, poll, pollfd, read as libc_read, setgid,
    setgroups, setuid, sigaddset, sigemptyset, signalfd, signalfd_siginfo, sigprocmask, sigset_t,
    waitpid, ECHILD, POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT, SFD_CLOEXEC, SFD_NONBLOCK,
    SIGHUP, SIGINT, SIGKILL, SIGQUIT, SIGTERM, SIG_BLOCK, SIG_SETMASK, SIG_UNBLOCK, WNOHANG,
};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Write},
    mem,
    os::fd::{AsRawFd, FromRawFd, RawFd},
    os::unix::{
        net::{UnixListener, UnixStream},
        process::{CommandExt, ExitStatusExt},
    },
    path::Path,
    process::{Child, Command, ExitStatus},
    ptr,
    time::{Duration, Instant},
};

const MAX_CONNECTIONS: usize = 32;
const MAX_BODY_READERS: usize = 4;
const MAX_ACCEPTS_PER_TICK: usize = 64;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const POLL_INTERVAL_MS: c_int = 50;
const FRAME_TIMEOUT: Duration = Duration::from_millis(FRAME_TIMEOUT_MS);

struct Connection {
    stream: UnixStream,
    peer: PeerCredentials,
    frame_deadline: Instant,
    header: [u8; 4],
    header_read: usize,
    expected_payload: Option<usize>,
    payload: Vec<u8>,
    body_admitted: bool,
    output: Option<Vec<u8>>,
    output_offset: usize,
    hello_completed: bool,
    keepalive_after_write: bool,
}

#[derive(Clone, Copy)]
enum PollTarget {
    Listener,
    Signal,
    Connection(RawFd),
}

pub(super) struct ShutdownSignalMask {
    previous: sigset_t,
}

pub(super) struct BackendSupervisor;

impl BackendSupervisor {
    pub(super) fn is_root_service(
        config: &BootstrapConfig,
        command: &[String],
        owner_uid: u32,
        initial_handoff: bool,
    ) -> bool {
        is_root_service(config, command, owner_uid, initial_handoff)
    }

    pub(super) fn spawn_handoff(
        handoff: &HandoffCommand,
        identity: &ResolvedIdentity,
        root_service: bool,
        cwd: &Path,
    ) -> io::Result<Child> {
        spawn_handoff(handoff, identity, root_service, cwd)
    }

    pub(super) fn supervise(
        listener: UnixListener,
        child: Child,
        runtime: &mut BackendRuntime,
    ) -> io::Result<i32> {
        supervise(listener, child, runtime)
    }

    pub(super) fn block_shutdown_signals() -> io::Result<ShutdownSignalMask> {
        let mask = shutdown_signal_mask()?;
        let mut previous: sigset_t = unsafe { mem::zeroed() };
        if unsafe { sigprocmask(SIG_BLOCK, &mask, &mut previous) } == 0 {
            Ok(ShutdownSignalMask { previous })
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Drop for ShutdownSignalMask {
    fn drop(&mut self) {
        unsafe {
            sigprocmask(SIG_SETMASK, &self.previous, ptr::null_mut());
        }
    }
}

fn is_root_service(
    config: &BootstrapConfig,
    command: &[String],
    owner_uid: u32,
    initial_handoff: bool,
) -> bool {
    owner_uid == 0
        && initial_handoff
        && (config.handoff().root_service()
            || config
                .handoff()
                .ssh_daemon()
                .as_deref()
                .is_some_and(|daemon| command.first().is_some_and(|candidate| candidate == daemon)))
}

fn spawn_handoff(
    handoff: &HandoffCommand,
    identity: &ResolvedIdentity,
    root_service: bool,
    cwd: &Path,
) -> io::Result<Child> {
    let mut command = Command::new(handoff.program());
    command
        .args(handoff.args())
        .current_dir(cwd)
        .process_group(0);
    if identity.uid != 0 {
        command
            .env("CONTAINER_INIT_HANDOFF_UID", identity.uid.to_string())
            .env("CONTAINER_INIT_HANDOFF_GID", identity.gid.to_string());
    }
    if root_service {
        command
            .env("CONTAINER_INIT_HANDOFF_UID", identity.uid.to_string())
            .env("HOME", "/root")
            .env("USER", "root")
            .env("LOGNAME", "root")
            .env("CONTAINER_INIT_HANDOFF_USER", &identity.user);
    } else {
        command
            .env("HOME", &identity.home)
            .env("USER", &identity.user)
            .env("LOGNAME", &identity.user);
    }
    let current_uid = unsafe { geteuid() };
    let current_gid = unsafe { getegid() };
    let change_credentials =
        !root_service && (identity.uid != current_uid || identity.gid != current_gid);
    let groups = if change_credentials {
        if current_uid != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "backend cannot switch initial handoff credentials",
            ));
        }
        Some(BackendIdentity::supplementary_groups(identity)?)
    } else {
        None
    };
    let uid = identity.uid;
    let gid = identity.gid;
    unsafe {
        command.pre_exec(move || {
            if let Some(groups) = &groups {
                if setgroups(groups.len(), groups.as_ptr()) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if setgid(gid) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if setuid(uid) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let mask = shutdown_signal_mask()?;
            if sigprocmask(SIG_UNBLOCK, &mask, ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}

fn supervise(
    listener: UnixListener,
    mut child: Child,
    runtime: &mut BackendRuntime,
) -> io::Result<i32> {
    let child_pid = child.id();
    let mut listener = Some(listener);
    let signal_fd = create_signal_fd()?;
    let mut connections = HashMap::<RawFd, Connection>::new();
    let mut child_status = None;
    let mut stopping_since = None;
    let mut sent_kill = false;

    loop {
        reap_main_child(&mut child, &mut child_status)?;
        if child_status.is_some() && stopping_since.is_none() {
            begin_stopping(runtime, &mut listener, &mut connections);
            let _ = unsafe { kill(-(child_pid as i32), SIGTERM) };
            stopping_since = Some(Instant::now());
        }

        if let Some(signal) = read_shutdown_signal(signal_fd.as_raw_fd())? {
            if stopping_since.is_none() {
                begin_stopping(runtime, &mut listener, &mut connections);
                let _ = unsafe { kill(-(child_pid as i32), signal) };
                stopping_since = Some(Instant::now());
            }
        }

        if let Some(started) = stopping_since {
            if child_status.is_none() && !sent_kill && started.elapsed() >= SHUTDOWN_GRACE {
                let _ = unsafe { kill(-(child_pid as i32), SIGKILL) };
                sent_kill = true;
            }
            if child_status.is_some() && connections.is_empty() && reap_adopted_children()? {
                break;
            }
            if started.elapsed() >= SHUTDOWN_GRACE + Duration::from_secs(1) {
                break;
            }
        }

        runtime.set_active_connections(connections.len());
        let (descriptors, targets) =
            poll_descriptors(listener.as_ref(), signal_fd.as_raw_fd(), &connections);
        let result = unsafe {
            poll(
                descriptors.as_ptr() as *mut pollfd,
                descriptors.len() as nfds_t,
                POLL_INTERVAL_MS,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let ready = descriptors
            .iter()
            .zip(targets)
            .filter_map(|(descriptor, target)| {
                (descriptor.revents != 0).then_some((target, descriptor.revents))
            })
            .collect::<Vec<_>>();
        for (target, events) in ready {
            match target {
                PollTarget::Listener if events & (POLLIN | POLLHUP) != 0 => {
                    if let Some(listener) = listener.as_ref() {
                        accept_connections(listener, runtime, &mut connections)?;
                    }
                }
                PollTarget::Signal => {}
                PollTarget::Connection(fd) => {
                    if events & (POLLERR | POLLNVAL) != 0 {
                        connections.remove(&fd);
                    } else if connections.contains_key(&fd) {
                        handle_connection(fd, events, runtime, &mut connections)?;
                    }
                }
                PollTarget::Listener => {}
            }
        }
        expire_connections(runtime, &mut connections);
    }

    drop(listener.take());
    runtime.mark_stopping();
    let status = child_status
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "initial handoff did not exit"))?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(SIGTERM)))
}

fn poll_descriptors(
    listener: Option<&UnixListener>,
    signal_fd: RawFd,
    connections: &HashMap<RawFd, Connection>,
) -> (Vec<pollfd>, Vec<PollTarget>) {
    let mut descriptors = Vec::with_capacity(connections.len() + 2);
    let mut targets = Vec::with_capacity(connections.len() + 2);
    if let Some(listener) = listener {
        descriptors.push(pollfd {
            fd: listener.as_raw_fd(),
            events: POLLIN,
            revents: 0,
        });
        targets.push(PollTarget::Listener);
    }
    descriptors.push(pollfd {
        fd: signal_fd,
        events: POLLIN,
        revents: 0,
    });
    targets.push(PollTarget::Signal);
    for (fd, connection) in connections {
        descriptors.push(pollfd {
            fd: *fd,
            events: if connection.output.is_some() {
                POLLOUT
            } else {
                POLLIN
            },
            revents: 0,
        });
        targets.push(PollTarget::Connection(*fd));
    }
    (descriptors, targets)
}

fn accept_connections(
    listener: &UnixListener,
    runtime: &BackendRuntime,
    connections: &mut HashMap<RawFd, Connection>,
) -> io::Result<()> {
    let mut accepted = 0;
    while accepted < MAX_ACCEPTS_PER_TICK {
        match listener.accept() {
            Ok((stream, _)) => {
                accepted += 1;
                ConnectionService::set_cloexec(&stream)?;
                let peer = match AbstractEndpoint::peer_credentials(&stream) {
                    Ok(peer) if ConnectionService::is_authorized(runtime, peer) => peer,
                    _ => continue,
                };
                if connections.len() >= MAX_CONNECTIONS {
                    continue;
                }
                stream.set_nonblocking(true)?;
                let fd = stream.as_raw_fd();
                connections.insert(
                    fd,
                    Connection {
                        stream,
                        peer,
                        frame_deadline: Instant::now() + FRAME_TIMEOUT,
                        header: [0; 4],
                        header_read: 0,
                        expected_payload: None,
                        payload: Vec::new(),
                        body_admitted: false,
                        output: None,
                        output_offset: 0,
                        hello_completed: false,
                        keepalive_after_write: false,
                    },
                );
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn handle_connection(
    fd: RawFd,
    events: i16,
    runtime: &mut BackendRuntime,
    connections: &mut HashMap<RawFd, Connection>,
) -> io::Result<()> {
    let mut remove = false;
    if events & (POLLIN | POLLHUP) != 0 {
        let body_readers = connections
            .values()
            .filter(|connection| connection.body_admitted)
            .count();
        let Some(connection) = connections.get_mut(&fd) else {
            return Ok(());
        };
        if !read_connection(connection, runtime, body_readers)? {
            remove = true;
        }
    }
    if !remove && events & POLLOUT != 0 {
        let Some(connection) = connections.get_mut(&fd) else {
            return Ok(());
        };
        if !write_connection(connection)? {
            remove = true;
        }
    }
    if remove {
        connections.remove(&fd);
    }
    Ok(())
}

fn read_connection(
    connection: &mut Connection,
    runtime: &mut BackendRuntime,
    body_readers: usize,
) -> io::Result<bool> {
    let mut scratch = [0_u8; 8192];
    loop {
        let read_result = if connection.expected_payload.is_none() {
            connection
                .stream
                .read(&mut connection.header[connection.header_read..])
        } else {
            connection.stream.read(&mut scratch)
        };
        match read_result {
            Ok(0) => return Ok(false),
            Ok(read) if connection.expected_payload.is_none() => {
                connection.header_read += read;
                if connection.header_read == connection.header.len() {
                    let length = u32::from_be_bytes(connection.header) as usize;
                    if length == 0 {
                        queue_error(
                            connection,
                            RuntimeErrors::backend(
                                "protocol",
                                false,
                                "frame length must be nonzero",
                            ),
                        );
                        return Ok(true);
                    }
                    if length > MAX_REQUEST_FRAME_BYTES {
                        queue_error(
                            connection,
                            RuntimeErrors::backend(
                                "request_too_large",
                                false,
                                "request frame exceeds the maximum frame size",
                            ),
                        );
                        return Ok(true);
                    }
                    if body_readers >= MAX_BODY_READERS {
                        queue_error(
                            connection,
                            RuntimeErrors::backend(
                                "capacity",
                                true,
                                "request body admission limit is full",
                            ),
                        );
                        return Ok(true);
                    }
                    connection.expected_payload = Some(length);
                    connection.payload = Vec::with_capacity(length);
                    connection.body_admitted = true;
                }
            }
            Ok(read) => {
                let expected = connection.expected_payload.unwrap_or_default();
                let remaining = expected.saturating_sub(connection.payload.len());
                let copied = read.min(remaining);
                connection.payload.extend_from_slice(&scratch[..copied]);
                if connection.payload.len() == expected {
                    connection.body_admitted = false;
                    let message = match ConnectionService::decode(&connection.payload) {
                        Ok(message) => message,
                        Err(error) => {
                            queue_error(
                                connection,
                                ConnectionService::protocol_error_response(error),
                            );
                            return Ok(true);
                        }
                    };
                    let (response, keepalive) = ConnectionService::dispatch(
                        runtime,
                        connection.peer,
                        message,
                        connection.hello_completed,
                    );
                    connection.output = Some(ConnectionService::encode(response));
                    connection.output_offset = 0;
                    connection.keepalive_after_write = keepalive;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Ok(false),
        }
        if connection.output.is_some() {
            return Ok(true);
        }
    }
}

fn write_connection(connection: &mut Connection) -> io::Result<bool> {
    let Some(output) = connection.output.as_ref() else {
        return Ok(true);
    };
    loop {
        match connection.stream.write(&output[connection.output_offset..]) {
            Ok(0) => return Ok(false),
            Ok(written) => {
                connection.output_offset += written;
                if connection.output_offset == output.len() {
                    if connection.keepalive_after_write {
                        connection.hello_completed = true;
                        connection.header = [0; 4];
                        connection.header_read = 0;
                        connection.expected_payload = None;
                        connection.payload.clear();
                        connection.body_admitted = false;
                        connection.output = None;
                        connection.output_offset = 0;
                        connection.keepalive_after_write = false;
                        connection.frame_deadline = Instant::now() + FRAME_TIMEOUT;
                        return Ok(true);
                    }
                    return Ok(false);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Ok(false),
        }
    }
}

fn queue_error(connection: &mut Connection, error: BackendError) {
    connection.output = Some(ConnectionService::encode(ServerResponse::Error(error)));
    connection.output_offset = 0;
    connection.keepalive_after_write = false;
}

fn expire_connections(runtime: &mut BackendRuntime, connections: &mut HashMap<RawFd, Connection>) {
    let now = Instant::now();
    let expired = connections
        .iter()
        .filter_map(|(fd, connection)| (now >= connection.frame_deadline).then_some(*fd))
        .collect::<Vec<_>>();
    let mut remove = Vec::new();
    for fd in expired {
        if let Some(connection) = connections.get_mut(&fd) {
            if connection.output.is_none() {
                connection.output = Some(ConnectionService::encode(ServerResponse::Error(
                    RuntimeErrors::backend(
                        "request_timeout",
                        true,
                        "request frame deadline expired",
                    ),
                )));
                connection.output_offset = 0;
                connection.frame_deadline = now + FRAME_TIMEOUT;
            } else {
                remove.push(fd);
            }
        }
    }
    for fd in remove {
        connections.remove(&fd);
    }
    runtime.set_active_connections(connections.len());
}

fn begin_stopping(
    runtime: &mut BackendRuntime,
    listener: &mut Option<UnixListener>,
    connections: &mut HashMap<RawFd, Connection>,
) {
    runtime.mark_stopping();
    listener.take();
    connections.clear();
}

fn reap_main_child(child: &mut Child, main_status: &mut Option<ExitStatus>) -> io::Result<()> {
    if main_status.is_none() {
        *main_status = child.try_wait()?;
    }
    Ok(())
}

fn reap_adopted_children() -> io::Result<bool> {
    loop {
        let mut status = 0_i32;
        let pid = unsafe { waitpid(-1, &mut status, WNOHANG) };
        if pid == 0 {
            return Ok(false);
        }
        if pid < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted || error.raw_os_error() == Some(ECHILD) {
                return Ok(true);
            }
            return Err(error);
        }
    }
}

fn shutdown_signal_mask() -> io::Result<sigset_t> {
    let mut mask: sigset_t = unsafe { mem::zeroed() };
    if unsafe { sigemptyset(&mut mask) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for signal in [SIGTERM, SIGINT, SIGHUP, SIGQUIT] {
        if unsafe { sigaddset(&mut mask, signal) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(mask)
}

fn create_signal_fd() -> io::Result<File> {
    let mask = shutdown_signal_mask()?;
    let fd = unsafe { signalfd(-1, &mask, SFD_CLOEXEC | SFD_NONBLOCK) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn read_shutdown_signal(fd: RawFd) -> io::Result<Option<c_int>> {
    let mut info: signalfd_siginfo = unsafe { mem::zeroed() };
    let bytes_read = unsafe {
        libc_read(
            fd,
            &mut info as *mut signalfd_siginfo as *mut c_void,
            mem::size_of::<signalfd_siginfo>(),
        )
    };
    if bytes_read == mem::size_of::<signalfd_siginfo>() as isize {
        return Ok(Some(info.ssi_signo as c_int));
    }
    if bytes_read < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock || error.kind() == io::ErrorKind::Interrupted {
            return Ok(None);
        }
        return Err(error);
    }
    Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "shutdown signal descriptor returned a truncated record",
    ))
}
