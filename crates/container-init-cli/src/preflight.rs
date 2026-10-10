use container_init_backend::PreparedHandoff;
use container_init_core::{HandoffCommand, ResolvedIdentity};
use container_init_protocol::{
    CwdObject, MAX_ARGV_ITEMS, MAX_ARGV_ITEM_BYTES, MAX_ARGV_TOTAL_BYTES,
};
use libc::{
    c_int, c_short, chdir, dup2, fcntl, getegid, geteuid, getgroups, gid_t, kill, pid_t, poll,
    pollfd, setgid, setgroups, setuid, sigaction, sigemptyset, EBADF, FD_CLOEXEC, F_GETFD, F_SETFD,
    POLLERR, POLLHUP, POLLIN, POLLNVAL, SIGHUP, SIGINT, SIGQUIT, SIGTERM,
};
use serde::{Deserialize, Serialize};
use serde_json::{from_slice, to_vec};
use std::{
    collections::BTreeMap,
    env,
    ffi::CString,
    fs::{self, Metadata},
    io::{self, Read, Write},
    mem,
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt, net::UnixStream, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    ptr,
    sync::atomic::{AtomicI32, Ordering},
    thread,
    time::{Duration, Instant},
};

const CONTROL_FD: RawFd = 3;
const HELPER_ARG: &str = "__container_init_handoff_helper";
const MAX_HANDOFF_SPEC_BYTES: usize = 16 * 1024 * 1024;

static FORWARDED_SIGNAL: AtomicI32 = AtomicI32::new(0);
static HELPER_PID: AtomicI32 = AtomicI32::new(0);

#[derive(Clone, Debug, Deserialize, Serialize)]
struct HandoffSpec {
    command: HandoffCommand,
    identity: ResolvedIdentity,
    supplemental_groups: Vec<u32>,
    root_service: bool,
    original_cwd: PathBuf,
    cwd_object: CwdObject,
    login_environment: BTreeMap<String, String>,
    environment: BTreeMap<String, String>,
}

pub struct PreflightChild {
    child: Child,
    control: UnixStream,
}

impl PreflightChild {
    pub fn spawn(prepared: &PreparedHandoff, original_cwd: PathBuf) -> io::Result<Self> {
        let (mut parent_control, child_control) = UnixStream::pair()?;
        parent_control.set_nonblocking(false)?;
        let spec = HandoffSpec {
            command: prepared.command.clone(),
            identity: prepared.identity.clone(),
            supplemental_groups: prepared.supplemental_groups.clone(),
            root_service: prepared.root_service,
            original_cwd,
            cwd_object: prepared.cwd_object,
            login_environment: prepared.login_environment.clone(),
            environment: env::vars().collect(),
        };
        let helper_cwd = spec.original_cwd.clone();
        let spec =
            to_vec(&spec).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if spec.is_empty() || spec.len() > MAX_HANDOFF_SPEC_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "handoff spec is {} bytes; maximum is {MAX_HANDOFF_SPEC_BYTES} bytes",
                    spec.len()
                ),
            ));
        }
        let spec_length = u32::try_from(spec.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "handoff spec exceeds u32"))?;
        let control_fd = child_control.as_raw_fd();
        let control_path_fd = control_fd;
        let cwd = CString::new(helper_cwd.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cwd contains NUL"))?;
        let groups = prepared
            .supplemental_groups
            .iter()
            .map(|group| *group as gid_t)
            .collect::<Vec<_>>();
        if groups.len() > container_init_protocol::max_supplementary_groups() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "handoff supplementary groups exceed the platform limit",
            ));
        }
        let uid = prepared.identity.uid;
        let gid = prepared.identity.gid;
        let root_service = prepared.root_service;
        let current_uid = unsafe { geteuid() };
        if current_uid != 0 && !root_service && (uid != current_uid || gid != unsafe { getegid() })
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "non-root handoff credentials do not match the current process",
            ));
        }
        install_signal_forwarder();
        let executable = env::current_exe()?;
        let mut command = Command::new(executable);
        command
            .arg(HELPER_ARG)
            .env_clear()
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        unsafe {
            command.pre_exec(move || {
                if control_path_fd != CONTROL_FD && dup2(control_path_fd, CONTROL_FD) < 0 {
                    return Err(io::Error::last_os_error());
                }
                let descriptor_flags = fcntl(CONTROL_FD, F_GETFD);
                if descriptor_flags < 0
                    || fcntl(CONTROL_FD, F_SETFD, descriptor_flags & !FD_CLOEXEC) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                if !root_service && current_uid == 0 {
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
                if chdir(cwd.as_ptr()) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        drop(child_control);
        if let Err(error) = parent_control
            .write_all(&spec_length.to_be_bytes())
            .and_then(|_| parent_control.write_all(&spec))
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        HELPER_PID.store(child.id() as i32, Ordering::SeqCst);
        Ok(Self {
            child,
            control: parent_control,
        })
    }

    pub fn wait_ready(&mut self, deadline: Instant) -> io::Result<()> {
        wait_control_message(&mut self.control, deadline).map(|message| match message {
            ControlMessage::Ready => Ok(()),
            ControlMessage::Error(message) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("preflight failed: {message}"),
            )),
            ControlMessage::Closed => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "preflight helper exited before reporting readiness",
            )),
        })?
    }

    pub fn release(&mut self) -> io::Result<()> {
        self.control.write_all(b"C")
    }

    pub fn abort(&mut self) {
        HELPER_PID.store(0, Ordering::SeqCst);
        let _ = self.control.write_all(b"A");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn wait_for_exec(&mut self, deadline: Instant) -> io::Result<()> {
        match wait_control_message(&mut self.control, deadline)? {
            ControlMessage::Ready => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "preflight helper sent an unexpected readiness message",
            )),
            ControlMessage::Error(message) => {
                Err(io::Error::other(format!("handoff exec failed: {message}")))
            }
            ControlMessage::Closed => Ok(()),
        }
    }

    pub fn wait_for_runtime(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                HELPER_PID.store(0, Ordering::SeqCst);
                return Ok(status);
            }
            let signal = FORWARDED_SIGNAL.swap(0, Ordering::SeqCst);
            if signal != 0 {
                unsafe {
                    kill(self.child.id() as pid_t, signal);
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

enum ControlMessage {
    Ready,
    Error(String),
    Closed,
}

fn wait_control_message(stream: &mut UnixStream, deadline: Instant) -> io::Result<ControlMessage> {
    poll_until(stream.as_raw_fd(), POLLIN, deadline)?;
    let mut tag = [0u8; 1];
    match stream.read(&mut tag) {
        Ok(0) => return Ok(ControlMessage::Closed),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
            return wait_control_message(stream, deadline)
        }
        Err(error) => return Err(error),
    }
    match tag[0] {
        b'R' => Ok(ControlMessage::Ready),
        b'E' => {
            let mut length = [0u8; 2];
            stream.read_exact(&mut length)?;
            let mut message = vec![0; u16::from_be_bytes(length) as usize];
            stream.read_exact(&mut message)?;
            Ok(ControlMessage::Error(
                String::from_utf8_lossy(&message).into_owned(),
            ))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "preflight helper sent an invalid control message",
        )),
    }
}

fn poll_until(fd: RawFd, events: c_short, deadline: Instant) -> io::Result<()> {
    loop {
        if FORWARDED_SIGNAL.load(Ordering::SeqCst) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "handoff was interrupted",
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "request deadline expired",
            ));
        }
        let millis = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut descriptor = pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe { poll(&mut descriptor, 1, millis) };
        if result > 0 {
            if descriptor.revents & POLLNVAL != 0 {
                return Err(io::Error::from_raw_os_error(EBADF));
            }
            if descriptor.revents & (events | POLLHUP | POLLERR) != 0 {
                return Ok(());
            }
        } else if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "request deadline expired",
            ));
        } else {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

pub fn run_helper() -> i32 {
    match run_helper_inner() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("container-init: handoff helper failed: {error}");
            126
        }
    }
}

fn run_helper_inner() -> io::Result<i32> {
    let mut control = unsafe { UnixStream::from_raw_fd(CONTROL_FD) };
    let spec_payload = read_spec_frame(&mut control)?;
    let spec: HandoffSpec = from_slice(&spec_payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if let Err(error) = verify_child_state(&spec).and_then(|()| verify_exec_budget(&spec)) {
        send_control_error(&mut control, &error.to_string());
        return Ok(126);
    }
    control.write_all(b"R")?;
    let mut release = [0u8; 1];
    control.read_exact(&mut release)?;
    if release[0] != b'C' {
        return Ok(125);
    }
    set_cloexec(CONTROL_FD)?;
    let mut handoff = Command::new(&spec.command.program);
    handoff.env_clear();
    handoff.args(&spec.command.args);
    for (name, value) in &spec.environment {
        handoff.env(name, value);
    }
    for (name, value) in &spec.login_environment {
        handoff.env(name, value);
    }
    let source = handoff.exec();
    send_control_error(
        &mut control,
        &format!("{}: {source}", spec.command.program.display()),
    );
    Ok(127)
}

fn read_spec_frame(control: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut header = [0_u8; 4];
    control.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_HANDOFF_SPEC_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "handoff spec frame length {length} exceeds the {MAX_HANDOFF_SPEC_BYTES} byte limit"
            ),
        ));
    }
    let mut payload = vec![0_u8; length];
    control.read_exact(&mut payload)?;
    Ok(payload)
}

fn verify_exec_budget(spec: &HandoffSpec) -> io::Result<()> {
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let argument_limit = unsafe { libc::sysconf(libc::_SC_ARG_MAX) };
    if page_size <= 0 || argument_limit <= 0 {
        return Err(exec_budget_error("system argument limits are unavailable"));
    }
    let page_size = page_size as usize;
    let argument_limit = argument_limit as usize;
    let single_argument_limit = page_size.saturating_mul(32);
    let mut argument_bytes = 0_usize;
    let mut argument_count = 1_usize;
    let program = spec.command.program.as_os_str().as_bytes();
    if program.len() > MAX_ARGV_ITEM_BYTES {
        return Err(exec_budget_error(format!(
            "argv[0] is {} bytes; protocol maximum is {MAX_ARGV_ITEM_BYTES} bytes",
            program.len()
        )));
    }
    check_exec_string(
        "argv[0]",
        program,
        single_argument_limit,
        &mut argument_bytes,
    )?;
    for (index, argument) in spec.command.args.iter().enumerate() {
        argument_count += 1;
        check_exec_string(
            &format!("argv[{}]", index + 1),
            argument.as_bytes(),
            single_argument_limit,
            &mut argument_bytes,
        )?;
    }

    if argument_count > MAX_ARGV_ITEMS
        || spec
            .command
            .args
            .iter()
            .any(|argument| argument.len() > MAX_ARGV_ITEM_BYTES)
    {
        return Err(exec_budget_error(
            "final handoff argv exceeds the protocol argument limits",
        ));
    }
    let raw_argument_bytes =
        program.len() + spec.command.args.iter().map(String::len).sum::<usize>();
    if raw_argument_bytes > MAX_ARGV_TOTAL_BYTES {
        return Err(exec_budget_error(format!(
            "final handoff argv is {raw_argument_bytes} bytes; protocol maximum is {MAX_ARGV_TOTAL_BYTES} bytes"
        )));
    }

    let mut environment = spec.environment.clone();
    environment.extend(spec.login_environment.clone());
    let mut environment_bytes = 0_usize;
    for (name, value) in &environment {
        if name.is_empty() || name.contains('=') || name.contains('\0') || value.contains('\0') {
            return Err(exec_budget_error(
                "handoff environment contains an invalid name or NUL",
            ));
        }
        let length = name
            .len()
            .checked_add(1)
            .and_then(|bytes| bytes.checked_add(value.len()))
            .and_then(|bytes| bytes.checked_add(1))
            .ok_or_else(|| exec_budget_error("handoff environment size overflowed"))?;
        if length > single_argument_limit {
            return Err(exec_budget_error(format!(
                "environment.{name} is {length} bytes; operating system per-string limit is {single_argument_limit} bytes"
            )));
        }
        environment_bytes = environment_bytes
            .checked_add(length)
            .ok_or_else(|| exec_budget_error("handoff environment size overflowed"))?;
    }

    let pointer_bytes = argument_count
        .checked_add(environment.len())
        .and_then(|count| count.checked_add(2))
        .and_then(|count| count.checked_mul(std::mem::size_of::<*const libc::c_char>()))
        .ok_or_else(|| exec_budget_error("handoff pointer table size overflowed"))?;
    let total_bytes = argument_bytes
        .checked_add(environment_bytes)
        .and_then(|bytes| bytes.checked_add(pointer_bytes))
        .ok_or_else(|| exec_budget_error("handoff exec size overflowed"))?;
    let safety_margin = page_size.saturating_mul(4).max(8 * 1024);
    let usable_limit = argument_limit.saturating_sub(safety_margin);
    if total_bytes > usable_limit {
        return Err(exec_budget_error(format!(
            "argv and environment need {total_bytes} bytes including pointers; safe system limit is {usable_limit} bytes"
        )));
    }
    Ok(())
}

fn check_exec_string(field: &str, value: &[u8], limit: usize, total: &mut usize) -> io::Result<()> {
    if value.contains(&0) {
        return Err(exec_budget_error(format!("{field} contains NUL")));
    }
    let length = value
        .len()
        .checked_add(1)
        .ok_or_else(|| exec_budget_error("handoff argument size overflowed"))?;
    if length > limit {
        return Err(exec_budget_error(format!(
            "{field} is {} bytes; operating system per-string limit is {limit} bytes",
            value.len()
        )));
    }
    *total = total
        .checked_add(length)
        .ok_or_else(|| exec_budget_error("handoff argv size overflowed"))?;
    Ok(())
}

fn exec_budget_error(message: impl Into<String>) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("handoff_exec_budget_exceeded: {}", message.into()),
    )
}

fn verify_child_state(spec: &HandoffSpec) -> io::Result<()> {
    if spec.supplemental_groups.len() > container_init_protocol::max_supplementary_groups() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "handoff supplementary groups exceed the platform limit",
        ));
    }
    let uid = unsafe { geteuid() };
    let gid = unsafe { getegid() };
    if spec.root_service {
        if uid != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "root-service handoff is not running as root",
            ));
        }
    } else if uid != spec.identity.uid || gid != spec.identity.gid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "handoff credentials changed before preflight",
        ));
    }
    if !spec.root_service {
        let count = unsafe { getgroups(0, ptr::null_mut()) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count as usize > container_init_protocol::max_supplementary_groups() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "current supplementary groups exceed the platform limit",
            ));
        }
        let mut groups = vec![0 as gid_t; count as usize];
        let result = unsafe { getgroups(count, groups.as_mut_ptr()) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut groups = groups[..result as usize].to_vec();
        groups.push(gid);
        groups.sort_unstable();
        groups.dedup();
        if groups != spec.supplemental_groups {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "handoff supplementary groups do not match the prepared identity",
            ));
        }
    }
    let cwd = fs::metadata(".")?;
    if !cwd_object_matches(spec.cwd_object, &cwd) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "working directory changed between Prepare and Preflight",
        ));
    }
    Ok(())
}

fn cwd_object_matches(expected: CwdObject, actual: &Metadata) -> bool {
    actual.dev() == expected.device && actual.ino() == expected.inode
}

fn send_control_error(control: &mut UnixStream, message: &str) {
    let bytes = message.as_bytes();
    let length = bytes.len().min(u16::MAX as usize);
    let _ = control.write_all(b"E");
    let _ = control.write_all(&(length as u16).to_be_bytes());
    let _ = control.write_all(&bytes[..length]);
}

fn set_cloexec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { fcntl(fd, F_GETFD) };
    if flags < 0 || unsafe { fcntl(fd, F_SETFD, flags | FD_CLOEXEC) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

extern "C" fn signal_handler(signal: c_int) {
    FORWARDED_SIGNAL.store(signal, Ordering::Relaxed);
    let child = HELPER_PID.load(Ordering::Relaxed);
    if child > 0 {
        unsafe {
            kill(child, signal);
        }
    }
}

fn install_signal_forwarder() {
    FORWARDED_SIGNAL.store(0, Ordering::Relaxed);
    for signal in [SIGTERM, SIGINT, SIGHUP, SIGQUIT] {
        let mut action: sigaction = unsafe { mem::zeroed() };
        action.sa_sigaction = signal_handler as *const () as usize;
        action.sa_flags = 0;
        unsafe {
            sigemptyset(&mut action.sa_mask);
            sigaction(signal, &action, ptr::null_mut());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        check_exec_string, cwd_object_matches, read_spec_frame, signal_handler, verify_exec_budget,
        HandoffSpec, FORWARDED_SIGNAL, HELPER_PID,
    };
    use container_init_core::{HandoffCommand, IdentitySource, ResolvedIdentity, WorkspaceStatus};
    use container_init_protocol::CwdObject;
    use libc::SIGTERM;
    use std::{
        collections::BTreeMap,
        fs,
        io::Write,
        os::unix::net::UnixStream,
        os::unix::{fs::MetadataExt, process::ExitStatusExt},
        path::PathBuf,
        process::Command,
        sync::atomic::Ordering,
    };

    #[test]
    fn cwd_object_check_detects_path_replacement() {
        let temp = tempfile::TempDir::new().unwrap();
        let target = temp.path().join("cwd");
        let moved = temp.path().join("cwd-original");
        fs::create_dir(&target).unwrap();
        let original = fs::metadata(&target).unwrap();
        let expected = CwdObject {
            device: original.dev(),
            inode: original.ino(),
        };
        fs::rename(&target, &moved).unwrap();
        fs::create_dir(&target).unwrap();
        let replacement = fs::metadata(&target).unwrap();
        assert!(!cwd_object_matches(expected, &replacement));
        assert!(cwd_object_matches(expected, &fs::metadata(&moved).unwrap()));
    }

    #[test]
    fn signal_handler_forwards_signal_to_the_handoff_child() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        HELPER_PID.store(child.id() as i32, Ordering::SeqCst);
        signal_handler(SIGTERM);
        let status = child.wait().unwrap();
        HELPER_PID.store(0, Ordering::SeqCst);
        FORWARDED_SIGNAL.store(0, Ordering::SeqCst);
        assert_eq!(status.signal(), Some(SIGTERM));
    }

    #[test]
    fn control_socket_transfers_a_length_prefixed_spec() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let payload = br#"{"fixed":"spec"}"#;
        sender
            .write_all(&(payload.len() as u32).to_be_bytes())
            .unwrap();
        sender.write_all(payload).unwrap();
        assert_eq!(read_spec_frame(&mut receiver).unwrap(), payload);
    }

    #[test]
    fn exec_budget_accepts_37543_byte_arguments_and_rejects_os_total_overflow() {
        let regression = test_spec(
            vec!["-c".to_owned(), "true".to_owned(), "x".repeat(37_543)],
            BTreeMap::new(),
        );
        verify_exec_budget(&regression).unwrap();

        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let argument_limit = unsafe { libc::sysconf(libc::_SC_ARG_MAX) } as usize;
        let value = "x".repeat(page_size * 8);
        let entry_bytes = value.len() + 24;
        let entry_count = argument_limit / entry_bytes + 1;
        let environment = (0..entry_count)
            .map(|index| (format!("BUDGET_{index}"), value.clone()))
            .collect();
        let result = verify_exec_budget(&test_spec(vec!["true".to_owned()], environment));
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("handoff_exec_budget_exceeded"));
    }

    #[test]
    fn exec_string_check_counts_the_terminating_nul_byte() {
        let mut total = 0;
        check_exec_string("argv[0]", &"x".repeat(127).into_bytes(), 128, &mut total).unwrap();
        assert_eq!(total, 128);
        assert!(
            check_exec_string("argv[0]", &"x".repeat(128).into_bytes(), 128, &mut total).is_err()
        );
    }

    fn test_spec(args: Vec<String>, environment: BTreeMap<String, String>) -> HandoffSpec {
        HandoffSpec {
            command: HandoffCommand {
                program: PathBuf::from("/bin/sh"),
                args,
            },
            identity: ResolvedIdentity {
                uid: unsafe { libc::geteuid() },
                gid: unsafe { libc::getegid() },
                user: "test".to_owned(),
                home: PathBuf::from("/tmp"),
                run_as_root: false,
                uid_source: IdentitySource::Current,
                gid_source: IdentitySource::Current,
                workspace: WorkspaceStatus::Unavailable,
            },
            supplemental_groups: Vec::new(),
            root_service: false,
            original_cwd: PathBuf::from("/"),
            cwd_object: CwdObject {
                device: 0,
                inode: 0,
            },
            login_environment: BTreeMap::new(),
            environment,
        }
    }
}
