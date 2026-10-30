use {
    crate::worker_protocol::{WorkerProtocol, WorkerRequest, FRAME_LIMIT, PROTOCOL_VERSION},
    dev_env_model::EffectiveIdentity,
    std::{
        collections::BTreeMap,
        env::current_exe,
        error::Error,
        fmt::{Display, Formatter, Result as FmtResult},
        fs::metadata,
        io,
        mem::zeroed,
        path::{Path, PathBuf},
        process::{id, Child, Command, Stdio},
        ptr::null_mut,
        str::from_utf8,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender},
            Arc, Mutex,
        },
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    },
};

#[cfg(unix)]
use {
    libc::{
        dup2, fcntl, getppid, pid_t, prctl, pthread_sigmask, sigaddset, sigemptyset, sigset_t,
        FD_CLOEXEC, F_GETFD, F_SETFD, PR_SET_PDEATHSIG, SIGTERM, SIG_BLOCK,
    },
    std::os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt},
    },
};

const WORKER_NAME: &str = "dev-env-provider-worker";
const CONTROL_FD: i32 = 3;
const RESPONSE_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CANCEL_HARD_DEADLINE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug)]
pub enum CommandError {
    InvalidProgram,
    InvalidArgument,
    InvalidEnvironment,
    InvalidTimeout,
    Spawn {
        source: io::Error,
    },
    Wait {
        source: io::Error,
    },
    Read {
        stream: OutputStream,
        source: io::Error,
    },
    ReaderPanicked {
        stream: OutputStream,
    },
    Kill {
        source: io::Error,
    },
    WorkerUnavailable {
        path: PathBuf,
    },
    WorkerSpawn {
        source: io::Error,
    },
    Worker {
        message: String,
    },
    Protocol {
        message: String,
    },
    SupervisorStopped,
}

#[derive(Clone, Debug)]
pub struct CommandRequest {
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    timeout: Option<Duration>,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
}

impl CommandRequest {
    pub fn new(program: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            environment: BTreeMap::new(),
            timeout: None,
            max_stdout_bytes: 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
        }
    }

    pub fn with_args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.args = args.into_iter().collect();
        self
    }

    pub fn with_environment(mut self, environment: BTreeMap<String, String>) -> Self {
        self.environment = environment;
        self
    }

    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_output_limits(mut self, stdout: usize, stderr: usize) -> Self {
        self.max_stdout_bytes = stdout;
        self.max_stderr_bytes = stderr;
        self
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    pub fn max_stdout_bytes(&self) -> usize {
        self.max_stdout_bytes
    }

    pub fn max_stderr_bytes(&self) -> usize {
        self.max_stderr_bytes
    }

    pub fn validate(&self) -> Result<(), CommandError> {
        if self.program.is_empty() || self.program.contains('\0') {
            return Err(CommandError::InvalidProgram);
        }
        if self.args.iter().any(|argument| argument.contains('\0')) {
            return Err(CommandError::InvalidArgument);
        }
        if self.environment.iter().any(|(name, value)| {
            name.is_empty() || name.contains('=') || name.contains('\0') || value.contains('\0')
        }) {
            return Err(CommandError::InvalidEnvironment);
        }
        if self.timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err(CommandError::InvalidTimeout);
        }
        Ok(())
    }
}

impl Display for CommandError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::InvalidProgram => formatter.write_str("provider program is invalid"),
            Self::InvalidArgument => formatter.write_str("provider argv contains NUL"),
            Self::InvalidEnvironment => formatter.write_str("provider environment is invalid"),
            Self::InvalidTimeout => {
                formatter.write_str("provider command timeout must be positive")
            }
            Self::Spawn { source } => write!(formatter, "could not spawn provider: {source}"),
            Self::Wait { source } => write!(formatter, "could not wait for provider: {source}"),
            Self::Read { stream, source } => {
                write!(formatter, "could not read provider {stream:?}: {source}")
            }
            Self::ReaderPanicked { stream } => {
                write!(formatter, "provider {stream:?} reader thread panicked")
            }
            Self::Kill { source } => write!(formatter, "could not stop provider: {source}"),
            Self::WorkerUnavailable { path } => write!(
                formatter,
                "provider worker is missing or not executable: {}",
                path.display()
            ),
            Self::WorkerSpawn { source } => {
                write!(formatter, "could not start provider worker: {source}")
            }
            Self::Worker { message } => write!(formatter, "provider worker failed: {message}"),
            Self::Protocol { message } => {
                write!(formatter, "provider worker protocol error: {message}")
            }
            Self::SupervisorStopped => {
                formatter.write_str("provider process supervisor has stopped")
            }
        }
    }
}

impl Error for CommandError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Spawn { source }
            | Self::Wait { source }
            | Self::Read { source, .. }
            | Self::Kill { source }
            | Self::WorkerSpawn { source } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    status: Option<i32>,
    timed_out: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl CommandOutput {
    pub fn new(status: Option<i32>, timed_out: bool, stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self {
            status,
            timed_out,
            stdout,
            stderr,
        }
    }

    pub fn success(stdout: impl Into<Vec<u8>>) -> Self {
        Self::new(Some(0), false, stdout.into(), Vec::new())
    }

    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.status == Some(0)
    }

    pub fn status(&self) -> Option<i32> {
        self.status
    }

    pub fn timed_out(&self) -> bool {
        self.timed_out
    }

    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }
}

pub trait CommandExecutor: Send + Sync {
    fn execute(&self, request: &CommandRequest) -> Result<CommandOutput, CommandError>;

    fn execute_with_identity(
        &self,
        request: &CommandRequest,
        _identity: Option<&EffectiveIdentity>,
    ) -> Result<CommandOutput, CommandError> {
        self.execute(request)
    }
}

enum SupervisorMessage {
    Execute {
        request: Box<CommandRequest>,
        identity: Option<EffectiveIdentity>,
        reply: SyncSender<Result<CommandOutput, CommandError>>,
    },
    Stop,
}

#[derive(Clone)]
pub struct ProcessExecutor {
    sender: Sender<SupervisorMessage>,
    worker_path: Arc<PathBuf>,
    stopping: Arc<AtomicBool>,
}

pub struct ProviderProcessSupervisor {
    executor: ProcessExecutor,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl ProviderProcessSupervisor {
    pub fn new() -> Self {
        let worker_path = resolve_worker_path().expect("could not locate provider worker");
        Self::try_new(worker_path).expect("could not start provider process supervisor")
    }

    pub fn try_new(worker_path: impl Into<PathBuf>) -> Result<Self, CommandError> {
        let worker_path = worker_path.into();
        validate_worker(&worker_path)?;
        verify_worker_protocol(&worker_path)?;
        let (sender, receiver) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let executor = ProcessExecutor {
            sender,
            worker_path: Arc::new(worker_path),
            stopping,
        };
        let actor_executor = executor.clone();
        let actor = thread::Builder::new()
            .name("dev-env-provider-supervisor".to_owned())
            .spawn(move || run_supervisor(receiver, actor_executor))
            .map_err(|source| CommandError::WorkerSpawn { source })?;
        Ok(Self {
            executor,
            thread: Mutex::new(Some(actor)),
        })
    }

    pub fn executor(&self) -> ProcessExecutor {
        self.executor.clone()
    }

    pub fn stop(&self) {
        if !self.executor.stopping.swap(true, Ordering::AcqRel) {
            let _ = self.executor.sender.send(SupervisorMessage::Stop);
        }
        if let Some(actor) = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = actor.join();
        }
    }
}

impl Drop for ProviderProcessSupervisor {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Default for ProviderProcessSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandExecutor for ProcessExecutor {
    fn execute(&self, request: &CommandRequest) -> Result<CommandOutput, CommandError> {
        self.execute_request(request, None)
    }

    fn execute_with_identity(
        &self,
        request: &CommandRequest,
        identity: Option<&EffectiveIdentity>,
    ) -> Result<CommandOutput, CommandError> {
        self.execute_request(request, identity)
    }
}

impl ProcessExecutor {
    fn execute_request(
        &self,
        request: &CommandRequest,
        identity: Option<&EffectiveIdentity>,
    ) -> Result<CommandOutput, CommandError> {
        request.validate()?;
        if self.stopping.load(Ordering::Acquire) {
            return Err(CommandError::SupervisorStopped);
        }
        let (reply, result) = mpsc::sync_channel(1);
        self.sender
            .send(SupervisorMessage::Execute {
                request: Box::new(request.clone()),
                identity: identity.cloned(),
                reply,
            })
            .map_err(|_| CommandError::SupervisorStopped)?;
        result.recv().map_err(|_| CommandError::SupervisorStopped)?
    }
}

fn run_supervisor(receiver: Receiver<SupervisorMessage>, executor: ProcessExecutor) {
    while let Ok(message) = receiver.recv() {
        match message {
            SupervisorMessage::Execute {
                request,
                identity,
                reply,
            } => {
                let result = if executor.stopping.load(Ordering::Acquire) {
                    Err(CommandError::SupervisorStopped)
                } else {
                    execute_worker(&executor, &request, identity.as_ref())
                };
                let _ = reply.send(result);
            }
            SupervisorMessage::Stop => break,
        }
    }
    while let Ok(message) = receiver.try_recv() {
        if let SupervisorMessage::Execute { reply, .. } = message {
            let _ = reply.send(Err(CommandError::SupervisorStopped));
        }
    }
}

fn execute_worker(
    executor: &ProcessExecutor,
    request: &CommandRequest,
    identity: Option<&EffectiveIdentity>,
) -> Result<CommandOutput, CommandError> {
    let frame = WorkerRequest::from_command(request, identity).encode()?;
    #[cfg(unix)]
    {
        execute_worker_unix(executor, frame)
    }
    #[cfg(not(unix))]
    {
        let _ = (executor, frame);
        Err(CommandError::Protocol {
            message: "provider workers require Unix domain sockets".to_owned(),
        })
    }
}

#[cfg(unix)]
fn execute_worker_unix(
    executor: &ProcessExecutor,
    request_frame: Vec<u8>,
) -> Result<CommandOutput, CommandError> {
    let (mut control, worker_control) =
        UnixStream::pair().map_err(|source| CommandError::WorkerSpawn { source })?;
    let worker_fd = worker_control.as_raw_fd();
    let parent_pid = id();
    let mut command = Command::new(executor.worker_path.as_path());
    command
        .arg("--control-fd")
        .arg(CONTROL_FD.to_string())
        .arg("--parent-pid")
        .arg(parent_pid.to_string())
        .env_clear()
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(move || {
            if worker_fd != CONTROL_FD && dup2(worker_fd, CONTROL_FD) == -1 {
                return Err(io::Error::last_os_error());
            }
            if worker_fd == CONTROL_FD {
                let flags = fcntl(CONTROL_FD, F_GETFD);
                if flags == -1 || fcntl(CONTROL_FD, F_SETFD, flags & !FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
            if prctl(PR_SET_PDEATHSIG, SIGTERM, 0, 0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            let mut signal_set = zeroed::<sigset_t>();
            if sigemptyset(&mut signal_set) == -1 || sigaddset(&mut signal_set, SIGTERM) == -1 {
                return Err(io::Error::last_os_error());
            }
            let mask_error = pthread_sigmask(SIG_BLOCK, &signal_set, null_mut());
            if mask_error != 0 {
                return Err(io::Error::from_raw_os_error(mask_error));
            }
            if getppid() != parent_pid as pid_t {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "provider worker parent exited during startup",
                ));
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|source| CommandError::WorkerSpawn { source })?;
    drop(worker_control);
    WorkerProtocol::write_frame(&mut control, &request_frame).map_err(|source| {
        CommandError::Worker {
            message: format!("could not send command request: {source}"),
        }
    })?;
    let mut response_stream = control.try_clone().map_err(|source| CommandError::Worker {
        message: source.to_string(),
    })?;
    let (response_sender, response_receiver) = mpsc::sync_channel(1);
    let response_reader = thread::spawn(move || {
        let response = WorkerProtocol::read_frame(&mut response_stream, FRAME_LIMIT);
        let _ = response_sender.send(response);
    });

    let mut cancel_sent = false;
    let mut cancel_deadline = None;
    let response = loop {
        if executor.stopping.load(Ordering::Acquire) && !cancel_sent {
            let _ = WorkerProtocol::write_frame(&mut control, &WorkerProtocol::encode_cancel());
            cancel_sent = true;
            cancel_deadline = Some(Instant::now() + CANCEL_HARD_DEADLINE);
        }
        match response_receiver.recv_timeout(RESPONSE_POLL_INTERVAL) {
            Ok(response) => break response,
            Err(RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                response_reader.join().map_err(|_| CommandError::Worker {
                    message: "worker response reader panicked".to_owned(),
                })?;
                return Err(CommandError::Worker {
                    message: "worker closed the control channel without a response".to_owned(),
                });
            }
            Err(RecvTimeoutError::Timeout) => {
                if cancel_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    child
                        .kill()
                        .map_err(|source| CommandError::Kill { source })?;
                    child
                        .wait()
                        .map_err(|source| CommandError::Wait { source })?;
                    return Err(CommandError::Worker {
                        message: "worker did not stop before the cancellation deadline".to_owned(),
                    });
                }
                if let Some(status) = child
                    .try_wait()
                    .map_err(|source| CommandError::Wait { source })?
                {
                    if status.success() {
                        continue;
                    }
                }
            }
        }
    };
    response_reader.join().map_err(|_| CommandError::Worker {
        message: "worker response reader panicked".to_owned(),
    })?;
    drop(control);
    wait_for_worker(&mut child)?;
    if executor.stopping.load(Ordering::Acquire) {
        return Err(CommandError::SupervisorStopped);
    }
    let response = response.map_err(|source| CommandError::Worker {
        message: format!("could not receive worker response: {source}"),
    })?;
    WorkerProtocol::decode_response(&response)
}

#[cfg(unix)]
fn wait_for_worker(child: &mut Child) -> Result<(), CommandError> {
    let deadline = Instant::now() + CANCEL_HARD_DEADLINE;
    loop {
        if child
            .try_wait()
            .map_err(|source| CommandError::Wait { source })?
            .is_some()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            child
                .kill()
                .map_err(|source| CommandError::Kill { source })?;
            child
                .wait()
                .map_err(|source| CommandError::Wait { source })?;
            return Err(CommandError::Worker {
                message: "worker did not exit after sending its response".to_owned(),
            });
        }
        thread::sleep(RESPONSE_POLL_INTERVAL);
    }
}

fn validate_worker(path: &Path) -> Result<(), CommandError> {
    let metadata = metadata(path).map_err(|_| CommandError::WorkerUnavailable {
        path: path.to_path_buf(),
    })?;
    #[cfg(unix)]
    let executable = metadata.permissions().mode() & 0o111 != 0;
    #[cfg(not(unix))]
    let executable = metadata.is_file();
    if !metadata.is_file() || !executable || !path.is_absolute() {
        return Err(CommandError::WorkerUnavailable {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn verify_worker_protocol(path: &Path) -> Result<(), CommandError> {
    let output = Command::new(path)
        .arg("--protocol-version")
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .map_err(|source| CommandError::WorkerSpawn { source })?;
    if !output.status.success() {
        return Err(CommandError::Worker {
            message: format!(
                "protocol handshake exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    let version = from_utf8(&output.stdout)
        .map_err(|_| CommandError::Protocol {
            message: "worker protocol handshake was not UTF-8".to_owned(),
        })?
        .trim()
        .parse::<u16>()
        .map_err(|_| CommandError::Protocol {
            message: "worker protocol handshake returned an invalid version".to_owned(),
        })?;
    if version != PROTOCOL_VERSION {
        return Err(CommandError::Protocol {
            message: format!(
                "worker protocol version {version} is incompatible with expected {PROTOCOL_VERSION}"
            ),
        });
    }
    Ok(())
}

fn resolve_worker_path() -> Result<PathBuf, CommandError> {
    let executable = current_exe().map_err(|_| CommandError::WorkerUnavailable {
        path: PathBuf::from(WORKER_NAME),
    })?;
    let Some(directory) = executable.parent() else {
        return Err(CommandError::WorkerUnavailable {
            path: PathBuf::from(WORKER_NAME),
        });
    };
    let sibling = directory.join(WORKER_NAME);
    if sibling.is_file() {
        return Ok(sibling);
    }
    if directory.file_name().is_some_and(|name| name == "deps") {
        if let Some(parent) = directory.parent() {
            let test_sibling = parent.join(WORKER_NAME);
            if test_sibling.is_file() {
                return Ok(test_sibling);
            }
        }
    }
    Err(CommandError::WorkerUnavailable { path: sibling })
}
