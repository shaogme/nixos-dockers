use dev_env_model::EffectiveIdentity;
use libc::c_int;
use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fmt::{Display, Formatter, Result as FmtResult},
    fs::{read_dir, read_to_string},
    io::{self, Read},
    path::{Path, PathBuf},
    process::{id, Child, Command, Stdio},
    ptr::null_mut,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use libc::{c_uint, prctl, syscall, SYS_pidfd_open, PR_SET_CHILD_SUBREAPER};

#[cfg(unix)]
use libc::{
    close, fcntl, geteuid, gid_t, kill, pid_t, setgid, setgroups, setpgid, setuid, waitpid,
    F_GETFL, F_SETFL, O_NONBLOCK, SIGKILL, SIGTERM, WNOHANG,
};

#[cfg(unix)]
use std::os::{fd::AsRawFd, unix::process::CommandExt};

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
            environment: Default::default(),
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
            | Self::Kill { source }
            | Self::Read { source, .. } => Some(source),
            Self::InvalidProgram
            | Self::InvalidArgument
            | Self::InvalidEnvironment
            | Self::InvalidTimeout
            | Self::ReaderPanicked { .. }
            | Self::SupervisorStopped => None,
        }
    }
}

/// The output is raw bytes on purpose: a failed provider must not make the
/// runtime lose the exact diagnostic payload by prematurely converting it to
/// a String, and shellenv parsing can then enforce UTF-8 explicitly.
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
        Self {
            status: Some(0),
            timed_out: false,
            stdout: stdout.into(),
            stderr: Vec::new(),
        }
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
        request: CommandRequest,
        identity: Option<EffectiveIdentity>,
        reply: SyncSender<Result<CommandOutput, CommandError>>,
    },
}

#[derive(Clone)]
pub struct ProcessExecutor {
    sender: Sender<SupervisorMessage>,
}

pub struct ProviderProcessSupervisor {
    executor: ProcessExecutor,
}

impl ProviderProcessSupervisor {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("dev-env-provider-supervisor".to_owned())
            .spawn(move || run_supervisor(receiver))
            .expect("could not spawn provider process supervisor");
        Self {
            executor: ProcessExecutor { sender },
        }
    }

    pub fn executor(&self) -> ProcessExecutor {
        self.executor.clone()
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
        let (reply, result) = mpsc::sync_channel(1);
        self.sender
            .send(SupervisorMessage::Execute {
                request: request.clone(),
                identity: identity.cloned(),
                reply,
            })
            .map_err(|_| CommandError::SupervisorStopped)?;
        result.recv().map_err(|_| CommandError::SupervisorStopped)?
    }
}

fn run_supervisor(receiver: Receiver<SupervisorMessage>) {
    install_subreaper();
    while let Ok(message) = receiver.recv() {
        match message {
            SupervisorMessage::Execute {
                request,
                identity,
                reply,
            } => {
                let result = execute_as(&request, identity.as_ref());
                let _ = reply.send(result);
            }
        }
    }
}

fn execute_as(
    request: &CommandRequest,
    identity: Option<&EffectiveIdentity>,
) -> Result<CommandOutput, CommandError> {
    request.validate()?;
    let mut command = Command::new(&request.program);
    command
        // The materializer supplies the environment explicitly.  Do not
        // leak the launcher process environment (tokens, host paths, or
        // unrelated runtime inputs) into a provider child.
        .env_clear()
        .args(&request.args)
        .current_dir(&request.cwd)
        .envs(&request.environment)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command, identity);
    let baseline_processes = process_ids();
    let mut child = command
        .spawn()
        .map_err(|source| CommandError::Spawn { source })?;
    let _pidfd = PidFd::open(child.id());

    let child_done = Arc::new(AtomicBool::new(false));
    let stdout = take_pipe(&mut child, OutputStream::Stdout, Arc::clone(&child_done))?;
    let stderr = take_pipe(&mut child, OutputStream::Stderr, Arc::clone(&child_done))?;
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child
            .try_wait()
            .map_err(|source| CommandError::Wait { source })?
        {
            Some(status) => break status.code(),
            None if request
                .timeout
                .is_some_and(|timeout| started.elapsed() >= timeout) =>
            {
                timed_out = true;
                terminate_process_group(child.id());
                if let Err(source) = child.kill() {
                    if source.kind() != io::ErrorKind::NotFound {
                        return Err(CommandError::Kill { source });
                    }
                }
                break child
                    .wait()
                    .map_err(|source| CommandError::Wait { source })?
                    .code();
            }
            None => thread::sleep(Duration::from_millis(5)),
        }
    };

    // A provider operation is one-shot.  Clean up anything left in its
    // process group before joining pipe readers, otherwise a detached
    // child can keep the pipe open and block the backend forever.
    child_done.store(true, Ordering::Release);
    terminate_process_group(child.id());
    terminate_new_adopted_children(&baseline_processes);
    for _ in 0..10 {
        reap_adopted_children();
        thread::sleep(Duration::from_millis(2));
    }

    Ok(CommandOutput {
        status,
        timed_out,
        stdout: join_pipe(stdout, OutputStream::Stdout, request.max_stdout_bytes)?,
        stderr: join_pipe(stderr, OutputStream::Stderr, request.max_stderr_bytes)?,
    })
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command, identity: Option<&EffectiveIdentity>) {
    let identity = identity.cloned();
    unsafe {
        command.pre_exec(move || {
            if setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            if let Some(identity) = &identity {
                if identity.uid() != 0 && geteuid() == 0 {
                    let groups = identity
                        .supplementary_groups()
                        .iter()
                        .map(|group| *group as gid_t)
                        .collect::<Vec<_>>();
                    if setgroups(groups.len(), groups.as_ptr()) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if setgid(identity.gid()) != 0 || setuid(identity.uid()) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command, _identity: Option<&EffectiveIdentity>) {}

#[cfg(unix)]
fn terminate_process_group(pid: u32) {
    let process_group = -(pid as pid_t);
    unsafe {
        let _ = kill(process_group, SIGTERM);
    }
    thread::sleep(Duration::from_millis(5));
    unsafe {
        let _ = kill(process_group, SIGKILL);
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_pid: u32) {}

#[cfg(unix)]
fn reap_adopted_children() {
    loop {
        let result = unsafe { waitpid(-1, null_mut(), WNOHANG) };
        if result <= 0 {
            break;
        }
    }
}

#[cfg(not(unix))]
fn reap_adopted_children() {}

#[cfg(target_os = "linux")]
fn process_ids() -> HashSet<u32> {
    read_dir("/proc")
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn process_ids() -> HashSet<u32> {
    HashSet::new()
}

#[cfg(target_os = "linux")]
fn adopted_children(baseline: &HashSet<u32>) -> Vec<pid_t> {
    let current = id();
    process_ids()
        .difference(baseline)
        .filter_map(|pid| {
            let stat = read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let parenthesized = stat.rfind(')')?;
            let mut fields = stat[parenthesized + 1..].split_whitespace();
            let _state = fields.next()?;
            let parent = fields.next()?.parse::<u32>().ok()?;
            (parent == current).then_some(*pid as pid_t)
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn terminate_new_adopted_children(baseline: &HashSet<u32>) {
    for _ in 0..20 {
        let children = adopted_children(baseline);
        if children.is_empty() {
            break;
        }
        for pid in &children {
            unsafe {
                let _ = kill(*pid, SIGTERM);
            }
        }
        thread::sleep(Duration::from_millis(5));
        for pid in adopted_children(baseline) {
            unsafe {
                let _ = kill(pid, SIGKILL);
            }
        }
        reap_adopted_children();
    }
}

#[cfg(not(target_os = "linux"))]
fn terminate_new_adopted_children(_baseline: &HashSet<u32>) {}

struct PidFd(Option<c_int>);

impl PidFd {
    fn open(pid: u32) -> Self {
        #[cfg(target_os = "linux")]
        {
            let fd = unsafe { syscall(SYS_pidfd_open, pid as c_uint, 0) };
            if fd >= 0 {
                return Self(Some(fd as c_int));
            }
        }
        Self(None)
    }
}

impl Drop for PidFd {
    fn drop(&mut self) {
        if let Some(fd) = self.0.take() {
            #[cfg(unix)]
            unsafe {
                let _ = close(fd);
            }
        }
    }
}

fn install_subreaper() {
    #[cfg(target_os = "linux")]
    unsafe {
        // Provider descendants are reparented to the backend instead of PID 1.
        let _ = prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    }
}

fn take_pipe(
    child: &mut Child,
    stream: OutputStream,
    child_done: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<io::Result<Vec<u8>>>, CommandError> {
    let mut pipe: Box<dyn Read + Send> = match stream {
        OutputStream::Stdout => {
            let pipe = child
                .stdout
                .take()
                .ok_or(CommandError::ReaderPanicked { stream })?;
            set_nonblocking(&pipe).map_err(|source| CommandError::Read { stream, source })?;
            Box::new(pipe)
        }
        OutputStream::Stderr => {
            let pipe = child
                .stderr
                .take()
                .ok_or(CommandError::ReaderPanicked { stream })?;
            set_nonblocking(&pipe).map_err(|source| CommandError::Read { stream, source })?;
            Box::new(pipe)
        }
    };
    Ok(thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 8192];
        let mut idle_since = Instant::now();
        loop {
            match (&mut pipe as &mut dyn Read).read(&mut buffer) {
                Ok(0) => break,
                Ok(length) => {
                    let remaining = 16 * 1024 * 1024usize - bytes.len().min(16 * 1024 * 1024);
                    bytes.extend_from_slice(&buffer[..length.min(remaining)]);
                    idle_since = Instant::now();
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if child_done.load(Ordering::Acquire)
                        && idle_since.elapsed() >= Duration::from_millis(100)
                    {
                        break;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(bytes)
    }))
}

#[cfg(unix)]
fn set_nonblocking<P: AsRawFd>(pipe: &P) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { fcntl(fd, F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_nonblocking<P>(_pipe: &P) -> io::Result<()> {
    Ok(())
}

fn join_pipe(
    handle: thread::JoinHandle<io::Result<Vec<u8>>>,
    stream: OutputStream,
    limit: usize,
) -> Result<Vec<u8>, CommandError> {
    let bytes = handle
        .join()
        .map_err(|_| CommandError::ReaderPanicked { stream })?
        .map_err(|source| CommandError::Read { stream, source })?;
    Ok(bytes.into_iter().take(limit).collect())
}
