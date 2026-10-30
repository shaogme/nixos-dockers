use {
    container_init_protocol::{
        read_message, write_message, AbstractEndpoint, BackendState, ClientMessage, ClientRequest,
        HelloInfo, IdentityRequest, PreparedIdentity, ProtocolLimits, ServerMessage,
        ServerResponse, PROTOCOL_VERSION,
    },
    dev_env_cli::BackendTransport,
    libc::{getegid, geteuid},
    serde_json::{from_slice, Value},
    std::{
        env::{temp_dir, var_os},
        fs,
        io::{ErrorKind, Write},
        path::{Path, PathBuf},
        process::{Child, Command, Output, Stdio},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    },
};

static BACKEND_STARTUP_TEST_LOCK: Mutex<()> = Mutex::new(());

struct Fixture {
    root: PathBuf,
    workspace: PathBuf,
    nested: PathBuf,
    profiles: PathBuf,
    default_profile: PathBuf,
    trust_file: PathBuf,
}

impl Fixture {
    fn real_bash() -> PathBuf {
        var_os("DEVENV_BOOTSTRAP_REAL_SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/bash"))
    }

    fn new() -> Self {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = temp_dir().join(format!("dev-env-startup-{}-{suffix}", std::process::id()));
        let workspace = root.join("workspace");
        let nested = workspace.join("nested");
        let profiles = root.join("profiles");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&profiles).unwrap();
        let default_profile = root.join("default-profile");
        fs::write(&default_profile, "startup\n").unwrap();
        let trust_file = root.join("trusted-hashes");
        Self {
            root,
            workspace,
            nested,
            profiles,
            default_profile,
            trust_file,
        }
    }

    fn write_profile(&self, provider: &Path, extra_variables: &str, timeout_ms: u64) {
        let real_bash = Self::real_bash();
        let profile = format!(
            r#"schema = 1
id = "startup"

[config.workspace]
root = "{}"
search = "fixed"

[config.shell]
default = "bash"

[config.shells.bash]
command = "{}"
kind = "posix"
interactive_args = ["-i"]
login_args = ["-l"]
command_arg = "-c"

[config.environment]
inherit_process = false

[config.environment.variables]
PATH = "/usr/bin:/bin"
PROVIDER_LOG = "{}"
{}

[config.providers.startup]
executable = "{}"
missing = "error"

[[config.providers.startup.prepare]]
argv = ["prepare"]
failure = "error"
timeout_ms = {}

[config.providers.startup.shellenv]
argv = ["env"]
format = "json"
failure = "error"
timeout_ms = 1000
"#,
            self.workspace.display(),
            real_bash.display(),
            self.root.join("provider.log").display(),
            extra_variables,
            provider.display(),
            timeout_ms,
        );
        fs::write(self.profiles.join("00-startup.toml"), profile).unwrap();
    }

    fn command(&self) -> Command {
        let runtime = self.root.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_dev-env"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.join("home"))
            .env("XDG_RUNTIME_DIR", runtime)
            .env("DEVENV_PROFILES_DIR", &self.profiles)
            .env("DEVENV_DEFAULT_PROFILE_FILE", &self.default_profile)
            .env("DEVENV_TRUST_FILE", &self.trust_file)
            .current_dir(&self.workspace);
        command
    }

    fn command_at_workspace(&self) -> Command {
        let mut command = self.command();
        command.env("DEVENV_CWD", &self.workspace);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct TestIdentityBroker {
    endpoint: AbstractEndpoint,
    stopping: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TestIdentityBroker {
    fn start(endpoint: AbstractEndpoint, home: PathBuf) -> Self {
        let listener = endpoint.bind(16).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stopping = Arc::new(AtomicBool::new(false));
        let should_stop = Arc::clone(&stopping);
        let thread = thread::spawn(move || {
            while !should_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let Ok(hello) = read_message::<ClientMessage>(&mut stream) else {
                            continue;
                        };
                        assert_eq!(hello.version, PROTOCOL_VERSION);
                        assert!(matches!(hello.request, ClientRequest::Hello));
                        let hello = ServerMessage {
                            version: PROTOCOL_VERSION,
                            response: ServerResponse::Hello(HelloInfo {
                                state: BackendState::Ready,
                                profile: "test".to_owned(),
                                snapshot_id: "test-snapshot".to_owned(),
                                runtime_inputs: Vec::new(),
                                environment_names: Vec::new(),
                                limits: ProtocolLimits::current(),
                            }),
                        };
                        write_message(&mut stream, &hello).unwrap();
                        let Ok(request) = read_message::<ClientMessage>(&mut stream) else {
                            continue;
                        };
                        let ClientRequest::PrepareIdentity { requested, .. } = request.request
                        else {
                            panic!("identity broker received an unexpected request");
                        };
                        let user = match requested {
                            IdentityRequest::Peer { .. } => "dev",
                            IdentityRequest::Root => "root",
                            IdentityRequest::User { ref name } => name,
                        };
                        let uid = unsafe { geteuid() };
                        let gid = unsafe { getegid() };
                        let identity = ServerMessage {
                            version: PROTOCOL_VERSION,
                            response: ServerResponse::Identity(PreparedIdentity {
                                uid,
                                gid,
                                user: user.to_owned(),
                                home: home.clone(),
                                supplemental_groups: Vec::new(),
                                run_as_root: uid == 0,
                            }),
                        };
                        write_message(&mut stream, &identity).unwrap();
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("identity test broker accept failed: {error}"),
                }
            }
        });
        Self {
            endpoint,
            stopping,
            thread: Some(thread),
        }
    }
}

impl Drop for TestIdentityBroker {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self
            .endpoint
            .connect_until(Instant::now() + Duration::from_secs(1));
        if let Some(thread) = self.thread.take() {
            thread.join().expect("identity test broker thread panicked");
        }
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn write_provider(path: &Path, source: &[u8]) {
    use std::os::unix::fs::PermissionsExt;

    let mut file = fs::File::create(path).unwrap();
    file.write_all(source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn start_backend(
    fixture: &Fixture,
    identity_endpoint: &AbstractEndpoint,
    initial_command: &str,
) -> ChildGuard {
    let mut command = fixture.command_at_workspace();
    command
        .env("CONTAINER_INIT_BACKEND_ENDPOINT", identity_endpoint.name())
        .env("CONTAINER_INIT_BACKEND_PEER_UID", "0")
        .env("DEVENV_BACKEND_SERVICE_COMMAND", "/bin/sh")
        .args([
            "backend",
            "run",
            "--initial-exec",
            "--",
            "/bin/sh",
            "-c",
            initial_command,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    ChildGuard(command.spawn().unwrap())
}

fn wait_for_provider_marker(backend: &mut ChildGuard, marker: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !marker.exists() {
        if let Some(status) = backend.0.try_wait().unwrap() {
            panic!("backend exited before its provider marker appeared: {status}");
        }
        assert!(Instant::now() < deadline, "provider marker was not created");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_initial_runtime(fixture: &Fixture, expected: &str, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    let mut last_status = "unavailable".to_owned();
    loop {
        let output = fixture.run(&["backend", "status", "--json"]);
        if output.status.success() {
            let status: Value = from_slice(&output.stdout).unwrap();
            last_status = status.to_string();
            if status["initial_runtime"].as_str() == Some(expected) {
                return status;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for initial_runtime={expected}; last status: {last_status}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn another_backend_owns_the_fixed_endpoint() -> bool {
    match BackendTransport::bind() {
        Ok(listener) => {
            drop(listener);
            false
        }
        Err(error) if error.kind() == ErrorKind::AddrInUse => true,
        Err(error) => panic!("could not probe backend abstract endpoint: {error}"),
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed with {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn slow_initial_provider_does_not_block_readiness_or_repeat_same_key_work() {
    let _backend_lifecycle = BACKEND_STARTUP_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(std::env::consts::OS, "linux");
    assert_eq!(unsafe { geteuid() }, 0, "this backend test requires root");
    if another_backend_owns_the_fixed_endpoint() {
        eprintln!("skipping backend startup test because a runtime owns the fixed endpoint");
        return;
    }
    let fixture = Fixture::new();
    let real_bash = Fixture::real_bash();
    let provider = fixture.root.join("slow-provider");
    let provider_start = fixture.workspace.join("provider-started");
    let extra_variables = format!(
        "PROVIDER_SLEEP_SECONDS = \"2\"\nPROVIDER_START_MARKER = \"{}\"",
        provider_start.display()
    );
    fixture.write_profile(&provider, &extra_variables, 120_000);
    write_provider(
        &provider,
        br#"#!/bin/sh
set -eu
case "$1" in
  prepare)
    printf 'prepare\n' >> "$PROVIDER_LOG"
    : > "$PROVIDER_START_MARKER"
    sleep "$PROVIDER_SLEEP_SECONDS"
    ;;
  env)
    printf 'env\n' >> "$PROVIDER_LOG"
    printf '%s\n' '{"FROM_PROVIDER":"ready"}'
    ;;
esac
"#,
    );
    let identity_socket = AbstractEndpoint::new(
        fixture
            .root
            .join("identity-broker")
            .to_string_lossy()
            .into_owned(),
    )
    .unwrap();
    let _identity_broker =
        TestIdentityBroker::start(identity_socket.clone(), fixture.root.join("home"));

    let mut clients = vec![
        fixture
            .command_at_workspace()
            .args([
                "--cwd",
                fixture.workspace.to_str().unwrap(),
                "exec",
                "--",
                "/bin/sh",
                "-c",
                "printf startup-exec",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
        fixture
            .command_at_workspace()
            .args([
                "shell",
                "--cwd",
                fixture.workspace.to_str().unwrap(),
                "--shell",
                "bash",
                "--",
                "-c",
                "printf startup-shell",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
        fixture
            .command_at_workspace()
            .args([
                "shim",
                "--cwd",
                fixture.workspace.to_str().unwrap(),
                "--shell",
                "bash",
                "--real",
                real_bash.to_str().unwrap(),
                "--",
                "-c",
                "printf startup-shim",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ];
    thread::sleep(Duration::from_millis(80));
    let mut backend = start_backend(&fixture, &identity_socket, "sleep 120");
    wait_for_provider_marker(&mut backend, &provider_start, Duration::from_secs(15));

    let status_started = Instant::now();
    let status = fixture.run(&["backend", "status", "--json"]);
    assert_success(&status);
    assert!(status_started.elapsed() < Duration::from_secs(2));
    let status: Value = from_slice(&status.stdout).unwrap();
    assert_eq!(status["control_plane"], "serving");
    assert_eq!(status["initial_runtime"], "preparing");

    let wait_ready = fixture.run(&["backend", "wait-ready", "--startup-timeout", "1000"]);
    assert_success(&wait_ready);
    let wait_ready: Value = from_slice(&wait_ready.stdout).unwrap();
    assert_eq!(wait_ready["control_plane"], "serving");
    assert_eq!(wait_ready["initial_runtime"], "preparing");

    let started = Instant::now();
    for (index, (client, expected)) in clients
        .drain(..)
        .zip(["startup-exec", "startup-shell", "startup-shim"])
        .enumerate()
    {
        let output = client.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "startup client {index} failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("backend_busy"));
    }
    assert!(started.elapsed() >= Duration::from_secs(1));
    let provider_log = fs::read_to_string(fixture.root.join("provider.log")).unwrap();
    assert_eq!(
        provider_log
            .lines()
            .filter(|line| *line == "prepare")
            .count(),
        1
    );
    assert_eq!(
        provider_log.lines().filter(|line| *line == "env").count(),
        1
    );

    let running = fixture.run(&["backend", "status", "--json"]);
    assert_success(&running);
    let running: Value = from_slice(&running.stdout).unwrap();
    assert_eq!(running["control_plane"], "serving");
    assert_eq!(running["initial_runtime"], "running");
    assert_success(&fixture.run(&["backend", "stop"]));
    assert_eq!(backend.0.wait().unwrap().code(), Some(143));
}

#[test]
fn provider_cleanup_keeps_initial_runtime_child_alive_during_another_prepare() {
    let _backend_lifecycle = BACKEND_STARTUP_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(unsafe { geteuid() }, 0, "this backend test requires root");
    if another_backend_owns_the_fixed_endpoint() {
        eprintln!("skipping backend startup test because a runtime owns the fixed endpoint");
        return;
    }

    let fixture = Fixture::new();
    let provider = fixture.root.join("isolated-provider");
    let initial_provider = fixture.workspace.join("initial-provider-started");
    let release_initial_provider = fixture.workspace.join("release-initial-provider");
    let regular_provider = fixture.workspace.join("regular-provider-started");
    let release_regular_provider = fixture.workspace.join("release-regular-provider");
    let initial_child = fixture.workspace.join("initial-child-pid");
    let extra_variables = format!(
        "WORKSPACE = \"{}\"\nINITIAL_PROVIDER_MARKER = \"{}\"\nINITIAL_PROVIDER_RELEASE = \"{}\"\nREGULAR_PROVIDER_MARKER = \"{}\"\nREGULAR_PROVIDER_RELEASE = \"{}\"",
        fixture.workspace.display(),
        initial_provider.display(),
        release_initial_provider.display(),
        regular_provider.display(),
        release_regular_provider.display(),
    );
    fixture.write_profile(&provider, &extra_variables, 30_000);
    let profile_path = fixture.profiles.join("00-startup.toml");
    let mut profile = fs::read_to_string(&profile_path).unwrap();
    profile.truncate(profile.find("[config.providers.startup.shellenv]").unwrap());
    fs::write(profile_path, profile).unwrap();
    write_provider(
        &provider,
        br#"#!/bin/sh
set -eu
case "$1" in
  prepare)
    case "$(pwd)" in
      "$WORKSPACE")
        : > "$INITIAL_PROVIDER_MARKER"
        while [ ! -e "$INITIAL_PROVIDER_RELEASE" ]; do sleep 0.01; done
        ;;
      *)
        : > "$REGULAR_PROVIDER_MARKER"
        while [ ! -e "$REGULAR_PROVIDER_RELEASE" ]; do sleep 0.01; done
        ;;
    esac
    ;;
  env)
    printf '%s\n' '{"FROM_PROVIDER":"isolated"}'
    ;;
esac
"#,
    );
    let identity_socket = AbstractEndpoint::new(
        fixture
            .root
            .join("identity-broker")
            .to_string_lossy()
            .into_owned(),
    )
    .unwrap();
    let _identity_broker =
        TestIdentityBroker::start(identity_socket.clone(), fixture.root.join("home"));
    let initial_command = format!("echo $$ > {}; exec sleep 120", initial_child.display());
    let mut backend = start_backend(&fixture, &identity_socket, &initial_command);
    wait_for_provider_marker(&mut backend, &initial_provider, Duration::from_secs(10));

    let regular_request = fixture
        .command()
        .args([
            "--cwd",
            fixture.nested.to_str().unwrap(),
            "exec",
            "--",
            "/bin/true",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    fs::write(&release_initial_provider, "continue\n").unwrap();
    wait_for_provider_marker(&mut backend, &regular_provider, Duration::from_secs(10));
    wait_for_initial_runtime(&fixture, "running", Duration::from_secs(10));
    let child_pid = fs::read_to_string(&initial_child).unwrap();
    let child_path = PathBuf::from(format!("/proc/{}", child_pid.trim()));
    assert!(child_path.exists(), "initial runtime child did not start");

    fs::write(&release_regular_provider, "continue\n").unwrap();
    let request_output = regular_request.wait_with_output().unwrap();
    assert_success(&request_output);
    assert!(
        child_path.exists(),
        "provider cleanup killed the initial runtime child"
    );
    assert!(backend.0.try_wait().unwrap().is_none());

    assert_success(&fixture.run(&["backend", "stop"]));
    assert_eq!(backend.0.wait().unwrap().code(), Some(143));
}

#[test]
fn provider_error_is_reported_without_retry_or_backend_shutdown() {
    let _backend_lifecycle = BACKEND_STARTUP_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(unsafe { geteuid() }, 0, "this backend test requires root");
    if another_backend_owns_the_fixed_endpoint() {
        eprintln!("skipping backend startup test because a runtime owns the fixed endpoint");
        return;
    }
    let fixture = Fixture::new();
    let provider = fixture.root.join("initial-only-provider");
    let marker = fixture.workspace.join("initial-provider-finished");
    let extra_variables = format!("PROVIDER_FAIL_MARKER = \"{}\"", marker.display());
    fixture.write_profile(&provider, &extra_variables, 1000);
    write_provider(
        &provider,
        br#"#!/bin/sh
set -eu
case "$1" in
  prepare)
    printf 'prepare\n' >> "$PROVIDER_LOG"
    if [ -e "$PROVIDER_FAIL_MARKER" ]; then
      echo 'intentional provider failure' >&2
      exit 42
    fi
    : > "$PROVIDER_FAIL_MARKER"
    ;;
  env)
    printf 'env\n' >> "$PROVIDER_LOG"
    printf '%s\n' '{"FROM_PROVIDER":"ready"}'
    ;;
esac
"#,
    );
    let identity_socket = AbstractEndpoint::new(
        fixture
            .root
            .join("identity-broker")
            .to_string_lossy()
            .into_owned(),
    )
    .unwrap();
    let _identity_broker =
        TestIdentityBroker::start(identity_socket.clone(), fixture.root.join("home"));
    let mut backend = start_backend(&fixture, &identity_socket, "sleep 120");
    wait_for_provider_marker(&mut backend, &marker, Duration::from_secs(10));
    wait_for_initial_runtime(&fixture, "running", Duration::from_secs(5));
    let ready = fixture.run(&["backend", "wait-ready", "--startup-timeout", "1000"]);
    assert_success(&ready);

    let failed_request = fixture
        .command()
        .env("DEVENV_CWD", &fixture.nested)
        .args(["exec", "--", "/bin/sh", "-c", "printf should-not-run"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&failed_request.stderr);
    assert_eq!(failed_request.status.code(), Some(70), "{stderr}");
    assert!(stderr.contains("DEVENV-E-PROVIDER"), "{stderr}");
    assert!(!stderr.contains("STARTUP-TIMEOUT"), "{stderr}");
    let provider_log = fs::read_to_string(fixture.root.join("provider.log")).unwrap();
    assert_eq!(
        provider_log
            .lines()
            .filter(|line| *line == "prepare")
            .count(),
        2
    );
    assert_eq!(
        provider_log.lines().filter(|line| *line == "env").count(),
        1
    );

    let status = fixture.run(&["backend", "status", "--json"]);
    assert_success(&status);
    let status: Value = from_slice(&status.stdout).unwrap();
    assert_eq!(status["control_plane"], "serving");
    assert_eq!(status["initial_runtime"], "running");
    assert_success(&fixture.run(&["backend", "stop"]));
    assert_eq!(backend.0.wait().unwrap().code(), Some(143));
}

#[test]
fn initial_provider_failure_releases_same_key_waiters_with_provider_error() {
    let _backend_lifecycle = BACKEND_STARTUP_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(unsafe { geteuid() }, 0, "this backend test requires root");
    if another_backend_owns_the_fixed_endpoint() {
        eprintln!("skipping backend startup test because a runtime owns the fixed endpoint");
        return;
    }

    let fixture = Fixture::new();
    let provider = fixture.root.join("failing-provider");
    let marker = fixture.workspace.join("provider-started");
    let extra_variables = format!("PROVIDER_START_MARKER = \"{}\"", marker.display());
    fixture.write_profile(&provider, &extra_variables, 10_000);
    write_provider(
        &provider,
        br#"#!/bin/sh
set -eu
case "$1" in
  prepare)
    printf 'prepare\n' >> "$PROVIDER_LOG"
    : > "$PROVIDER_START_MARKER"
    sleep 1
    echo 'intentional initial provider failure' >&2
    exit 42
    ;;
  env) printf '%s\n' '{}' ;;
esac
"#,
    );
    let identity_socket = AbstractEndpoint::new(
        fixture
            .root
            .join("identity-broker")
            .to_string_lossy()
            .into_owned(),
    )
    .unwrap();
    let _identity_broker =
        TestIdentityBroker::start(identity_socket.clone(), fixture.root.join("home"));
    let client = fixture
        .command_at_workspace()
        .args(["exec", "--", "/bin/sh", "-c", "printf should-not-run"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(80));
    let mut backend = start_backend(&fixture, &identity_socket, "sleep 120");
    wait_for_provider_marker(&mut backend, &marker, Duration::from_secs(10));

    let output = client.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(70), "{stderr}");
    assert!(stderr.contains("DEVENV-E-PROVIDER"), "{stderr}");
    assert!(!stderr.contains("STARTUP-TIMEOUT"), "{stderr}");
    assert_eq!(backend.0.wait().unwrap().code(), Some(70));
    let provider_log = fs::read_to_string(fixture.root.join("provider.log")).unwrap();
    assert_eq!(
        provider_log
            .lines()
            .filter(|line| *line == "prepare")
            .count(),
        1
    );
}

#[test]
fn initial_executable_failure_is_not_reported_as_a_readiness_timeout() {
    let _backend_lifecycle = BACKEND_STARTUP_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(unsafe { geteuid() }, 0, "this backend test requires root");
    if another_backend_owns_the_fixed_endpoint() {
        eprintln!("skipping backend startup test because a runtime owns the fixed endpoint");
        return;
    }
    let fixture = Fixture::new();
    let provider = fixture.root.join("provider");
    fixture.write_profile(&provider, "", 1000);
    write_provider(
        &provider,
        br#"#!/bin/sh
case "$1" in
  prepare) exit 0 ;;
  env) printf '%s\n' '{}' ;;
esac
"#,
    );
    let identity_socket = AbstractEndpoint::new(
        fixture
            .root
            .join("identity-broker")
            .to_string_lossy()
            .into_owned(),
    )
    .unwrap();
    let _identity_broker =
        TestIdentityBroker::start(identity_socket.clone(), fixture.root.join("home"));
    let output = fixture
        .command_at_workspace()
        .env("CONTAINER_INIT_BACKEND_ENDPOINT", identity_socket.name())
        .env("CONTAINER_INIT_BACKEND_PEER_UID", "0")
        .env("DEVENV_BACKEND_SERVICE_COMMAND", "/bin/sh")
        .args([
            "backend",
            "run",
            "--initial-exec",
            "--",
            "/missing/initial-runtime-executable",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(74), "{stderr}");
    assert!(
        stderr.contains("could not spawn initial runtime"),
        "{stderr}"
    );
    assert!(!stderr.contains("STARTUP-TIMEOUT"), "{stderr}");
}

#[test]
fn invalid_profile_fails_before_publishing_a_ready_endpoint() {
    let _backend_lifecycle = BACKEND_STARTUP_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(unsafe { geteuid() }, 0, "this backend test requires root");
    if another_backend_owns_the_fixed_endpoint() {
        eprintln!("skipping backend startup test because a runtime owns the fixed endpoint");
        return;
    }

    let fixture = Fixture::new();
    fs::write(fixture.profiles.join("00-startup.toml"), "schema = [\n").unwrap();
    let output = fixture
        .command_at_workspace()
        .args(["backend", "run", "--initial-exec", "--", "/bin/true"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "invalid profile unexpectedly started"
    );
    assert!(stderr.contains("profile"), "{stderr}");
    assert!(!stderr.contains("identity broker"), "{stderr}");
    assert!(!stderr.contains("STARTUP-TIMEOUT"), "{stderr}");
}
