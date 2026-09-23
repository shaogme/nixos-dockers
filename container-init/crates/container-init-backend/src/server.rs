use crate::protocol::{
    read_message, validate_message, write_message, BackendError, BackendState, BackendStatus,
    ClientMessage, ClientRequest, HelloInfo, PeerCredentials, PreparedHandoff, ProtocolError,
    ReceiptSummary, ServerMessage, ServerResponse, PROTOCOL_VERSION,
};
use crate::socket::{bind_socket, cleanup_stale_socket, ensure_socket_directory, peer_credentials};
use bootstrap_model::{ActionKind, BootstrapConfig, Condition, ConditionValue, Plan};
use container_init_core::{
    CoreError, ExecutionOptions, HandoffCommand, PlanExecutor, ResourceLockManager, RuntimeContext,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_WORKERS: usize = 32;
const MAX_REQUEST_IDS: usize = 65_536;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

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
    _lock: crate::InstanceLock,
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
        if parent != socket_parent || paths.owner_uid != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "backend socket and lock must share a runtime directory owned by this user",
            ));
        }
        prepare_lock_directory(parent, paths.owner_uid)?;
        match crate::InstanceLock::try_acquire(&paths.lock_path)? {
            crate::InstanceClaim::Acquired(lock) => {
                cleanup_stale_socket(&paths.socket_path)?;
                Ok(BackendClaim::Acquired(Self { paths, _lock: lock }))
            }
            crate::InstanceClaim::Occupied => Ok(BackendClaim::AlreadyRunning(
                request_existing_status(&paths.socket_path, timeout)?,
            )),
        }
    }

    pub fn start(
        mut self,
        profile: String,
        config: BootstrapConfig,
        plan: Plan,
        startup_context: RuntimeContext,
        command: &[String],
        mut execution_options: ExecutionOptions,
    ) -> Result<i32, BackendRunError> {
        install_signal_handlers()?;
        let snapshot_id = snapshot_id(&profile, &config, &plan)?;
        let startup_config = startup_config(&config);
        let startup_plan = startup_config.build_plan().map_err(CoreError::Model)?;
        let request_config = Arc::new(request_config(&config));
        let request_plan = Arc::new(request_config.build_plan().map_err(CoreError::Model)?);
        let allowed = allowed_client_values(&request_config)?;
        let config = Arc::new(config);
        let plan = Arc::new(plan);
        execution_options = execution_options
            .with_resource_locks(ResourceLockManager::default())
            .preserve_current_process_in_cgroup();

        let startup = PlanExecutor::new(startup_config, startup_context.clone())
            .with_options(execution_options.clone())
            .execute_prevalidated(&startup_plan, &[])?;
        let identity = startup.identity;
        // Startup receipts describe the one-time reconcile. Request workers
        // must not overwrite that report with a partial identity plan.
        execution_options.receipt_path = None;
        self._lock.allow_peer_group(identity.gid)?;
        let root_service = is_root_service(&config, command, self.paths.owner_uid);
        let handoff =
            PlanExecutor::with_shared_config(Arc::clone(&config), startup_context.clone())
                .build_handoff_command(command)?;
        if unsafe { libc::geteuid() } != 0
            && (identity.uid != unsafe { libc::geteuid() }
                || identity.gid != unsafe { libc::getegid() })
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
        ensure_socket_directory(
            socket_dir,
            owner_uid,
            group_gid,
            if owner_uid == 0 { 0o710 } else { 0o700 },
        )?;
        cleanup_stale_socket(&self.paths.socket_path)?;
        let listener = bind_socket(
            &self.paths.socket_path,
            owner_uid,
            group_gid,
            if owner_uid == 0 { 0o660 } else { 0o600 },
        )?;
        listener.set_nonblocking(true)?;
        let child = match spawn_handoff(&handoff, &identity, root_service, startup_context.cwd()) {
            Ok(child) => child,
            Err(error) => {
                drop(listener);
                cleanup_stale_socket(&self.paths.socket_path)?;
                return Err(error.into());
            }
        };

        let status = BackendStatus {
            state: BackendState::Ready,
            profile: profile.clone(),
            snapshot_id: snapshot_id.clone(),
            backend_pid: std::process::id(),
            initial_child_pid: Some(child.id()),
            started_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            active_requests: 0,
        };
        let runtime = Arc::new(BackendRuntime {
            profile,
            snapshot_id,
            config,
            plan,
            request_config,
            request_plan,
            startup_identity: identity,
            allowed,
            execution_options,
            status: RwLock::new(status),
            active_requests: Arc::new(AtomicUsize::new(0)),
            request_ids: Mutex::new(HashSet::new()),
        });
        supervise(listener, child, &self.paths.socket_path, runtime).map_err(BackendRunError::Io)
    }
}

#[derive(Debug)]
pub enum BackendRunError {
    Core(CoreError),
    Io(io::Error),
}

impl std::fmt::Display for BackendRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Core(error) => error.fmt(f),
            Self::Io(error) => write!(f, "backend failed: {error}"),
        }
    }
}

impl std::error::Error for BackendRunError {}

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

struct BackendRuntime {
    profile: String,
    snapshot_id: String,
    config: Arc<BootstrapConfig>,
    plan: Arc<Plan>,
    request_config: Arc<BootstrapConfig>,
    request_plan: Arc<Plan>,
    startup_identity: container_init_core::ResolvedIdentity,
    allowed: AllowedClientValues,
    execution_options: ExecutionOptions,
    status: RwLock<BackendStatus>,
    active_requests: Arc<AtomicUsize>,
    request_ids: Mutex<HashSet<String>>,
}

struct AllowedClientValues {
    runtime_inputs: BTreeSet<String>,
    environment_names: BTreeSet<String>,
}

impl BackendRuntime {
    fn status(&self) -> BackendStatus {
        let mut status = self
            .status
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        status.active_requests = self.active_requests.load(Ordering::Relaxed);
        status
    }
}

fn prepare_lock_directory(path: &Path, owner_uid: u32) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "backend runtime directory must be an absolute normalized path",
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_lock_directory(&metadata, owner_uid)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_directories(path)?;
            let metadata = fs::symlink_metadata(path)?;
            validate_lock_directory(&metadata, owner_uid)?;
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

fn validate_lock_directory(metadata: &fs::Metadata, owner_uid: u32) -> io::Result<()> {
    if !metadata.file_type().is_dir() {
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

fn create_private_directories(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut current = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => continue,
            Component::Normal(name) => current.push(name),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "backend runtime directory must be absolute and normalized",
                ));
            }
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "backend runtime path contains a non-directory component",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                match builder.create(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn startup_config(config: &BootstrapConfig) -> BootstrapConfig {
    let mut startup = config.clone();
    startup.actions.retain(|action| {
        !action.origin.source.is_workspace()
            && !matches!(
                action.kind,
                ActionKind::ProcessDropPrivileges | ActionKind::HandoffExec
            )
    });
    let action_ids = startup
        .actions
        .iter()
        .map(|action| action.id.clone())
        .collect::<BTreeSet<_>>();
    for action in &mut startup.actions {
        action
            .depends_on
            .retain(|dependency| action_ids.contains(dependency));
    }
    startup
}

fn request_config(config: &BootstrapConfig) -> BootstrapConfig {
    let mut request = config.clone();
    request.actions.retain(|action| {
        action.origin.source.is_workspace()
            || matches!(
                action.kind,
                ActionKind::IdentityResolve
                    | ActionKind::IdentityMapUser
                    | ActionKind::IdentityEnsureHome
                    | ActionKind::ProcessSetUserShell
            )
    });
    let action_ids = request
        .actions
        .iter()
        .map(|action| action.id.clone())
        .collect::<BTreeSet<_>>();
    for action in &mut request.actions {
        action
            .depends_on
            .retain(|dependency| action_ids.contains(dependency));
    }
    request
}

fn allowed_client_values(config: &BootstrapConfig) -> Result<AllowedClientValues, CoreError> {
    let mut runtime_inputs = BTreeSet::new();
    for (name, input) in &config.inputs {
        if input.runtime {
            runtime_inputs.insert(name.clone());
            runtime_inputs.extend(input.aliases.iter().cloned());
        }
    }
    let mut environment_names = runtime_inputs.clone();
    for action in &config.actions {
        if matches!(
            action.kind,
            ActionKind::IdentityResolve
                | ActionKind::IdentityMapUser
                | ActionKind::IdentityEnsureHome
                | ActionKind::ProcessSetUserShell
        ) {
            let condition = action.condition().map_err(CoreError::Model)?;
            collect_environment_names(&condition, &mut environment_names);
        }
    }
    Ok(AllowedClientValues {
        runtime_inputs,
        environment_names,
    })
}

fn collect_environment_names(condition: &Condition, names: &mut BTreeSet<String>) {
    fn value(reference: &ConditionValue, names: &mut BTreeSet<String>) {
        if let ConditionValue::Reference(reference) = reference {
            if let Some(name) = reference.strip_prefix("env.") {
                names.insert(name.to_owned());
            }
        }
    }
    match condition {
        Condition::Equal(left, right) | Condition::NotEqual(left, right) => {
            value(left, names);
            value(right, names);
        }
        Condition::And(parts) | Condition::Or(parts) => {
            for part in parts {
                collect_environment_names(part, names);
            }
        }
        Condition::Not(part) => collect_environment_names(part, names),
        Condition::Always
        | Condition::Boolean(_)
        | Condition::ContextPathExistsOrCreate
        | Condition::Exists(_)
        | Condition::Writable(_)
        | Condition::InputSet(_)
        | Condition::Feature(_) => {}
    }
}

fn snapshot_id(profile: &str, config: &BootstrapConfig, plan: &Plan) -> Result<String, CoreError> {
    let bytes = serde_json::to_vec(&(profile, config, plan)).map_err(|source| {
        CoreError::Serialization {
            operation: "serialize backend snapshot id".to_owned(),
            source,
        }
    })?;
    let hash = bytes
        .into_iter()
        .fold(0xcbf29ce484222325_u64, |mut hash, byte| {
            hash ^= u64::from(byte);
            hash.wrapping_mul(0x100000001b3)
        });
    Ok(format!("{hash:016x}"))
}

fn is_root_service(config: &BootstrapConfig, command: &[String], owner_uid: u32) -> bool {
    owner_uid == 0
        && config
            .handoff
            .ssh_daemon
            .as_deref()
            .is_some_and(|daemon| command.first().is_some_and(|candidate| candidate == daemon))
}

fn spawn_handoff(
    handoff: &HandoffCommand,
    identity: &container_init_core::ResolvedIdentity,
    root_service: bool,
    cwd: &Path,
) -> io::Result<Child> {
    let mut command = Command::new(&handoff.program);
    command
        .args(&handoff.args)
        .current_dir(cwd)
        .process_group(0);
    if root_service {
        command
            .env("HOME", "/root")
            .env("USER", "root")
            .env("LOGNAME", "root");
    } else {
        command
            .env("HOME", &identity.home)
            .env("USER", &identity.user)
            .env("LOGNAME", &identity.user);
    }
    let current_uid = unsafe { libc::geteuid() };
    let current_gid = unsafe { libc::getegid() };
    if !root_service && (identity.uid != current_uid || identity.gid != current_gid) {
        if current_uid != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "backend cannot switch initial handoff credentials",
            ));
        }
        let groups = supplementary_groups(identity)?;
        let uid = identity.uid;
        let gid = identity.gid;
        unsafe {
            command.pre_exec(move || {
                if libc::setgroups(groups.len(), groups.as_ptr()) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::setgid(gid) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::setuid(uid) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command.spawn()
}

fn supplementary_groups(
    identity: &container_init_core::ResolvedIdentity,
) -> io::Result<Vec<libc::gid_t>> {
    let username = std::ffi::CString::new(identity.user.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "user name contains NUL"))?;
    let mut count = 16_i32;
    loop {
        let mut groups = vec![identity.gid as libc::gid_t; count as usize];
        let result = unsafe {
            libc::getgrouplist(
                username.as_ptr(),
                identity.gid as libc::gid_t,
                groups.as_mut_ptr(),
                &mut count,
            )
        };
        if result >= 0 {
            groups.truncate(count as usize);
            groups.sort_unstable();
            groups.dedup();
            if !groups.contains(&(identity.gid as libc::gid_t)) {
                groups.push(identity.gid as libc::gid_t);
            }
            groups.sort_unstable();
            return Ok(groups);
        }
        if count <= 0 || count > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "supplementary group list is invalid or too large",
            ));
        }
    }
}

fn reserve_worker(active: &AtomicUsize) -> bool {
    active
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            (current < MAX_WORKERS).then_some(current + 1)
        })
        .is_ok()
}

struct ActiveRequest(Arc<AtomicUsize>);

impl ActiveRequest {
    fn new(active: Arc<AtomicUsize>) -> Self {
        Self(active)
    }
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
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
    let mut child_status = None;
    let mut stopping_since = None;
    let mut sent_kill = false;

    loop {
        reap_main_child(&mut child, &mut child_status)?;
        if child_status.is_some() && stopping_since.is_none() {
            begin_stopping(&runtime, &mut listener);
            let _ = unsafe { libc::kill(-(child_pid as i32), libc::SIGTERM) };
            stopping_since = Some(Instant::now());
        }

        let signal = PENDING_SIGNAL.swap(0, Ordering::Relaxed);
        if signal != 0 && stopping_since.is_none() {
            begin_stopping(&runtime, &mut listener);
            let _ = unsafe { libc::kill(-(child_pid as i32), signal) };
            stopping_since = Some(Instant::now());
        }

        if let Some(started) = stopping_since {
            if child_status.is_none() && !sent_kill && started.elapsed() >= SHUTDOWN_GRACE {
                let _ = unsafe { libc::kill(-(child_pid as i32), libc::SIGKILL) };
                sent_kill = true;
            }
            if child_status.is_some()
                && runtime.active_requests.load(Ordering::Relaxed) == 0
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
                        set_cloexec(&stream)?;
                        if reserve_worker(&runtime.active_requests) {
                            let runtime = Arc::clone(&runtime);
                            workers.push(thread::spawn(move || {
                                let _active =
                                    ActiveRequest::new(Arc::clone(&runtime.active_requests));
                                let _ = serve_connection(stream, runtime);
                            }));
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
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
    runtime
        .status
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .state = BackendState::Stopping;
    cleanup_stale_socket(socket_path)?;
    let status = child_status
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "initial handoff did not exit"))?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(libc::SIGTERM)))
}

fn begin_stopping(runtime: &BackendRuntime, listener: &mut Option<UnixListener>) {
    runtime
        .status
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .state = BackendState::Stopping;
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
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid == 0 {
            return Ok(false);
        }
        if pid < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted
                || error.raw_os_error() == Some(libc::ECHILD)
            {
                return Ok(true);
            }
            return Err(error);
        }
    }
}

static PENDING_SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn signal_handler(signal: libc::c_int) {
    PENDING_SIGNAL.store(signal, Ordering::Relaxed);
}

fn install_signal_handlers() -> io::Result<()> {
    PENDING_SIGNAL.store(0, Ordering::Relaxed);
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = signal_handler as *const () as usize;
        action.sa_flags = 0;
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
        }
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn serve_connection(mut stream: UnixStream, runtime: Arc<BackendRuntime>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let peer = match peer_credentials(&stream) {
        Ok(peer) if authorized_peer(&runtime, peer) => peer,
        _ => return Ok(()),
    };
    let mut greeted = false;
    for _ in 0..32 {
        let message = match read_message::<ClientMessage>(&mut stream) {
            Ok(message) => message,
            Err(ProtocolError::Io(error))
                if error.kind() == io::ErrorKind::UnexpectedEof
                    || error.kind() == io::ErrorKind::TimedOut =>
            {
                return Ok(())
            }
            Err(_) => return Ok(()),
        };
        if validate_message(&message).is_err() {
            return Ok(());
        }
        if let Err(error) = reserve_request_id(&runtime, &message.request_id) {
            write_response(
                &mut stream,
                &message.request_id,
                ServerResponse::Error(error),
            )?;
            continue;
        }
        let response = match message.request {
            ClientRequest::Hello => {
                greeted = true;
                ServerResponse::Hello(HelloInfo {
                    state: runtime.status().state,
                    profile: runtime.profile.clone(),
                    snapshot_id: runtime.snapshot_id.clone(),
                    runtime_inputs: runtime.allowed.runtime_inputs.iter().cloned().collect(),
                    environment_names: runtime.allowed.environment_names.iter().cloned().collect(),
                })
            }
            ClientRequest::Exec {
                argv,
                cwd,
                inputs,
                environment,
            } => {
                if !greeted {
                    ServerResponse::Error(backend_error(
                        "protocol",
                        false,
                        "exec requires a hello request on this connection",
                    ))
                } else {
                    match prepare_exec(&runtime, peer, argv, cwd, inputs, environment) {
                        Ok(prepared) => ServerResponse::Prepared(prepared),
                        Err(error) => ServerResponse::Error(error),
                    }
                }
            }
            ClientRequest::Status => ServerResponse::Status(runtime.status()),
            ClientRequest::Plan => ServerResponse::Plan(plan_response(&runtime)),
            ClientRequest::Doctor => ServerResponse::Doctor(doctor_response(&runtime)),
        };
        write_response(&mut stream, &message.request_id, response)?;
    }
    Ok(())
}

fn reserve_request_id(runtime: &BackendRuntime, request_id: &str) -> Result<(), BackendError> {
    let mut ids = runtime
        .request_ids
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if ids.contains(request_id) {
        return Err(backend_error(
            "duplicate_request",
            false,
            "request id has already been used",
        ));
    }
    if ids.len() >= MAX_REQUEST_IDS {
        return Err(backend_error(
            "capacity",
            true,
            "backend request id capacity is exhausted",
        ));
    }
    ids.insert(request_id.to_owned());
    Ok(())
}

fn authorized_peer(runtime: &BackendRuntime, peer: PeerCredentials) -> bool {
    authorized_peer_uid(runtime.startup_identity.uid, peer)
}

fn authorized_peer_uid(startup_uid: u32, peer: PeerCredentials) -> bool {
    peer.pid > 0 && (peer.uid == 0 || peer.uid == startup_uid)
}

fn prepare_exec(
    runtime: &BackendRuntime,
    peer: PeerCredentials,
    argv: Vec<String>,
    cwd: PathBuf,
    inputs: BTreeMap<String, String>,
    environment: BTreeMap<String, String>,
) -> Result<PreparedHandoff, BackendError> {
    if runtime.status().state != BackendState::Ready {
        return Err(backend_error(
            "backend_not_ready",
            true,
            "backend is not ready to prepare commands",
        ));
    }
    if argv.len() > 256 {
        return Err(backend_error(
            "invalid_request",
            false,
            "too many argv fields",
        ));
    }
    if inputs
        .keys()
        .any(|name| !runtime.allowed.runtime_inputs.contains(name))
    {
        return Err(backend_error(
            "invalid_input",
            false,
            "request contains an undeclared or non-runtime input",
        ));
    }
    if environment
        .keys()
        .any(|name| !runtime.allowed.environment_names.contains(name))
    {
        return Err(backend_error(
            "invalid_environment",
            false,
            "request contains an environment name not used by identity reconciliation",
        ));
    }
    if inputs
        .values()
        .chain(environment.values())
        .any(|value| value.contains('\0'))
    {
        return Err(backend_error(
            "invalid_request",
            false,
            "runtime values may not contain NUL bytes",
        ));
    }
    let cwd = accessible_cwd(&cwd, peer).map_err(|_| {
        backend_error(
            "permission",
            false,
            "request working directory is unavailable to the peer",
        )
    })?;
    validate_inputs(&runtime.request_config, &inputs)?;
    let mut context = RuntimeContext::new(cwd.clone()).with_environment(environment);
    for (name, value) in inputs {
        context = context.with_cli_input(name, value);
    }
    let executor = PlanExecutor::with_shared_config(Arc::clone(&runtime.request_config), context)
        .with_options(runtime.execution_options.clone());
    let identity = executor
        .resolve_identity_prevalidated()
        .map_err(|error| backend_core_error(&error))?;
    let root_service = is_root_service(&runtime.config, &argv, unsafe { libc::geteuid() });
    if root_service && peer.uid != 0 {
        return Err(backend_error(
            "permission",
            false,
            "root service handoff requires a root peer",
        ));
    }
    if peer.uid != 0 && (identity.uid != peer.uid || identity.run_as_root) {
        return Err(backend_error(
            "permission",
            false,
            "non-root peers may only hand off as their own UID",
        ));
    }
    let report = executor
        .execute_prevalidated(&runtime.request_plan, &[])
        .map_err(|error| backend_core_error(&error))?;
    let command = executor
        .build_handoff_command(&argv)
        .map_err(|error| backend_core_error(&error))?;
    if !command.program.is_absolute() || command.args.iter().any(|argument| argument.contains('\0'))
    {
        return Err(backend_error(
            "invalid_snapshot",
            false,
            "snapshot produced an invalid handoff command",
        ));
    }
    let supplemental_groups = if root_service {
        Vec::new()
    } else {
        supplementary_groups(&report.identity)
            .map_err(|_| backend_error("identity", false, "supplementary groups unavailable"))?
            .into_iter()
            .map(|group| group as u32)
            .collect()
    };
    let login_environment = BTreeMap::from([
        (
            "HOME".to_owned(),
            if root_service {
                "/root".to_owned()
            } else {
                report.identity.home.to_string_lossy().into_owned()
            },
        ),
        (
            "USER".to_owned(),
            if root_service {
                "root".to_owned()
            } else {
                report.identity.user.clone()
            },
        ),
        (
            "LOGNAME".to_owned(),
            if root_service {
                "root".to_owned()
            } else {
                report.identity.user.clone()
            },
        ),
    ]);
    let receipt_summary = ReceiptSummary {
        succeeded: report.succeeded(),
        action_count: report.outcomes.len(),
        warning_count: report.warnings.len(),
    };
    let prepared_identity = report.identity;
    Ok(PreparedHandoff {
        command,
        cwd,
        login_environment,
        identity: prepared_identity,
        supplemental_groups,
        root_service,
        receipt_summary,
    })
}

fn set_cloexec(stream: &UnixStream) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let fd = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn validate_inputs(
    config: &BootstrapConfig,
    inputs: &BTreeMap<String, String>,
) -> Result<(), BackendError> {
    let mut targets = BTreeSet::new();
    for (name, value) in inputs {
        let declaration = config
            .inputs
            .get(name)
            .or_else(|| {
                config
                    .inputs
                    .values()
                    .find(|input| input.aliases.iter().any(|alias| alias == name))
            })
            .filter(|input| input.runtime)
            .ok_or_else(|| {
                backend_error("invalid_input", false, "runtime input is not declared")
            })?;
        if !targets.insert(declaration.target.as_str()) {
            return Err(backend_error(
                "invalid_input",
                false,
                "request sets an input target more than once",
            ));
        }
        declaration.parse_value(value).map_err(|_| {
            backend_error("invalid_input", false, "runtime input has an invalid value")
        })?;
    }
    Ok(())
}

fn accessible_cwd(path: &Path, peer: PeerCredentials) -> io::Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cwd must be absolute",
        ));
    }
    let path = fs::canonicalize(path)?;
    if !fs::metadata(&path)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "cwd is not a directory",
        ));
    }
    if peer.uid == 0 {
        return Ok(path);
    }
    let groups = peer_groups(peer.pid, peer.gid);
    let mut current = PathBuf::from("/");
    for component in path.components() {
        if let Component::Normal(name) = component {
            current.push(name);
            let metadata = fs::metadata(&current)?;
            let mode = metadata.mode();
            let searchable = if metadata.uid() == peer.uid {
                mode & 0o100 != 0
            } else if groups.contains(&metadata.gid()) {
                mode & 0o010 != 0
            } else {
                mode & 0o001 != 0
            };
            if !searchable {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "peer cannot traverse cwd path",
                ));
            }
        }
    }
    Ok(path)
}

fn peer_groups(pid: u32, primary_gid: u32) -> BTreeSet<u32> {
    let mut groups = BTreeSet::from([primary_gid]);
    if let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) {
        if let Some(line) = status.lines().find(|line| line.starts_with("Groups:")) {
            groups.extend(
                line.split_whitespace()
                    .skip(1)
                    .filter_map(|group| group.parse::<u32>().ok()),
            );
        }
    }
    groups
}

fn plan_response(runtime: &BackendRuntime) -> serde_json::Value {
    serde_json::json!({
        "online": true,
        "profile": runtime.profile,
        "snapshot_id": runtime.snapshot_id,
        "actions": runtime.plan.actions(),
    })
}

fn doctor_response(runtime: &BackendRuntime) -> serde_json::Value {
    let executable = Path::new(&runtime.config.handoff.runtime);
    let exists = executable.is_file();
    let executable_ok = is_executable(executable);
    serde_json::json!({
        "online": true,
        "ok": exists && executable_ok,
        "profile": runtime.profile,
        "snapshot_id": runtime.snapshot_id,
        "backend": runtime.status(),
        "identity": runtime.startup_identity,
        "handoff": {
            "runtime": runtime.config.handoff.runtime,
            "exists": exists,
            "executable": executable_ok,
        },
    })
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn backend_core_error(error: &CoreError) -> BackendError {
    BackendError {
        class: format!("{:?}", error.class()).to_ascii_lowercase(),
        retryable: false,
        message: "request identity could not be reconciled".to_owned(),
        action_id: None,
        path: None,
    }
}

fn backend_error(class: &str, retryable: bool, message: &str) -> BackendError {
    BackendError {
        class: class.to_owned(),
        retryable,
        message: message.to_owned(),
        action_id: None,
        path: None,
    }
}

fn write_response(
    stream: &mut UnixStream,
    request_id: &str,
    response: ServerResponse,
) -> io::Result<()> {
    write_message(
        stream,
        &ServerMessage {
            version: PROTOCOL_VERSION,
            request_id: request_id.to_owned(),
            response,
        },
    )
    .map_err(|error| error.as_io_error())
}

fn request_existing_status(path: &Path, timeout: Duration) -> io::Result<BackendStatus> {
    let started = Instant::now();
    let mut delay = Duration::from_millis(10);
    loop {
        match crate::BackendClient::new(path, Duration::from_millis(200)).status() {
            Ok(status) => return Ok(status),
            Err(error) if started.elapsed() < timeout => {
                let _ = error;
                thread::sleep(delay.min(timeout.saturating_sub(started.elapsed())));
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
    use super::authorized_peer_uid;
    use crate::PeerCredentials;

    #[test]
    fn peer_authorization_allows_root_and_startup_uid_only() {
        let startup_uid = 1000;
        assert!(authorized_peer_uid(
            startup_uid,
            PeerCredentials {
                pid: 42,
                uid: 0,
                gid: 0,
            }
        ));
        assert!(authorized_peer_uid(
            startup_uid,
            PeerCredentials {
                pid: 43,
                uid: startup_uid,
                gid: 100,
            }
        ));
        assert!(!authorized_peer_uid(
            startup_uid,
            PeerCredentials {
                pid: 44,
                uid: 1001,
                gid: 100,
            }
        ));
        assert!(!authorized_peer_uid(
            startup_uid,
            PeerCredentials {
                pid: 0,
                uid: startup_uid,
                gid: 100,
            }
        ));
    }
}
