use super::{
    connection::ConnectionService,
    identity::BackendIdentity,
    runtime::{BackendRuntime, RuntimeErrors},
};
use crate::socket::BackendSocket;
use container_init_bootstrap_model::BootstrapConfig;
use container_init_core::{HandoffCommand, ResolvedIdentity};
use container_init_protocol::{PeerCredentials, ServerResponse};
use libc::{
    c_int, getegid, geteuid, kill, setgid, setgroups, setuid, sigaction, sigemptyset, waitpid,
    ECHILD, SIGHUP, SIGINT, SIGKILL, SIGQUIT, SIGTERM, WNOHANG,
};
use std::{
    collections::VecDeque,
    io, mem,
    os::unix::{
        net::{UnixListener, UnixStream},
        process::{CommandExt, ExitStatusExt},
    },
    path::Path,
    process::{Child, Command, ExitStatus},
    ptr,
    sync::{
        atomic::{AtomicI32, AtomicUsize, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_WORKERS: usize = 32;
const MAX_PENDING_CONNECTIONS: usize = 16;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

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
        socket_path: &Path,
        runtime: Arc<BackendRuntime>,
    ) -> io::Result<i32> {
        supervise(listener, child, socket_path, runtime)
    }

    pub(super) fn install_signal_handlers() -> io::Result<()> {
        install_signal_handlers()
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
        && (config.handoff.root_service
            || config
                .handoff
                .ssh_daemon
                .as_deref()
                .is_some_and(|daemon| command.first().is_some_and(|candidate| candidate == daemon)))
}

fn spawn_handoff(
    handoff: &HandoffCommand,
    identity: &ResolvedIdentity,
    root_service: bool,
    cwd: &Path,
) -> io::Result<Child> {
    let mut command = Command::new(&handoff.program);
    command
        .args(&handoff.args)
        .current_dir(cwd)
        .process_group(0);
    if identity.uid != 0 {
        // handoff 运行时可能需要映射后的非 root 身份来设置自身 ACL；root 服务仍使用运行时配置的 peer ACL。
        command
            .env("CONTAINER_INIT_HANDOFF_UID", identity.uid.to_string())
            .env("CONTAINER_INIT_HANDOFF_GID", identity.gid.to_string());
    }
    if root_service {
        command
            .env("HOME", "/root")
            .env("USER", "root")
            .env("LOGNAME", "root")
            // 初始运行时依据此值选择身份，使其与 container-init 已解析的身份一致，包括显式 RUN_AS_ROOT 请求。
            .env("DEVENV_BACKEND_INITIAL_USER", &identity.user);
    } else {
        command
            .env("HOME", &identity.home)
            .env("USER", &identity.user)
            .env("LOGNAME", &identity.user);
    }
    let current_uid = unsafe { geteuid() };
    let current_gid = unsafe { getegid() };
    if !root_service && (identity.uid != current_uid || identity.gid != current_gid) {
        if current_uid != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "backend cannot switch initial handoff credentials",
            ));
        }
        let groups = BackendIdentity::supplementary_groups(identity)?;
        let uid = identity.uid;
        let gid = identity.gid;
        unsafe {
            command.pre_exec(move || {
                if setgroups(groups.len(), groups.as_ptr()) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if setgid(gid) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if setuid(uid) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command.spawn()
}

fn reserve_worker(active: &AtomicUsize) -> bool {
    let mut current = active.load(Ordering::Relaxed);
    loop {
        if current >= MAX_WORKERS {
            return false;
        }
        match active.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

struct ActiveConnection(Arc<AtomicUsize>);

impl ActiveConnection {
    fn new(active: Arc<AtomicUsize>) -> Self {
        active.fetch_add(1, Ordering::Relaxed);
        Self(active)
    }
}

impl Drop for ActiveConnection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

struct WorkerSlot(Arc<AtomicUsize>);

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

struct PendingConnection {
    stream: UnixStream,
    accepted_at: Instant,
    peer: PeerCredentials,
    _active: ActiveConnection,
}

impl PendingConnection {
    fn new(stream: UnixStream, peer: PeerCredentials, active: Arc<AtomicUsize>) -> Self {
        Self {
            stream,
            accepted_at: Instant::now(),
            peer,
            _active: ActiveConnection::new(active),
        }
    }
}

fn supervise(
    listener: UnixListener,
    mut child: Child,
    socket_path: &Path,
    runtime: Arc<BackendRuntime>,
) -> io::Result<i32> {
    let child_pid = child.id();
    let mut listener = Some(listener);
    let mut workers = Vec::<JoinHandle<()>>::new();
    let mut pending = VecDeque::<PendingConnection>::new();
    let worker_slots = Arc::new(AtomicUsize::new(0));
    let mut child_status = None;
    let mut stopping_since = None;
    let mut sent_kill = false;

    loop {
        reap_main_child(&mut child, &mut child_status)?;
        if child_status.is_some() && stopping_since.is_none() {
            begin_stopping(&runtime, &mut listener);
            let _ = unsafe { kill(-(child_pid as i32), SIGTERM) };
            stopping_since = Some(Instant::now());
        }

        let signal = PENDING_SIGNAL.swap(0, Ordering::Relaxed);
        if signal != 0 && stopping_since.is_none() {
            begin_stopping(&runtime, &mut listener);
            let _ = unsafe { kill(-(child_pid as i32), signal) };
            stopping_since = Some(Instant::now());
        }

        if let Some(started) = stopping_since {
            if child_status.is_none() && !sent_kill && started.elapsed() >= SHUTDOWN_GRACE {
                let _ = unsafe { kill(-(child_pid as i32), SIGKILL) };
                sent_kill = true;
            }
            if child_status.is_some()
                && runtime.status().active_connections == 0
                && workers.iter().all(JoinHandle::is_finished)
                && reap_adopted_children()?
            {
                break;
            }
            if started.elapsed() >= SHUTDOWN_GRACE + Duration::from_secs(1) {
                break;
            }
        }

        if let Some(listener) = &listener {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        ConnectionService::set_cloexec(&stream)?;
                        let peer = match BackendSocket::peer_credentials(&stream) {
                            Ok(peer) if ConnectionService::is_authorized(&runtime, peer) => peer,
                            _ => continue,
                        };
                        let connection =
                            PendingConnection::new(stream, peer, runtime.active_connections());
                        if reserve_worker(&worker_slots) {
                            spawn_connection_worker(
                                connection,
                                Arc::clone(&runtime),
                                Arc::clone(&worker_slots),
                                &mut workers,
                            );
                        } else if pending.len() < MAX_PENDING_CONNECTIONS {
                            pending.push_back(connection);
                        } else {
                            reject_busy(connection, "backend connection queue is full");
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
        }
        if stopping_since.is_some() {
            pending.clear();
        } else {
            while reserve_worker(&worker_slots) {
                let Some(connection) = pending.pop_front() else {
                    worker_slots.fetch_sub(1, Ordering::Relaxed);
                    break;
                };
                spawn_reserved_connection_worker(
                    connection,
                    Arc::clone(&runtime),
                    Arc::clone(&worker_slots),
                    &mut workers,
                );
            }
        }
        join_finished(&mut workers);
        thread::sleep(Duration::from_millis(10));
    }

    if let Some(listener) = listener.take() {
        drop(listener);
    }
    if child_status.is_some() && workers.is_empty() {
        let _ = reap_adopted_children()?;
    }
    runtime.mark_stopping();
    BackendSocket::cleanup_stale_socket(socket_path)?;
    let status = child_status
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "initial handoff did not exit"))?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(SIGTERM)))
}

fn spawn_connection_worker(
    connection: PendingConnection,
    runtime: Arc<BackendRuntime>,
    worker_slots: Arc<AtomicUsize>,
    workers: &mut Vec<JoinHandle<()>>,
) {
    spawn_reserved_connection_worker(connection, runtime, worker_slots, workers);
}

fn spawn_reserved_connection_worker(
    connection: PendingConnection,
    runtime: Arc<BackendRuntime>,
    worker_slots: Arc<AtomicUsize>,
    workers: &mut Vec<JoinHandle<()>>,
) {
    let fallback = connection.stream.try_clone().ok();
    let thread_runtime = Arc::clone(&runtime);
    let thread_slots = Arc::clone(&worker_slots);
    match thread::Builder::new()
        .name("container-init-rpc".to_owned())
        .spawn(move || {
            let _slot = WorkerSlot(thread_slots);
            let PendingConnection {
                mut stream,
                accepted_at,
                peer,
                _active,
            } = connection;
            let _ = ConnectionService::serve(&mut stream, thread_runtime, peer, accepted_at);
            drop(_active);
        }) {
        Ok(worker) => workers.push(worker),
        Err(_) => {
            worker_slots.fetch_sub(1, Ordering::Relaxed);
            if let Some(mut stream) = fallback {
                let _ = ConnectionService::write_response(
                    &mut stream,
                    ServerResponse::Error(RuntimeErrors::backend(
                        "capacity",
                        true,
                        "backend could not create a request worker; request was not dispatched",
                    )),
                );
            }
        }
    }
}

fn reject_busy(mut connection: PendingConnection, message: &str) {
    let _ = ConnectionService::write_response(
        &mut connection.stream,
        ServerResponse::Error(RuntimeErrors::backend("capacity", true, message)),
    );
}

fn begin_stopping(runtime: &BackendRuntime, listener: &mut Option<UnixListener>) {
    runtime.mark_stopping();
    listener.take();
}

fn join_finished(workers: &mut Vec<JoinHandle<()>>) {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let worker = workers.swap_remove(index);
            let _ = worker.join();
        } else {
            index += 1;
        }
    }
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

static PENDING_SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn signal_handler(signal: c_int) {
    PENDING_SIGNAL.store(signal, Ordering::Relaxed);
}

fn install_signal_handlers() -> io::Result<()> {
    PENDING_SIGNAL.store(0, Ordering::Relaxed);
    for signal in [SIGTERM, SIGINT, SIGHUP, SIGQUIT] {
        let mut action: sigaction = unsafe { mem::zeroed() };
        action.sa_sigaction = signal_handler as *const () as usize;
        action.sa_flags = 0;
        unsafe {
            sigemptyset(&mut action.sa_mask);
        }
        if unsafe { sigaction(signal, &action, ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
