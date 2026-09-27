use dev_env_model::EffectiveIdentity;
use std::collections::HashSet;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct CommandRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub environment: std::collections::BTreeMap<String, String>,
    pub timeout: Option<Duration>,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
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
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
        }
    }
}

impl std::error::Error for CommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source }
            | Self::Wait { source }
            | Self::Kill { source }
            | Self::Read { source, .. } => Some(source),
            Self::InvalidProgram
            | Self::InvalidArgument
            | Self::InvalidEnvironment
            | Self::InvalidTimeout
            | Self::ReaderPanicked { .. } => None,
        }
    }
}

/// The output is raw bytes on purpose: a failed provider must not make the
/// runtime lose the exact diagnostic payload by prematurely converting it to
/// a String, and shellenv parsing can then enforce UTF-8 explicitly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub status: Option<i32>,
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandOutput {
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

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessExecutor;

impl CommandExecutor for ProcessExecutor {
    fn execute(&self, request: &CommandRequest) -> Result<CommandOutput, CommandError> {
        self.execute_as(request, None)
    }

    fn execute_with_identity(
        &self,
        request: &CommandRequest,
        identity: Option<&EffectiveIdentity>,
    ) -> Result<CommandOutput, CommandError> {
        self.execute_as(request, identity)
    }
}

impl ProcessExecutor {
    fn execute_as(
        &self,
        request: &CommandRequest,
        identity: Option<&EffectiveIdentity>,
    ) -> Result<CommandOutput, CommandError> {
        // waitpid is process-wide.  Keeping the wait/reap boundary serialized
        // prevents concurrent provider workers from stealing each other's
        // direct child status.
        let _execution_guard = process_execution_lock()
            .lock()
            .expect("provider execution lock poisoned");
        request.validate()?;
        install_subreaper();
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
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command, identity: Option<&EffectiveIdentity>) {
    use std::os::unix::process::CommandExt;
    let identity = identity.cloned();
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            if let Some(identity) = &identity {
                if identity.uid != 0 && libc::geteuid() == 0 {
                    let groups = identity
                        .supplementary_groups
                        .iter()
                        .map(|group| *group as libc::gid_t)
                        .collect::<Vec<_>>();
                    if libc::setgroups(groups.len(), groups.as_ptr()) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::setgid(identity.gid) != 0 || libc::setuid(identity.uid) != 0 {
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
    let process_group = -(pid as libc::pid_t);
    unsafe {
        let _ = libc::kill(process_group, libc::SIGTERM);
    }
    thread::sleep(Duration::from_millis(5));
    unsafe {
        let _ = libc::kill(process_group, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_pid: u32) {}

#[cfg(unix)]
fn reap_adopted_children() {
    loop {
        let result = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
        if result <= 0 {
            break;
        }
    }
}

#[cfg(not(unix))]
fn reap_adopted_children() {}

#[cfg(target_os = "linux")]
fn process_ids() -> HashSet<u32> {
    std::fs::read_dir("/proc")
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
fn adopted_children(baseline: &HashSet<u32>) -> Vec<libc::pid_t> {
    let current = std::process::id();
    process_ids()
        .difference(baseline)
        .filter_map(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let parenthesized = stat.rfind(')')?;
            let mut fields = stat[parenthesized + 1..].split_whitespace();
            let _state = fields.next()?;
            let parent = fields.next()?.parse::<u32>().ok()?;
            (parent == current).then_some(*pid as libc::pid_t)
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
                let _ = libc::kill(*pid, libc::SIGTERM);
            }
        }
        thread::sleep(Duration::from_millis(5));
        for pid in adopted_children(baseline) {
            unsafe {
                let _ = libc::kill(pid, libc::SIGKILL);
            }
        }
        reap_adopted_children();
    }
}

#[cfg(not(target_os = "linux"))]
fn terminate_new_adopted_children(_baseline: &HashSet<u32>) {}

static SUBREAPER_ONCE: Once = Once::new();

struct PidFd(Option<libc::c_int>);

impl PidFd {
    fn open(pid: u32) -> Self {
        #[cfg(target_os = "linux")]
        {
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_uint, 0) };
            if fd >= 0 {
                return Self(Some(fd as libc::c_int));
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
                let _ = libc::close(fd);
            }
        }
    }
}

fn process_execution_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn install_subreaper() {
    #[cfg(target_os = "linux")]
    SUBREAPER_ONCE.call_once(|| unsafe {
        // Provider descendants are reparented to the backend instead of PID 1.
        let _ = libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    });
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
fn set_nonblocking<P: std::os::fd::AsRawFd>(pipe: &P) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
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
