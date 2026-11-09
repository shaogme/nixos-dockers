use super::{
    child_reaper::{ChildReaper, DrainOutcome},
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
    c_int, c_void, getegid, geteuid, kill, nfds_t, pid_t, poll, pollfd, prctl, read as libc_read,
    setgid, setgroups, setuid, sigaction, sigaddset, sigemptyset, signalfd, signalfd_siginfo,
    sigprocmask, sigset_t, POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT, PR_SET_CHILD_SUBREAPER,
    SFD_CLOEXEC, SFD_NONBLOCK, SIGCHLD, SIGHUP, SIGINT, SIGKILL, SIGQUIT, SIGTERM, SIG_BLOCK,
    SIG_DFL, SIG_SETMASK, SIG_UNBLOCK,
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
    process::Command,
    ptr,
    time::{Duration, Instant},
};

const MAX_CONNECTIONS: usize = 32;
const MAX_BODY_READERS: usize = 4;
const MAX_ACCEPTS_PER_TICK: usize = 64;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const SHUTDOWN_HARD_LIMIT: Duration = Duration::from_secs(6);
const POLL_INTERVAL_MS: c_int = 50;
const SIGNAL_RECORD_BATCH: usize = 16;
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

pub(super) struct BackendSignalSetup {
    signal_fd: File,
    _mask: ShutdownSignalMask,
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
    ) -> io::Result<u32> {
        spawn_handoff(handoff, identity, root_service, cwd)
    }

    pub(super) fn supervise(
        listener: UnixListener,
        handoff_pid: u32,
        signal_setup: BackendSignalSetup,
        runtime: &mut BackendRuntime,
    ) -> io::Result<i32> {
        supervise(listener, handoff_pid, signal_setup, runtime)
    }

    pub(super) fn initialize() -> io::Result<BackendSignalSetup> {
        Self::initialize_with_signal_fd(create_signal_fd)
    }

    pub(super) fn initialize_with_signal_fd(
        create_fd: impl FnOnce(&sigset_t) -> io::Result<File>,
    ) -> io::Result<BackendSignalSetup> {
        let mask = shutdown_signal_mask()?;
        let mut previous: sigset_t = unsafe { mem::zeroed() };
        if unsafe { sigprocmask(SIG_BLOCK, &mask, &mut previous) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mask_guard = ShutdownSignalMask { previous };
        set_sigchld_default()?;
        let signal_fd = create_fd(&mask)?;
        if unsafe { prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(BackendSignalSetup {
            signal_fd,
            _mask: mask_guard,
        })
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
) -> io::Result<u32> {
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
            reset_handoff_signal_dispositions()?;
            let mask = shutdown_signal_mask()?;
            if sigprocmask(SIG_UNBLOCK, &mask, ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    Ok(child.id())
}

fn supervise(
    listener: UnixListener,
    handoff_pid: u32,
    signal_setup: BackendSignalSetup,
    runtime: &mut BackendRuntime,
) -> io::Result<i32> {
    let child_pid = handoff_pid as pid_t;
    let mut listener = Some(listener);
    let signal_fd = signal_setup.signal_fd.as_raw_fd();
    let mut connections = HashMap::<RawFd, Connection>::new();
    let mut reaper = ChildReaper::new(child_pid);
    let mut stopping_since = None;
    let mut sent_kill = false;

    loop {
        let drain_outcome = reaper.drain()?;
        if drain_outcome == DrainOutcome::NoChildren && !reaper.handoff_exited() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "initial handoff status is unavailable after all children were reaped",
            ));
        }

        if let Some(signal) = shutdown_signal_for_iteration(reaper.handoff_exited(), None) {
            if stopping_since.is_none() {
                begin_stopping(runtime, &mut listener, &mut connections);
                let _ = unsafe { kill(-(child_pid as i32), signal) };
                stopping_since = Some(Instant::now());
            }
        }
        let shutdown_signal =
            shutdown_signal_for_iteration(false, read_shutdown_signals(signal_fd)?);
        if let Some(signal) = shutdown_signal {
            if stopping_since.is_none() {
                begin_stopping(runtime, &mut listener, &mut connections);
                let _ = unsafe { kill(-(child_pid as i32), signal) };
                stopping_since = Some(Instant::now());
            }
        }

        if let Some(started) = stopping_since {
            if !sent_kill && started.elapsed() >= SHUTDOWN_GRACE {
                let _ = unsafe { kill(-(child_pid as i32), SIGKILL) };
                sent_kill = true;
            }
            if reaper.handoff_exited()
                && connections.is_empty()
                && drain_outcome == DrainOutcome::NoChildren
            {
                break;
            }
            if started.elapsed() >= SHUTDOWN_HARD_LIMIT {
                break;
            }
        }

        runtime.set_active_connections(connections.len());
        let (descriptors, targets) =
            poll_descriptors(listener.as_ref(), signal_fd.as_raw_fd(), &connections);
        let timeout = if drain_outcome == DrainOutcome::BudgetExhausted {
            0
        } else {
            stopping_since
                .map(|started| poll_timeout(started.elapsed(), sent_kill))
                .unwrap_or(POLL_INTERVAL_MS)
        };
        let result = unsafe {
            poll(
                descriptors.as_ptr() as *mut pollfd,
                descriptors.len() as nfds_t,
                timeout,
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
    let status = reaper.take_handoff_status()?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(SIGTERM)))
}

fn poll_timeout(elapsed: Duration, sent_kill: bool) -> c_int {
    let hard_remaining = SHUTDOWN_HARD_LIMIT.saturating_sub(elapsed);
    let remaining = if sent_kill {
        hard_remaining
    } else {
        hard_remaining.min(SHUTDOWN_GRACE.saturating_sub(elapsed))
    };
    let millis = remaining.as_millis().clamp(1, POLL_INTERVAL_MS as u128);
    millis as c_int
}

fn shutdown_signal_for_iteration(
    handoff_exited: bool,
    observed_shutdown_signal: Option<c_int>,
) -> Option<c_int> {
    if handoff_exited {
        Some(SIGTERM)
    } else {
        observed_shutdown_signal
    }
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

fn shutdown_signal_mask() -> io::Result<sigset_t> {
    let mut mask: sigset_t = unsafe { mem::zeroed() };
    if unsafe { sigemptyset(&mut mask) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for signal in [SIGCHLD, SIGTERM, SIGINT, SIGHUP, SIGQUIT] {
        if unsafe { sigaddset(&mut mask, signal) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(mask)
}

fn set_sigchld_default() -> io::Result<()> {
    set_signal_default(SIGCHLD)
}

fn reset_handoff_signal_dispositions() -> io::Result<()> {
    for signal in [SIGTERM, SIGINT, SIGHUP, SIGQUIT] {
        set_signal_default(signal)?;
    }
    Ok(())
}

fn set_signal_default(signal: c_int) -> io::Result<()> {
    let mut action: sigaction = unsafe { mem::zeroed() };
    if unsafe { sigemptyset(&mut action.sa_mask) } != 0 {
        return Err(io::Error::last_os_error());
    }
    action.sa_sigaction = SIG_DFL;
    action.sa_flags = 0;
    if unsafe { sigaction(signal, &action, ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn create_signal_fd(mask: &sigset_t) -> io::Result<File> {
    let fd = unsafe { signalfd(-1, mask, SFD_CLOEXEC | SFD_NONBLOCK) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn read_shutdown_signals(fd: RawFd) -> io::Result<Option<c_int>> {
    let mut first_shutdown_signal = None;
    loop {
        let mut records: [signalfd_siginfo; SIGNAL_RECORD_BATCH] = unsafe { mem::zeroed() };
        let bytes_read = unsafe {
            libc_read(
                fd,
                records.as_mut_ptr() as *mut c_void,
                mem::size_of_val(&records),
            )
        };
        if bytes_read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(first_shutdown_signal);
            }
            return Err(error);
        }
        if bytes_read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "signal descriptor closed while backend was running",
            ));
        }
        let record_size = mem::size_of::<signalfd_siginfo>();
        if bytes_read as usize % record_size != 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "signal descriptor returned a truncated record",
            ));
        }
        for record in records.iter().take(bytes_read as usize / record_size) {
            let signal = record.ssi_signo as c_int;
            if signal != SIGCHLD && first_shutdown_signal.is_none() {
                first_shutdown_signal = Some(signal);
            }
        }
    }
}

#[cfg(test)]
#[test]
fn handoff_exit_wins_over_shutdown_signal_observed_in_the_same_iteration() {
    use libc::{SIGHUP, SIGINT, SIGTERM};

    assert_eq!(
        shutdown_signal_for_iteration(true, Some(SIGINT)),
        Some(SIGTERM)
    );
    assert_eq!(
        shutdown_signal_for_iteration(false, Some(SIGHUP)),
        Some(SIGHUP)
    );
    assert_eq!(shutdown_signal_for_iteration(false, None), None);
}
