use libc::{
    c_int, close, fcntl, geteuid, getpid, getppid, gid_t, kill, pid_t, prctl, pthread_sigmask,
    setgid, setgroups, setpgid, setuid, sigaction, sigaddset, sigemptyset, siginfo_t, sigset_t,
    syscall, waitpid, SYS_pidfd_open, SYS_pidfd_send_signal, ECHILD, EINTR, EPERM, ESRCH,
    FD_CLOEXEC, F_GETFD, F_GETFL, F_SETFD, F_SETFL, O_NONBLOCK, PR_SET_CHILD_SUBREAPER, SIGKILL,
    SIGTERM, SIG_UNBLOCK, WNOHANG,
};
use {
    crate::{
        command::{CommandOutput, OutputStream},
        worker_protocol::{
            WorkerIdentity, WorkerProtocol, WorkerRequest, PROTOCOL_VERSION, REQUEST_LIMIT,
        },
    },
    std::{
        env::args_os,
        ffi::OsStr,
        fs::read_to_string,
        io::{self, Read},
        mem::zeroed,
        net::Shutdown,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::net::UnixStream,
            unix::process::CommandExt,
        },
        process::{Child, Command, Stdio},
        ptr::{null, null_mut},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    },
};

const TERM_GRACE: Duration = Duration::from_millis(250);
const CLEANUP_DEADLINE: Duration = Duration::from_secs(2);
const PIPE_IDLE_GRACE: Duration = Duration::from_millis(100);
const PIPE_LIMIT: usize = 16 * 1024 * 1024;

static PARENT_SIGNALLED: AtomicBool = AtomicBool::new(false);

struct ProviderCredentials {
    uid: u32,
    gid: u32,
    supplementary_groups: Vec<u32>,
}

struct PipeReader {
    stream: OutputStream,
    handle: JoinHandle<io::Result<Vec<u8>>>,
}

struct ProcessResult {
    status: Option<i32>,
    timed_out: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

trait ReadPipe: Read + AsRawFd {}

impl<T> ReadPipe for T where T: Read + AsRawFd {}

pub struct ProviderWorker;

impl ProviderWorker {
    pub fn run() -> i32 {
        let mut arguments = args_os().skip(1);
        if arguments.next().as_deref() == Some(OsStr::new("--protocol-version"))
            && arguments.next().is_none()
        {
            println!("{PROTOCOL_VERSION}");
            return 0;
        }
        match run_worker() {
            Ok(()) => 0,
            Err(_) => 1,
        }
    }
}

fn run_worker() -> Result<(), String> {
    let (control_fd, expected_parent) = parse_arguments()?;
    if unsafe { getppid() } != expected_parent {
        return Err("provider worker parent changed during startup".to_owned());
    }
    set_control_close_on_exec(control_fd)?;
    install_parent_signal_receiver()?;
    let mut control = unsafe { UnixStream::from_raw_fd(control_fd) };
    let request_frame = WorkerProtocol::read_frame(&mut control, REQUEST_LIMIT)
        .map_err(|error| error.to_string())?;
    let request = match WorkerRequest::decode(&request_frame) {
        Ok(request) => request,
        Err(message) => {
            let _ = WorkerProtocol::write_frame(
                &mut control,
                &WorkerProtocol::encode_failure(&message),
            );
            return Ok(());
        }
    };
    let cancelled = Arc::new(AtomicBool::new(PARENT_SIGNALLED.load(Ordering::Acquire)));
    let cancel_reader = start_cancel_reader(&control, Arc::clone(&cancelled))?;
    let execution = if cancelled.load(Ordering::Acquire) || PARENT_SIGNALLED.load(Ordering::Acquire)
    {
        Err("provider worker was cancelled before the command started".to_owned())
    } else {
        execute_command(&request, Arc::clone(&cancelled))
    };
    let response = match execution {
        Ok(result) => WorkerProtocol::encode_output(&CommandOutput::new(
            result.status,
            result.timed_out,
            result.stdout,
            result.stderr,
        ))
        .map_err(|error| error.to_string())?,
        Err(message) => WorkerProtocol::encode_failure(&message),
    };
    WorkerProtocol::write_frame(&mut control, &response).map_err(|error| error.to_string())?;
    let _ = control.shutdown(Shutdown::Both);
    cancel_reader
        .join()
        .map_err(|_| "provider worker cancel reader panicked".to_owned())?;
    Ok(())
}

fn parse_arguments() -> Result<(i32, pid_t), String> {
    let mut arguments = args_os().skip(1);
    let mut control_fd = None;
    let mut parent_pid = None;
    while let Some(argument) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| "provider worker argument has no value".to_owned())?;
        match argument.to_str() {
            Some("--control-fd") => {
                control_fd = Some(
                    value
                        .to_str()
                        .ok_or_else(|| "provider worker control fd is invalid".to_owned())?
                        .parse::<i32>()
                        .map_err(|_| "provider worker control fd is invalid".to_owned())?,
                );
            }
            Some("--parent-pid") => {
                parent_pid = Some(
                    value
                        .to_str()
                        .ok_or_else(|| "provider worker parent pid is invalid".to_owned())?
                        .parse::<pid_t>()
                        .map_err(|_| "provider worker parent pid is invalid".to_owned())?,
                );
            }
            _ => return Err("provider worker received an unknown argument".to_owned()),
        }
    }
    let control_fd =
        control_fd.ok_or_else(|| "provider worker control fd is missing".to_owned())?;
    let parent_pid =
        parent_pid.ok_or_else(|| "provider worker parent pid is missing".to_owned())?;
    if control_fd < 0 || parent_pid <= 0 {
        return Err("provider worker startup arguments are out of range".to_owned());
    }
    Ok((control_fd, parent_pid))
}

fn set_control_close_on_exec(fd: i32) -> Result<(), String> {
    let flags = unsafe { fcntl(fd, F_GETFD) };
    if flags == -1 || unsafe { fcntl(fd, F_SETFD, flags | FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error().to_string());
    }
    Ok(())
}

fn install_parent_signal_receiver() -> Result<(), String> {
    unsafe extern "C" fn record_parent_signal(_signal: c_int) {
        PARENT_SIGNALLED.store(true, Ordering::Release);
    }

    unsafe {
        let mut action = zeroed::<sigaction>();
        action.sa_sigaction = record_parent_signal as *const () as usize;
        action.sa_flags = 0;
        if sigemptyset(&mut action.sa_mask) == -1 || sigaction(SIGTERM, &action, null_mut()) == -1 {
            return Err(io::Error::last_os_error().to_string());
        }
        let mut signal_set = zeroed::<sigset_t>();
        if sigemptyset(&mut signal_set) == -1 || sigaddset(&mut signal_set, SIGTERM) == -1 {
            return Err(io::Error::last_os_error().to_string());
        }
        let mask_error = pthread_sigmask(SIG_UNBLOCK, &signal_set, null_mut());
        if mask_error != 0 {
            return Err(io::Error::from_raw_os_error(mask_error).to_string());
        }
    }
    Ok(())
}

fn start_cancel_reader(
    control: &UnixStream,
    cancelled: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, String> {
    let mut reader = control.try_clone().map_err(|error| error.to_string())?;
    thread::Builder::new()
        .name("dev-env-provider-cancel".to_owned())
        .spawn(
            move || match WorkerProtocol::read_frame(&mut reader, REQUEST_LIMIT) {
                Ok(frame) if WorkerProtocol::decode_cancel(&frame).is_ok() => {
                    cancelled.store(true, Ordering::Release);
                }
                _ => {
                    cancelled.store(true, Ordering::Release);
                }
            },
        )
        .map_err(|error| error.to_string())
}

fn execute_command(
    request: &WorkerRequest,
    cancelled: Arc<AtomicBool>,
) -> Result<ProcessResult, String> {
    set_subreaper()?;
    let mut command = Command::new(request.program());
    command
        .env_clear()
        .args(request.args())
        .current_dir(request.cwd())
        .envs(request.environment())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_provider_process(&mut command, request.identity());
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not spawn provider: {error}"))?;
    let process_group = child.id() as pid_t;
    let finished_reading = Arc::new(AtomicBool::new(false));
    let stdout = start_pipe_reader(
        &mut child,
        OutputStream::Stdout,
        Arc::clone(&finished_reading),
    )?;
    let stderr = start_pipe_reader(
        &mut child,
        OutputStream::Stderr,
        Arc::clone(&finished_reading),
    )?;
    let started = Instant::now();
    let mut timed_out = false;
    let mut stop_requested = false;
    let mut status = None;
    let mut child_exited = false;
    loop {
        if let Some(exit_status) = child
            .try_wait()
            .map_err(|error| format!("could not wait for provider: {error}"))?
        {
            child_exited = true;
            status = exit_status.code();
            break;
        }
        if request
            .timeout()
            .is_some_and(|timeout| started.elapsed() >= timeout)
        {
            timed_out = true;
            stop_requested = true;
            break;
        }
        if cancelled.load(Ordering::Acquire) || PARENT_SIGNALLED.load(Ordering::Acquire) {
            stop_requested = true;
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    if stop_requested {
        let provider_pid = child.id() as pid_t;
        status = terminate_process_tree(
            &mut child,
            process_group,
            provider_pid,
            status,
            child_exited,
        )?;
    } else {
        cleanup_process_tree(process_group)?;
    }
    finished_reading.store(true, Ordering::Release);
    let stdout = finish_pipe_reader(stdout, request.max_stdout_bytes())?;
    let stderr = finish_pipe_reader(stderr, request.max_stderr_bytes())?;
    Ok(ProcessResult {
        status,
        timed_out,
        stdout,
        stderr,
    })
}

#[cfg(unix)]
fn configure_provider_process(command: &mut Command, identity: Option<&WorkerIdentity>) {
    let identity = identity.map(|identity| ProviderCredentials {
        uid: identity.uid,
        gid: identity.gid,
        supplementary_groups: identity.supplementary_groups.clone(),
    });
    unsafe {
        command.pre_exec(move || {
            if setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            if let Some(credentials) = &identity {
                if credentials.uid != 0 && geteuid() == 0 {
                    let groups = credentials
                        .supplementary_groups
                        .iter()
                        .map(|group| *group as gid_t)
                        .collect::<Vec<_>>();
                    if setgroups(groups.len(), groups.as_ptr()) == -1
                        || setgid(credentials.gid) == -1
                        || setuid(credentials.uid) == -1
                    {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
            Ok(())
        });
    }
}

fn set_subreaper() -> Result<(), String> {
    if unsafe { prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } == -1 {
        return Err(format!(
            "could not set provider worker as a child subreaper: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn terminate_process_tree(
    child: &mut Child,
    process_group: pid_t,
    provider_pid: pid_t,
    mut status: Option<i32>,
    mut child_exited: bool,
) -> Result<Option<i32>, String> {
    signal_process_group(process_group, SIGTERM);
    let grace_deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < grace_deadline {
        if !child_exited {
            if let Some(exit_status) = child
                .try_wait()
                .map_err(|error| format!("could not wait for provider: {error}"))?
            {
                child_exited = true;
                status = exit_status.code();
            }
        }
        signal_adopted_children(SIGTERM)?;
        reap_children((!child_exited).then_some(provider_pid))?;
        if child_exited && !process_group_exists(process_group) && adopted_children()?.is_empty() {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(5));
    }
    signal_process_group(process_group, SIGKILL);
    let deadline = Instant::now() + CLEANUP_DEADLINE;
    loop {
        if !child_exited {
            if let Some(exit_status) = child
                .try_wait()
                .map_err(|error| format!("could not wait for provider: {error}"))?
            {
                child_exited = true;
                status = exit_status.code();
            }
        }
        signal_adopted_children(SIGKILL)?;
        reap_children((!child_exited).then_some(provider_pid))?;
        if child_exited && !process_group_exists(process_group) && adopted_children()?.is_empty() {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            if !child_exited {
                child
                    .wait()
                    .map_err(|error| format!("could not wait for provider: {error}"))?;
            }
            return Err("provider descendants did not exit before cleanup deadline".to_owned());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn cleanup_process_tree(process_group: pid_t) -> Result<(), String> {
    signal_process_group(process_group, SIGTERM);
    let grace_deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < grace_deadline {
        signal_adopted_children(SIGTERM)?;
        reap_children(None)?;
        if !process_group_exists(process_group) && adopted_children()?.is_empty() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(5));
    }
    signal_process_group(process_group, SIGKILL);
    let deadline = Instant::now() + CLEANUP_DEADLINE;
    loop {
        signal_adopted_children(SIGKILL)?;
        reap_children(None)?;
        if !process_group_exists(process_group) && adopted_children()?.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("provider descendants did not exit before cleanup deadline".to_owned());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn signal_process_group(process_group: pid_t, signal: i32) {
    unsafe {
        let _ = kill(-process_group, signal);
    }
}

fn process_group_exists(process_group: pid_t) -> bool {
    if unsafe { kill(-process_group, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(EPERM)
}

fn adopted_children() -> Result<Vec<pid_t>, String> {
    let process = unsafe { getpid() };
    let children = read_to_string(format!("/proc/{process}/task/{process}/children"))
        .map_err(|error| format!("could not inspect worker children: {error}"))?;
    children
        .split_whitespace()
        .map(|pid| {
            pid.parse::<pid_t>()
                .map_err(|_| "worker child list contains an invalid PID".to_owned())
        })
        .collect()
}

fn signal_adopted_children(signal: i32) -> Result<(), String> {
    for pid in adopted_children()? {
        let pidfd = unsafe { syscall(SYS_pidfd_open, pid, 0) } as c_int;
        if pidfd == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ESRCH) {
                continue;
            }
            return Err(format!(
                "could not open a pidfd for provider child {pid}: {error}"
            ));
        }
        let signal_result =
            unsafe { syscall(SYS_pidfd_send_signal, pidfd, signal, null::<siginfo_t>(), 0) };
        let signal_error = (signal_result == -1).then(io::Error::last_os_error);
        unsafe {
            close(pidfd);
        }
        if let Some(error) = signal_error {
            if error.raw_os_error() != Some(ESRCH) {
                return Err(format!("could not signal provider child {pid}: {error}"));
            }
        }
    }
    Ok(())
}

fn reap_children(protected_pid: Option<pid_t>) -> Result<(), String> {
    if let Some(protected_pid) = protected_pid {
        for pid in adopted_children()? {
            if pid != protected_pid {
                reap_child(pid)?;
            }
        }
        return Ok(());
    }
    loop {
        let result = unsafe { waitpid(-1, null_mut(), WNOHANG) };
        if result > 0 {
            continue;
        }
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(EINTR) {
                continue;
            }
            if error.raw_os_error() == Some(ECHILD) {
                return Ok(());
            }
            return Err(format!("could not reap provider child: {error}"));
        }
        return Ok(());
    }
}

fn reap_child(pid: pid_t) -> Result<(), String> {
    loop {
        let result = unsafe { waitpid(pid, null_mut(), WNOHANG) };
        if result != -1 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(EINTR) {
            continue;
        }
        if error.raw_os_error() == Some(ECHILD) {
            return Ok(());
        }
        return Err(format!("could not reap provider descendant {pid}: {error}"));
    }
}

fn start_pipe_reader(
    child: &mut Child,
    stream: OutputStream,
    finished: Arc<AtomicBool>,
) -> Result<PipeReader, String> {
    let mut pipe: Box<dyn ReadPipe + Send> = match stream {
        OutputStream::Stdout => Box::new(child.stdout.take().ok_or("provider stdout is missing")?),
        OutputStream::Stderr => Box::new(child.stderr.take().ok_or("provider stderr is missing")?),
    };
    set_nonblocking(pipe.as_ref()).map_err(|error| error.to_string())?;
    let handle = thread::Builder::new()
        .name(format!("dev-env-provider-{stream:?}"))
        .spawn(move || {
            let mut output = Vec::new();
            let mut buffer = [0_u8; 8192];
            let mut last_data = Instant::now();
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(length) => {
                        let remaining = PIPE_LIMIT.saturating_sub(output.len());
                        output.extend_from_slice(&buffer[..length.min(remaining)]);
                        last_data = Instant::now();
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        if finished.load(Ordering::Acquire)
                            && last_data.elapsed() >= PIPE_IDLE_GRACE
                        {
                            break;
                        }
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(output)
        })
        .map_err(|error| error.to_string())?;
    Ok(PipeReader { stream, handle })
}

fn set_nonblocking<P: AsRawFd + ?Sized>(pipe: &P) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { fcntl(fd, F_GETFL) };
    if flags == -1 || unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn finish_pipe_reader(reader: PipeReader, limit: usize) -> Result<Vec<u8>, String> {
    let output = reader
        .handle
        .join()
        .map_err(|_| format!("provider {:?} reader thread panicked", reader.stream))?
        .map_err(|error| format!("could not read provider {:?}: {error}", reader.stream))?;
    Ok(output.into_iter().take(limit).collect())
}
