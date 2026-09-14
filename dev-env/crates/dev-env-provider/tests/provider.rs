use dev_env_model::{
    FailurePolicy, MaterializedEnv, MissingProviderPolicy, PathMode, PrepareStep, ProviderConfig,
    Sensitivity, ShellEnvConfig, ShellEnvFormat, ValueTree,
};
use dev_env_provider::{
    decode_request, encode_response, CommandExecutor, CommandOutput, CommandRequest,
    ExecutableLocator, GenericProvider, LockManager, ProtocolOperation, ProtocolRequest,
    ProtocolResponse, Provider, ProviderContext, ProviderDiagnostic, ProviderRunner,
    ProviderRuntimeError, ProviderRuntimeErrorKind, SystemExecutableLocator,
};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "dev-env-provider-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
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
    ProviderConfig {
        executable: "fake-provider".to_owned(),
        detect_files: vec!["project.config".to_owned()],
        missing: MissingProviderPolicy::Error,
        depends_on: Vec::new(),
        prepare,
        shellenv,
        sensitivity: Sensitivity::Public,
    }
}

fn prepare(when: Option<&str>, failure: FailurePolicy) -> PrepareStep {
    PrepareStep {
        argv: vec![
            "prepare".to_owned(),
            "{shell}".to_owned(),
            "{workspace}".to_owned(),
        ],
        when: when.map(str::to_owned),
        failure,
        timeout_ms: Some(500),
        sensitivity: Sensitivity::Public,
    }
}

fn shellenv(format: ShellEnvFormat, failure: FailurePolicy) -> ShellEnvConfig {
    ShellEnvConfig {
        argv: vec!["env".to_owned(), "{shell}".to_owned()],
        format,
        failure,
        path_mode: PathMode::Merge,
        timeout_ms: Some(500),
    }
}

#[test]
fn lifecycle_detects_files_runs_prepare_and_returns_json_delta_and_receipt() {
    let workspace = TempWorkspace::new();
    std::fs::write(workspace.path.join("project.config"), "version = 1\n").unwrap();
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
    )
    .without_locks();
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

    assert!(result.detection.applicable);
    assert_eq!(result.detection.matched_files.len(), 1);
    assert_eq!(result.prepared_steps, 1);
    assert_eq!(result.environment.set["TOOL_HOME"], "/data/tool");
    assert!(result.environment.unset.contains("REMOVE_ME"));
    let receipt = result.receipt.unwrap();
    assert_eq!(receipt.provider, "tool");
    assert_eq!(receipt.config_fingerprint, [7; 32]);
    assert_ne!(receipt.workspace_fingerprint, [0; 32]);

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].program, "/fake/provider");
    assert_eq!(
        calls[0].args,
        vec![
            "prepare".to_owned(),
            "bash".to_owned(),
            workspace.path.display().to_string()
        ]
    );
    assert_eq!(calls[1].args, vec!["env".to_owned(), "bash".to_owned()]);
}

#[test]
fn file_gating_is_not_a_missing_provider_error() {
    let workspace = TempWorkspace::new();
    let executor = FakeExecutor::default();
    let runner =
        ProviderRunner::with_executor_and_locator(executor, FakeLocator { executable: None })
            .without_locks();
    let result = runner
        .run(
            "tool",
            &provider(Vec::new(), None),
            &context(&workspace.path),
        )
        .unwrap();

    assert!(!result.detection.applicable);
    assert!(matches!(
        result.diagnostics.as_slice(),
        [ProviderDiagnostic::NotApplicable { provider }] if provider == "tool"
    ));
}

#[test]
fn applicable_missing_provider_follows_missing_policy() {
    let workspace = TempWorkspace::new();
    let mut config = provider(Vec::new(), None);
    config.detect_files.clear();
    config.missing = MissingProviderPolicy::Warn;
    let runner = ProviderRunner::with_executor_and_locator(
        FakeExecutor::default(),
        FakeLocator { executable: None },
    )
    .without_locks();
    let result = runner
        .run("tool", &config, &context(&workspace.path))
        .unwrap();
    assert!(matches!(
        result.diagnostics.as_slice(),
        [ProviderDiagnostic::MissingExecutable { provider, .. }] if provider == "tool"
    ));

    config.missing = MissingProviderPolicy::Error;
    let error = runner
        .run("tool", &config, &context(&workspace.path))
        .unwrap_err();
    assert_eq!(error.kind(), ProviderRuntimeErrorKind::MissingExecutable);
    assert_eq!(error.kind().code(), "DEVENV-E-PROVIDER-MISSING");
}

#[test]
fn shellenv_parser_rejects_shell_code_as_a_typed_error() {
    let workspace = TempWorkspace::new();
    std::fs::write(workspace.path.join("project.config"), "enabled\n").unwrap();
    let executor = FakeExecutor::with_outputs([CommandOutput::success(
        b"export BAD=$(touch /tmp/provider-should-not-run)\n".to_vec(),
    )]);
    let runner = ProviderRunner::with_executor_and_locator(
        executor,
        FakeLocator {
            executable: Some(PathBuf::from("/fake/provider")),
        },
    )
    .without_locks();
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
    std::fs::write(workspace.path.join("project.config"), "enabled\n").unwrap();
    let executor = FakeExecutor::with_outputs([
        CommandOutput {
            status: Some(23),
            timed_out: false,
            stdout: Vec::new(),
            stderr: b"temporary failure".to_vec(),
        },
        CommandOutput::success(b"OK=1\n".to_vec()),
    ]);
    let runner = ProviderRunner::with_executor_and_locator(
        executor,
        FakeLocator {
            executable: Some(PathBuf::from("/fake/provider")),
        },
    )
    .without_locks();
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

    assert_eq!(result.prepared_steps, 0);
    assert_eq!(result.environment.set["OK"], "1");
    assert!(matches!(
        result.diagnostics.as_slice(),
        [ProviderDiagnostic::PrepareFailed { status: Some(23), stderr, .. }] if stderr == b"temporary failure"
    ));
}

#[test]
fn lock_manager_serializes_the_same_workspace_user_and_provider_key() {
    let workspace = TempWorkspace::new();
    let locks = TempWorkspace::new();
    let manager = LockManager::new(&locks.path).with_poll_interval(Duration::from_millis(1));
    let key = dev_env_provider::lock_key(&workspace.path, 1000, "tool");
    let first = manager.acquire(key, Duration::from_millis(50)).unwrap();
    let error = manager.acquire(key, Duration::from_millis(10)).unwrap_err();
    assert!(matches!(error, dev_env_provider::LockError::Timeout { .. }));
    drop(first);
    let second = manager.acquire(key, Duration::from_millis(50)).unwrap();
    assert!(second.path().exists());
}

#[test]
fn protocol_round_trip_keeps_typed_operation_and_response_error() {
    let workspace = TempWorkspace::new();
    let request = ProtocolRequest {
        protocol: 1,
        op: ProtocolOperation::Shellenv,
        context: context(&workspace.path),
        config: ValueTree::Map(BTreeMap::from([(
            "enabled".to_owned(),
            ValueTree::Bool(true),
        )])),
        argv: vec!["env".to_owned()],
    };
    let encoded = serde_json::to_vec(&request).unwrap();
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
    let decoded: ProtocolResponse = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, response);
}

#[test]
fn real_process_executor_passes_an_argument_as_data() {
    let program = if Path::new("/usr/bin/printf").is_file() {
        "/usr/bin/printf"
    } else {
        "/bin/printf"
    };
    let request = CommandRequest {
        program: program.to_owned(),
        args: vec!["%s".to_owned(), "value; not shell code".to_owned()],
        cwd: std::env::temp_dir(),
        environment: BTreeMap::new(),
        timeout: Some(Duration::from_secs(1)),
        max_stdout_bytes: 1024,
        max_stderr_bytes: 1024,
    };
    let output = SystemExecutableLocator;
    assert!(output.locate(program, &BTreeMap::new()).unwrap().is_some());
    let output = dev_env_provider::ProcessExecutor.execute(&request).unwrap();
    assert!(output.succeeded());
    assert_eq!(output.stdout, b"value; not shell code");
}

#[test]
fn generic_provider_trait_uses_the_same_runner_api() {
    let workspace = TempWorkspace::new();
    std::fs::write(workspace.path.join("project.config"), "enabled\n").unwrap();
    let provider = GenericProvider::new(
        "tool",
        provider(Vec::new(), None),
        ProviderRunner::with_executor_and_locator(
            FakeExecutor::default(),
            FakeLocator {
                executable: Some(PathBuf::from("/fake/provider")),
            },
        )
        .without_locks(),
    );
    assert_eq!(Provider::id(&provider), "tool");
    assert!(
        Provider::run(&provider, &context(&workspace.path))
            .unwrap()
            .detection
            .applicable
    );
}

#[test]
fn provider_result_applies_provider_sensitivity_and_receipt_to_model_environment() {
    let workspace = TempWorkspace::new();
    std::fs::write(workspace.path.join("project.config"), "enabled\n").unwrap();
    let executor = FakeExecutor::with_outputs([CommandOutput::success(b"SECRET=token\n".to_vec())]);
    let runner = ProviderRunner::with_executor_and_locator(
        executor,
        FakeLocator {
            executable: Some(PathBuf::from("/fake/provider")),
        },
    )
    .without_locks();
    let mut config = provider(
        Vec::new(),
        Some(shellenv(ShellEnvFormat::Shell, FailurePolicy::Error)),
    );
    config.sensitivity = Sensitivity::Secret;
    let result = runner
        .run("tool", &config, &context(&workspace.path))
        .unwrap();
    let mut environment = MaterializedEnv::default();
    result.apply_to(&mut environment);

    assert_eq!(environment.values["SECRET"].value, "token");
    assert_eq!(
        environment.values["SECRET"].sensitivity,
        Sensitivity::Secret
    );
    assert_eq!(
        environment.values["SECRET"].provider.as_deref(),
        Some("tool")
    );
}

#[test]
fn glob_detection_supports_nested_files_without_following_a_shell() {
    let workspace = TempWorkspace::new();
    std::fs::create_dir_all(workspace.path.join("nested/project")).unwrap();
    std::fs::write(workspace.path.join("nested/project/env.json"), "{}\n").unwrap();
    let mut config = provider(Vec::new(), None);
    config.detect_files = vec!["**/*.json".to_owned()];
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
    let mut request = ProtocolRequest {
        protocol: 2,
        op: ProtocolOperation::Detect,
        context: context(&workspace.path),
        config: ValueTree::default(),
        argv: Vec::new(),
    };
    let bytes = serde_json::to_vec(&request).unwrap();
    assert!(matches!(
        decode_request(&bytes),
        Err(dev_env_provider::ProtocolCodecError::Protocol {
            source: dev_env_provider::ProtocolError::UnsupportedProtocol {
                found: 2,
                expected: 1
            }
        })
    ));
    request.protocol = 1;
    request.context.shell.clear();
    let bytes = serde_json::to_vec(&request).unwrap();
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
    let request = CommandRequest {
        program: program.to_owned(),
        args: vec!["1".to_owned()],
        cwd: std::env::temp_dir(),
        environment: BTreeMap::new(),
        timeout: Some(Duration::from_millis(20)),
        max_stdout_bytes: 1024,
        max_stderr_bytes: 1024,
    };
    let output = dev_env_provider::ProcessExecutor.execute(&request).unwrap();
    assert!(output.timed_out);
    assert!(!output.succeeded());
}
