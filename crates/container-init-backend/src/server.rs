use self::{
    lifecycle::BackendSupervisor,
    runtime::{BackendRuntime, BackendRuntimeOptions},
};
use crate::{
    client::BackendClient,
    instance::{InstanceClaim, InstanceLock},
    socket::BackendSocket,
};
use container_init_bootstrap_model::{BootstrapConfig, Plan};
use container_init_core::{
    CoreError, ExecutionOptions, PlanExecutor, ResourceLockManager, RuntimeContext,
};
use container_init_protocol::{BackendState, BackendStatus};
use libc::{getegid, geteuid};
use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
    process,
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod connection;
mod identity;
mod inspection;
mod lifecycle;
mod runtime;
mod transaction;

#[derive(Clone, Debug)]
pub struct BackendPaths {
    pub socket_path: PathBuf,
    pub lock_path: PathBuf,
    pub owner_uid: u32,
}

pub enum BackendClaim {
    Acquired(BackendLease),
    AlreadyRunning(BackendStatus),
}

pub struct BackendLease {
    paths: BackendPaths,
    _lock: InstanceLock,
}

#[derive(Clone, Debug)]
pub struct BackendStartOptions {
    pub profile: String,
    pub config: BootstrapConfig,
    pub plan: Plan,
    pub startup_context: RuntimeContext,
    pub command: Vec<String>,
    pub execution_options: ExecutionOptions,
}

impl BackendLease {
    pub fn claim(paths: BackendPaths, timeout: Duration) -> io::Result<BackendClaim> {
        let parent = paths.lock_path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend lock path has no parent",
            )
        })?;
        let socket_parent = paths.socket_path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend socket path has no parent",
            )
        })?;
        if parent != socket_parent || paths.owner_uid != unsafe { geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend socket and lock must share a runtime directory owned by this user",
            ));
        }
        BackendRuntime::prepare_lock_directory(parent, paths.owner_uid)?;
        match InstanceLock::try_acquire(&paths.lock_path)? {
            InstanceClaim::Acquired(lock) => {
                BackendSocket::cleanup_stale_socket(&paths.socket_path)?;
                Ok(BackendClaim::Acquired(Self { paths, _lock: lock }))
            }
            InstanceClaim::Occupied => Ok(BackendClaim::AlreadyRunning(request_existing_status(
                &paths.socket_path,
                timeout,
            )?)),
        }
    }

    pub fn start(mut self, mut options: BackendStartOptions) -> Result<i32, BackendRunError> {
        BackendSupervisor::install_signal_handlers()?;
        let snapshot_id = BackendRuntime::calculate_snapshot_id(
            &options.profile,
            &options.config,
            &options.plan,
        )?;
        let startup_config = BackendRuntime::startup_config(&options.config);
        let startup_plan = startup_config.build_plan().map_err(CoreError::Model)?;
        let request_config = Arc::new(BackendRuntime::trimmed_request_config(&options.config));
        let request_plan = Arc::new(request_config.build_plan().map_err(CoreError::Model)?);
        let allowed = BackendRuntime::allowed_client_values(&request_config)?;
        let config = Arc::new(options.config);
        let plan = Arc::new(options.plan);
        options.execution_options = options
            .execution_options
            .with_resource_locks(ResourceLockManager::default())
            .preserve_current_process_in_cgroup();

        let startup = PlanExecutor::new(startup_config, options.startup_context.clone())
            .with_options(options.execution_options.clone())
            .execute_prevalidated(&startup_plan, &[])?;
        let identity = startup.identity;
        options.execution_options.receipt_path = None;
        self._lock.allow_peer_group(identity.gid)?;
        let root_service = BackendSupervisor::is_root_service(
            &config,
            &options.command,
            self.paths.owner_uid,
            true,
        );
        let handoff =
            PlanExecutor::with_shared_config(Arc::clone(&config), options.startup_context.clone())
                .build_initial_handoff_command(&options.command)?;
        if unsafe { geteuid() } != 0
            && (identity.uid != unsafe { geteuid() } || identity.gid != unsafe { getegid() })
        {
            return Err(CoreError::Permission {
                action: None,
                message: "rootless backend cannot hand off to a different identity".to_owned(),
            }
            .into());
        }

        let owner_uid = self.paths.owner_uid;
        let group_gid = if owner_uid == 0 {
            identity.gid
        } else {
            owner_uid
        };
        let socket_dir = self.paths.socket_path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "socket path has no parent")
        })?;
        BackendSocket::ensure_socket_directory(
            socket_dir,
            owner_uid,
            group_gid,
            if owner_uid == 0 { 0o710 } else { 0o700 },
        )?;
        BackendSocket::cleanup_stale_socket(&self.paths.socket_path)?;
        let listener = BackendSocket::bind_socket(
            &self.paths.socket_path,
            owner_uid,
            group_gid,
            if owner_uid == 0 { 0o660 } else { 0o600 },
        )?;
        listener.set_nonblocking(true)?;
        let child = match BackendSupervisor::spawn_handoff(
            &handoff,
            &identity,
            root_service,
            options.startup_context.cwd(),
        ) {
            Ok(child) => child,
            Err(error) => {
                drop(listener);
                BackendSocket::cleanup_stale_socket(&self.paths.socket_path)?;
                return Err(error.into());
            }
        };
        let status = BackendStatus {
            state: BackendState::Ready,
            profile: options.profile.clone(),
            snapshot_id: snapshot_id.clone(),
            backend_pid: process::id(),
            initial_child_pid: Some(child.id()),
            started_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            active_connections: 0,
        };
        let runtime = Arc::new(BackendRuntime::new(BackendRuntimeOptions {
            profile: options.profile,
            snapshot_id,
            config,
            plan,
            request_config,
            request_plan,
            startup_identity: identity,
            allowed,
            execution_options: options.execution_options,
            status,
        }));
        BackendSupervisor::supervise(listener, child, &self.paths.socket_path, runtime)
            .map_err(BackendRunError::Io)
    }
}

#[derive(Debug)]
pub enum BackendRunError {
    Core(CoreError),
    Io(io::Error),
}

impl fmt::Display for BackendRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core(error) => error.fmt(f),
            Self::Io(error) => write!(f, "backend failed: {error}"),
        }
    }
}

impl Error for BackendRunError {}

impl From<CoreError> for BackendRunError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

impl From<io::Error> for BackendRunError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn request_existing_status(path: &Path, timeout: Duration) -> io::Result<BackendStatus> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "status deadline is invalid"))?;
    let mut delay = Duration::from_millis(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "backend status request exceeded its deadline",
            ));
        }
        match BackendClient::new(path, remaining).status_until(deadline) {
            Ok(status) => return Ok(status),
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                let remaining = deadline.saturating_duration_since(Instant::now());
                thread::sleep(delay.min(remaining));
                delay = delay.saturating_mul(2).min(Duration::from_millis(100));
            }
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!("backend owns the singleton lock but is not ready: {error}"),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::connection::ConnectionService;
    use container_init_protocol::PeerCredentials;

    #[test]
    fn peer_authorization_allows_root_and_startup_uid_only() {
        let startup_uid = 1000;
        assert!(ConnectionService::peer_uid_authorized(
            startup_uid,
            PeerCredentials {
                pid: 42,
                uid: 0,
                gid: 0,
            }
        ));
        assert!(ConnectionService::peer_uid_authorized(
            startup_uid,
            PeerCredentials {
                pid: 43,
                uid: startup_uid,
                gid: 100,
            }
        ));
        assert!(!ConnectionService::peer_uid_authorized(
            startup_uid,
            PeerCredentials {
                pid: 44,
                uid: 1001,
                gid: 100,
            }
        ));
        assert!(!ConnectionService::peer_uid_authorized(
            startup_uid,
            PeerCredentials {
                pid: 0,
                uid: startup_uid,
                gid: 100,
            }
        ));
    }
}
