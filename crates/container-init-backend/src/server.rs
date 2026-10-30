mod connection;
mod identity;
mod inspection;
mod lifecycle;
mod runtime;
mod transaction;

use self::{
    lifecycle::BackendSupervisor,
    runtime::{BackendRuntime, BackendRuntimeOptions},
};
use crate::client::BackendClient;
use container_init_bootstrap_model::{BootstrapConfig, Plan};
use container_init_core::{CoreError, ExecutionOptions, PlanExecutor, RuntimeContext};
use container_init_protocol::{AbstractEndpoint, BackendState, BackendStatus};
use libc::{getegid, geteuid};
use std::{
    error::Error,
    fmt, io,
    os::unix::net::UnixListener,
    process,
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct BackendPaths {
    pub endpoint: AbstractEndpoint,
    pub owner_uid: u32,
    pub peer_uid: u32,
}

pub struct BackendLease {
    paths: BackendPaths,
    listener: Option<UnixListener>,
}

pub enum BackendClaim {
    Acquired(BackendLease),
    AlreadyRunning(BackendStatus),
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

#[derive(Debug)]
pub enum BackendRunError {
    Core(CoreError),
    Io(io::Error),
}

impl BackendLease {
    pub fn claim(paths: BackendPaths, timeout: Duration) -> io::Result<BackendClaim> {
        if paths.owner_uid != unsafe { geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend owner uid must match the current user",
            ));
        }
        match paths.endpoint.bind(32) {
            Ok(listener) => {
                listener.set_nonblocking(true)?;
                Ok(BackendClaim::Acquired(Self {
                    paths,
                    listener: Some(listener),
                }))
            }
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                Ok(BackendClaim::AlreadyRunning(request_existing_status(
                    &paths.endpoint,
                    paths.peer_uid,
                    timeout,
                )?))
            }
            Err(error) => Err(error),
        }
    }

    pub fn start(mut self, mut options: BackendStartOptions) -> Result<i32, BackendRunError> {
        let _signal_mask = BackendSupervisor::block_shutdown_signals()?;
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
            .preserve_current_process_in_cgroup();

        let startup = PlanExecutor::new(startup_config, options.startup_context.clone())
            .with_options(options.execution_options.clone())
            .execute_prevalidated(&startup_plan, &[])?;
        let identity = startup.identity().clone();
        options.execution_options = options.execution_options.without_receipt_path();
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

        let listener = self.listener.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotConnected,
                "backend listener is unavailable",
            )
        })?;
        let child = BackendSupervisor::spawn_handoff(
            &handoff,
            &identity,
            root_service,
            options.startup_context.cwd(),
        )?;
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
        let mut runtime = BackendRuntime::new(BackendRuntimeOptions {
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
        });
        BackendSupervisor::supervise(listener, child, &mut runtime).map_err(BackendRunError::Io)
    }
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

fn request_existing_status(
    endpoint: &AbstractEndpoint,
    peer_uid: u32,
    timeout: Duration,
) -> io::Result<BackendStatus> {
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
        match BackendClient::new(endpoint.clone(), peer_uid, remaining).status_until(deadline) {
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
                    format!(
                        "backend endpoint {} is occupied without a verifiable backend response: {error}",
                        endpoint.display()
                    ),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{connection::ConnectionService, BackendClaim, BackendLease, BackendPaths};
    use container_init_protocol::{AbstractEndpoint, PeerCredentials};
    use libc::geteuid;
    use std::{io, process::id, time::Duration};

    #[test]
    fn endpoint_conflict_fails_before_start_and_is_not_cleared() {
        let endpoint =
            AbstractEndpoint::new(format!("container-init-claim-test.{}", id())).unwrap();
        let occupied = endpoint.bind(4).unwrap();
        let result = BackendLease::claim(
            BackendPaths {
                endpoint: endpoint.clone(),
                owner_uid: unsafe { geteuid() },
                peer_uid: unsafe { geteuid() },
            },
            Duration::from_millis(100),
        );
        let error = match result {
            Ok(_) => panic!("occupied endpoint unexpectedly produced a lease"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(endpoint.bind(4).is_err());
        drop(occupied);
        let lease = BackendLease::claim(
            BackendPaths {
                endpoint,
                owner_uid: unsafe { geteuid() },
                peer_uid: unsafe { geteuid() },
            },
            Duration::from_millis(100),
        )
        .unwrap();
        assert!(matches!(lease, BackendClaim::Acquired(_)));
    }

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
