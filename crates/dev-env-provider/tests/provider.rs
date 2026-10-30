use dev_env_model::{
    FailurePolicy, MaterializedEnv, MissingProviderPolicy, PathMode, PrepareStep, ProviderConfig,
    Sensitivity, ShellEnvConfig, ShellEnvFormat, ValueTree,
};
use dev_env_provider::{
    decode_request, encode_response, CommandExecutor, CommandOutput, CommandRequest,
    ExecutableLocator, GenericProvider, ProtocolOperation, ProtocolRequest, ProtocolResponse,
    Provider, ProviderContext, ProviderDiagnostic, ProviderProcessSupervisor, ProviderRunner,
    ProviderRuntimeError, ProviderRuntimeErrorKind, SystemExecutableLocator,
};
use serde_json::{from_slice, to_vec};
use std::{
    collections::{BTreeMap, VecDeque},
    env::temp_dir,
    fs, io,
    path::{Path, PathBuf},
    process::id,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = temp_dir().join(format!(
            "dev-env-provider-test-{}-{}",
            id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn process_executor() -> dev_env_provider::ProcessExecutor {
    static SUPERVISOR: OnceLock<ProviderProcessSupervisor> = OnceLock::new();
    SUPERVISOR
        .get_or_init(ProviderProcessSupervisor::new)
        .executor()
}

#[derive(Clone)]
struct FakeLocator {
    executable: Option<PathBuf>,
}

impl ExecutableLocator for FakeLocator {
    fn locate(
        &self,
        _executable: &str,
        _environment: &BTreeMap<String, String>,
    ) -> Result<Option<PathBuf>, io::Error> {
        Ok(self.executable.clone())
    }
}

#[derive(Clone, Default)]
struct FakeExecutor {
    outputs: Arc<Mutex<VecDeque<CommandOutput>>>,
    calls: Arc<Mutex<Vec<CommandRequest>>>,
}

impl FakeExecutor {
    fn with_outputs(outputs: impl IntoIterator<Item = CommandOutput>) -> Self {
        Self {
            outputs: Arc::new(Mutex::new(outputs.into_iter().collect())),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl CommandExecutor for FakeExecutor {
    fn execute(
        &self,
        request: &CommandRequest,
    ) -> Result<CommandOutput, dev_env_provider::CommandError> {
        self.calls.lock().unwrap().push(request.clone());
        Ok(self
            .outputs
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| CommandOutput::success(Vec::new())))
    }
}

fn context(workspace: &Path) -> ProviderContext {
    ProviderContext::new(
        workspace,
        workspace,
        "bash",
        BTreeMap::from([("PATH".to_owned(), "/bin".to_owned())]),
    )
    .with_config(ValueTree::Map(BTreeMap::from([(
        "features".to_owned(),
        ValueTree::Map(BTreeMap::from([(
            "tool".to_owned(),
            ValueTree::Map(BTreeMap::from([(
                "mode".to_owned(),
                ValueTree::String("enabled".to_owned()),
            )])),
        )])),
    )])))
    .with_workspace_writable(true)
    .with_user_id(1000)
    .with_config_fingerprint([7; 32])
}

fn provider(prepare: Vec<PrepareStep>, shellenv: Option<ShellEnvConfig>) -> ProviderConfig {
    let config = ProviderConfig::new(
        "fake-provider",
        vec!["project.config".to_owned()],
        MissingProviderPolicy::Error,
        Vec::new(),
        prepare,
    );
    match shellenv {
        Some(shellenv) => config.with_shellenv(shellenv),
        None => config,
    }
}

fn prepare(when: Option<&str>, failure: FailurePolicy) -> PrepareStep {
    PrepareStep::new(
        vec![
            "prepare".to_owned(),
            "{shell}".to_owned(),
            "{workspace}".to_owned(),
        ],
        when.map(str::to_owned),
        failure,
        Some(500),
        Sensitivity::Public,
    )
}

fn shellenv(format: ShellEnvFormat, failure: FailurePolicy) -> ShellEnvConfig {
    ShellEnvConfig::new(
        vec!["env".to_owned(), "{shell}".to_owned()],
        format,
        failure,
        PathMode::Merge,
        Some(500),
    )
}

#[test]
fn lifecycle_detects_files_runs_prepare_and_returns_json_delta_and_receipt() {
    let workspace = TempWorkspace::new();
    fs::write(workspace.path.join("project.config"), "version = 1\n").unwrap();
    let executor = FakeExecutor::with_outputs([
        CommandOutput::success(Vec::new()),
        CommandOutput::success(br#"{"TOOL_HOME":"/data/tool","REMOVE_ME":null}"#),
    ]);
    let calls = executor.calls.clone();
    let runner = ProviderRunner::with_executor_and_locator(
        executor,
        FakeLocator {
            executable: Some(PathBuf::from("/fake/provider")),
        },
    );
    let config = provider(
        vec![prepare(
            Some("workspace.config-present && features.tool.mode == 'enabled'"),
            FailurePolicy::Error,
        )],
        Some(shellenv(ShellEnvFormat::Json, FailurePolicy::Error)),
    );

    let result = runner
        .run("tool", &config, &context(&workspace.path))
        .unwrap();

    assert!(result.detection().applicable);
    assert_eq!(result.detection().matched_files.len(), 1);
    assert_eq!(result.prepared_steps(), 1);
    assert_eq!(result.environment().set()["TOOL_HOME"], "/data/tool");
    assert!(result.environment().unset().contains("REMOVE_ME"));
    let receipt = result.receipt().unwrap();
    assert_eq!(receipt.provider, "tool");
    assert_eq!(receipt.config_fingerprint, [7; 32]);
    assert_ne!(receipt.workspace_fingerprint, [0; 32]);

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].program(), "/fake/provider");
    assert_eq!(
        calls[0].args(),
        vec![
            "prepare".to_owned(),
            "bash".to_owned(),
            workspace.path.display().to_string()
        ]
    );
    assert_eq!(calls[1].args(), ["env", "bash"]);
}

#[test]
fn file_gating_is_not_a_missing_provider_error() {
    let workspace = TempWorkspace::new();
    let executor = FakeExecutor::default();
    let runner =
        ProviderRunner::with_executor_and_locator(executor, FakeLocator { executable: None });
    let result = runner
        .run(
            "tool",
            &provider(Vec::new(), None),
            &context(&workspace.path),
        )
        .unwrap();

    assert!(!result.detection().applicable);
    assert!(matches!(
        result.diagnostics(),
        [ProviderDiagnostic::NotApplicable { provider }] if provider == "tool"
    ));
}

#[test]
fn applicable_missing_provider_follows_missing_policy() {
    let workspace = TempWorkspace::new();
    let config = provider(Vec::new(), None)
        .with_detect_files(Vec::new())
        .with_missing(MissingProviderPolicy::Warn);
    let runner = ProviderRunner::with_executor_and_locator(
        FakeExecutor::default(),
        FakeLocator { executable: None },
    );
    let result = runner
        .run("tool", &config, &context(&workspace.path))
        .unwrap();
    assert!(matches!(
        result.diagnostics(),
        [ProviderDiagnostic::MissingExecutable { provider, .. }] if provider == "tool"
    ));

    let config = config.with_missing(MissingProviderPolicy::Error);
    let error = runner
        .run("tool", &config, &context(&workspace.path))
        .unwrap_err();
    assert_eq!(error.kind(), ProviderRuntimeErrorKind::MissingExecutable);
    assert_eq!(error.kind().code(), "DEVENV-E-PROVIDER-MISSING");
}

#[test]
fn shellenv_parser_rejects_shell_code_as_a_typed_error() {
    let workspace = TempWorkspace::new();
    fs::write(workspace.path.join("project.config"), "enabled\n").unwrap();
    let executor = FakeExecutor::with_outputs([CommandOutput::success(
        b"export BAD=$(touch /tmp/provider-should-not-run)\n".to_vec(),
    )]);
    let runner = ProviderRunner::with_executor_and_locator(
        executor,
        FakeLocator {
            executable: Some(PathBuf::from("/fake/provider")),
        },
    );
    let error = runner
        .run(
            "tool",
            &provider(
                Vec::new(),
                Some(shellenv(ShellEnvFormat::Shell, FailurePolicy::Error)),
            ),
            &context(&workspace.path),
        )
        .unwrap_err();

    assert_eq!(error.kind(), ProviderRuntimeErrorKind::OutputRejected);
    assert!(matches!(
        error,
        ProviderRuntimeError::OutputRejected {
            source: dev_env_provider::EnvironmentParseError::Shell { .. },
            ..
        }
    ));
}

#[test]
fn prepare_warning_preserves_exit_status_and_stderr_without_flattening_error() {
    let workspace = TempWorkspace::new();
    fs::write(workspace.path.join("project.config"), "enabled\n").unwrap();
    let executor = FakeExecutor::with_outputs([
        CommandOutput::new(Some(23), false, Vec::new(), b"temporary failure".to_vec()),
        CommandOutput::success(b"OK=1\n".to_vec()),
    ]);
    let runner = ProviderRunner::with_executor_and_locator(
        executor,
        FakeLocator {
            executable: Some(PathBuf::from("/fake/provider")),
        },
    );
    let result = runner
        .run(
            "tool",
            &provider(
                vec![prepare(None, FailurePolicy::Warn)],
                Some(shellenv(ShellEnvFormat::Dotenv, FailurePolicy::Error)),
            ),
            &context(&workspace.path),
        )
        .unwrap();

    assert_eq!(result.prepared_steps(), 0);
    assert_eq!(result.environment().set()["OK"], "1");
    assert!(matches!(
        result.diagnostics(),
        [ProviderDiagnostic::PrepareFailed { status: Some(23), stderr, .. }] if stderr == b"temporary failure"
    ));
}

#[test]
fn protocol_round_trip_keeps_typed_operation_and_response_error() {
    let workspace = TempWorkspace::new();
    let request = ProtocolRequest::new(
        1,
        ProtocolOperation::Shellenv,
        context(&workspace.path),
        ValueTree::Map(BTreeMap::from([(
            "enabled".to_owned(),
            ValueTree::Bool(true),
        )])),
        vec!["env".to_owned()],
    );
    let encoded = to_vec(&request).unwrap();
    let decoded = decode_request(&encoded).unwrap();
    assert_eq!(decoded, request);

    let response = ProtocolResponse {
        ok: false,
        error: Some(dev_env_provider::ProtocolError::UnsupportedProtocol {
            found: 2,
            expected: 1,
        }),
        ..ProtocolResponse::default()
    };
    let encoded = encode_response(&response).unwrap();
    let decoded: ProtocolResponse = from_slice(&encoded).unwrap();
    assert_eq!(decoded, response);
}

#[test]
fn real_process_executor_passes_an_argument_as_data() {
    let program = if Path::new("/usr/bin/printf").is_file() {
        "/usr/bin/printf"
    } else {
        "/bin/printf"
    };
    let request = CommandRequest::new(program, temp_dir())
        .with_args(["%s".to_owned(), "value; not shell code".to_owned()])
        .with_environment(BTreeMap::new())
        .with_timeout(Some(Duration::from_secs(1)))
        .with_output_limits(1024, 1024);
    let output = SystemExecutableLocator;
    assert!(output.locate(program, &BTreeMap::new()).unwrap().is_some());
    let output = process_executor().execute(&request).unwrap();
    assert!(output.succeeded());
    assert_eq!(output.stdout(), b"value; not shell code");
}

#[test]
fn generic_provider_trait_uses_the_same_runner_api() {
    let workspace = TempWorkspace::new();
    fs::write(workspace.path.join("project.config"), "enabled\n").unwrap();
    let provider = GenericProvider::new(
        "tool",
        provider(Vec::new(), None),
        ProviderRunner::with_executor_and_locator(
            FakeExecutor::default(),
            FakeLocator {
                executable: Some(PathBuf::from("/fake/provider")),
            },
        ),
    );
    assert_eq!(Provider::id(&provider), "tool");
    assert!(
        Provider::run(&provider, &context(&workspace.path))
            .unwrap()
            .detection()
            .applicable
    );
}

#[test]
fn provider_result_applies_provider_sensitivity_and_receipt_to_model_environment() {
    let workspace = TempWorkspace::new();
    fs::write(workspace.path.join("project.config"), "enabled\n").unwrap();
    let executor = FakeExecutor::with_outputs([CommandOutput::success(b"SECRET=token\n".to_vec())]);
    let runner = ProviderRunner::with_executor_and_locator(
        executor,
        FakeLocator {
            executable: Some(PathBuf::from("/fake/provider")),
        },
    );
    let config = provider(
        Vec::new(),
        Some(shellenv(ShellEnvFormat::Shell, FailurePolicy::Error)),
    )
    .with_sensitivity(Sensitivity::Secret);
    let result = runner
        .run("tool", &config, &context(&workspace.path))
        .unwrap();
    let mut environment = MaterializedEnv::default();
    result.apply_to(&mut environment);

    assert_eq!(environment.values()["SECRET"].value(), "token");
    assert_eq!(
        environment.values()["SECRET"].sensitivity(),
        Sensitivity::Secret
    );
    assert_eq!(environment.values()["SECRET"].provider(), Some("tool"));
}

#[test]
fn glob_detection_supports_nested_files_without_following_a_shell() {
    let workspace = TempWorkspace::new();
    fs::create_dir_all(workspace.path.join("nested/project")).unwrap();
    fs::write(workspace.path.join("nested/project/env.json"), "{}\n").unwrap();
    let config = provider(Vec::new(), None).with_detect_files(vec!["**/*.json".to_owned()]);
    let result = dev_env_provider::detect(
        &config,
        &workspace.path,
        &BTreeMap::new(),
        &FakeLocator {
            executable: Some(PathBuf::from("/fake/provider")),
        },
    )
    .unwrap();
    assert!(result.applicable);
    assert_eq!(result.matched_files.len(), 1);
}

#[test]
fn protocol_decoder_rejects_a_wrong_version_as_a_typed_protocol_error() {
    let workspace = TempWorkspace::new();
    let mut request = ProtocolRequest::new(
        2,
        ProtocolOperation::Detect,
        context(&workspace.path),
        ValueTree::default(),
        Vec::new(),
    );
    let bytes = to_vec(&request).unwrap();
    assert!(matches!(
        decode_request(&bytes),
        Err(dev_env_provider::ProtocolCodecError::Protocol {
            source: dev_env_provider::ProtocolError::UnsupportedProtocol {
                found: 2,
                expected: 1
            }
        })
    ));
    request = request.with_protocol(1);
    request.context_mut().shell_mut().clear();
    let bytes = to_vec(&request).unwrap();
    assert!(matches!(
        decode_request(&bytes),
        Err(dev_env_provider::ProtocolCodecError::Protocol {
            source: dev_env_provider::ProtocolError::InvalidRequest { .. }
        })
    ));
}

#[test]
fn process_executor_enforces_a_timeout() {
    let program = if Path::new("/usr/bin/sleep").is_file() {
        "/usr/bin/sleep"
    } else {
        "/bin/sleep"
    };
    if !Path::new(program).is_file() {
        return;
    }
    let request = CommandRequest::new(program, temp_dir())
        .with_args(["1".to_owned()])
        .with_environment(BTreeMap::new())
        .with_timeout(Some(Duration::from_millis(20)))
        .with_output_limits(1024, 1024);
    let output = process_executor().execute(&request).unwrap();
    assert!(output.timed_out());
    assert!(!output.succeeded());
}

#[test]
fn shared_supervisor_keeps_concurrent_direct_child_statuses_separate() {
    let executor = process_executor();
    let program = "/bin/sh";
    if !Path::new(program).is_file() {
        return;
    }
    let first_request = CommandRequest::new(program, temp_dir())
        .with_args(["-c".to_owned(), "sleep 0.03; exit 23".to_owned()])
        .with_environment(BTreeMap::from([(
            "PATH".to_owned(),
            "/usr/bin:/bin".to_owned(),
        )]))
        .with_timeout(Some(Duration::from_secs(1)));
    let second_request = CommandRequest::new(program, temp_dir())
        .with_args(["-c".to_owned(), "sleep 0.01; exit 42".to_owned()])
        .with_environment(BTreeMap::from([(
            "PATH".to_owned(),
            "/usr/bin:/bin".to_owned(),
        )]))
        .with_timeout(Some(Duration::from_secs(1)));
    let first_executor = executor.clone();
    let first = thread::spawn(move || first_executor.execute(&first_request).unwrap());
    let second = thread::spawn(move || executor.execute(&second_request).unwrap());

    assert_eq!(first.join().unwrap().status(), Some(23));
    assert_eq!(second.join().unwrap().status(), Some(42));
}

#[test]
fn process_timeout_reaps_a_descendant_that_leaves_the_command_group() {
    let program = "/bin/sh";
    let setsid = if Path::new("/usr/bin/setsid").is_file() {
        "/usr/bin/setsid"
    } else if Path::new("/bin/setsid").is_file() {
        "/bin/setsid"
    } else {
        return;
    };
    if !Path::new(program).is_file() {
        return;
    }
    let marker = temp_dir().join(format!(
        "dev-env-provider-child-{}-{}",
        id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let command = format!(
        "{setsid} /bin/sh -c 'sleep 30' >/dev/null 2>&1 & echo $! > \"$PID_FILE\"; sleep 30"
    );
    let request = CommandRequest::new(program, temp_dir())
        .with_args(["-c".to_owned(), command])
        .with_environment(BTreeMap::from([
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("PID_FILE".to_owned(), marker.display().to_string()),
        ]))
        .with_timeout(Some(Duration::from_millis(100)));
    let output = process_executor().execute(&request).unwrap();

    assert!(output.timed_out());
    let child_pid = fs::read_to_string(&marker).unwrap();
    let child_path = PathBuf::from(format!("/proc/{}", child_pid.trim()));
    for _ in 0..100 {
        if !child_path.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!child_path.exists(), "provider descendant was not reaped");
    let _ = fs::remove_file(marker);
}
