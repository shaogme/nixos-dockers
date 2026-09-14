use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
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
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessExecutor;

impl CommandExecutor for ProcessExecutor {
    fn execute(&self, request: &CommandRequest) -> Result<CommandOutput, CommandError> {
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
        let mut child = command
            .spawn()
            .map_err(|source| CommandError::Spawn { source })?;

        let stdout = take_pipe(&mut child, OutputStream::Stdout)?;
        let stderr = take_pipe(&mut child, OutputStream::Stderr)?;
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
                    child
                        .kill()
                        .map_err(|source| CommandError::Kill { source })?;
                    break child
                        .wait()
                        .map_err(|source| CommandError::Wait { source })?
                        .code();
                }
                None => thread::sleep(Duration::from_millis(5)),
            }
        };

        Ok(CommandOutput {
            status,
            timed_out,
            stdout: join_pipe(stdout, OutputStream::Stdout, request.max_stdout_bytes)?,
            stderr: join_pipe(stderr, OutputStream::Stderr, request.max_stderr_bytes)?,
        })
    }
}

fn take_pipe(
    child: &mut Child,
    stream: OutputStream,
) -> Result<thread::JoinHandle<io::Result<Vec<u8>>>, CommandError> {
    let pipe: Box<dyn Read + Send> = match stream {
        OutputStream::Stdout => Box::new(
            child
                .stdout
                .take()
                .ok_or(CommandError::ReaderPanicked { stream })?,
        ),
        OutputStream::Stderr => Box::new(
            child
                .stderr
                .take()
                .ok_or(CommandError::ReaderPanicked { stream })?,
        ),
    };
    Ok(thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.take(16 * 1024 * 1024).read_to_end(&mut bytes)?;
        Ok(bytes)
    }))
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
