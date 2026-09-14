use dev_env_core::{
    CoreError, MaterializationDiagnostic, Materializer, ProviderRuntime, RuntimeContext,
};
use dev_env_model::{
    ConditionalEnvironmentVariable, ConfiguredValuePrecedence, EnvironmentConfig, EnvironmentPath,
    FailurePolicy, MissingProviderPolicy, ModelError, Origin, PathMode, PolicyConfig, PrepareStep,
    ProviderConfig, ProviderReceipt, ResolvedConfig, Sensitivity, ShellConfig, ShellKind,
    ShellSelection, ValueTree, WorkspaceConfig, WorkspaceSearch,
};
use dev_env_provider::{
    DetectionResult, EnvironmentDelta, ProviderContext, ProviderRunResult, ProviderRuntimeError,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn shell() -> ShellConfig {
    ShellConfig {
        command: "/bin/sh".to_owned(),
        kind: ShellKind::Posix,
        login_args: vec!["-l".to_owned()],
        interactive_args: vec!["-i".to_owned()],
        command_arg: Some("-c".to_owned()),
    }
}

fn provider(depends_on: Vec<String>, path_mode: PathMode) -> ProviderConfig {
    ProviderConfig {
        executable: "fake-provider".to_owned(),
        detect_files: Vec::new(),
        missing: MissingProviderPolicy::Ignore,
        depends_on,
        prepare: vec![PrepareStep {
            argv: vec!["prepare".to_owned()],
            when: Some("provider.enabled".to_owned()),
            failure: FailurePolicy::Error,
            timeout_ms: Some(1_000),
            sensitivity: Sensitivity::Public,
        }],
        shellenv: Some(dev_env_model::ShellEnvConfig {
            argv: vec!["env".to_owned()],
            format: dev_env_model::ShellEnvFormat::Json,
            failure: FailurePolicy::Error,
            path_mode,
            timeout_ms: Some(1_000),
        }),
        sensitivity: Sensitivity::Public,
    }
}

fn config() -> ResolvedConfig {
    let workspace = std::env::current_dir().unwrap();
    let mut shells = BTreeMap::new();
    shells.insert("bash".to_owned(), shell());
    let mut providers = BTreeMap::new();
    providers.insert("base".to_owned(), provider(Vec::new(), PathMode::Merge));
    providers.insert(
        "project".to_owned(),
        provider(vec!["base".to_owned()], PathMode::Merge),
    );
    ResolvedConfig {
        workspace: WorkspaceConfig {
            root: workspace.display().to_string(),
            search: WorkspaceSearch::Upward,
        },
        shell: ShellSelection {
            default: "bash".to_owned(),
        },
        shells,
        environment: EnvironmentConfig {
            inherit_process: true,
            configured_value_precedence: ConfiguredValuePrecedence::Locked,
            variables: BTreeMap::from([
                ("CONFIGURED".to_owned(), "from-config".to_owned()),
                ("PATH".to_owned(), "/config-base".to_owned()),
            ]),
            conditional_variables: BTreeMap::new(),
            path: EnvironmentPath {
                prepend: vec!["/configured-first".to_owned()],
                append: vec![
                    "/configured-last".to_owned(),
                    "/configured-first".to_owned(),
                ],
                remove: vec!["/ambient-removed".to_owned()],
            },
        },
        features: ValueTree::Map(BTreeMap::new()),
        providers,
        inputs: BTreeMap::new(),
        policy: PolicyConfig::default(),
        provenance: Default::default(),
    }
}

fn context() -> RuntimeContext {
    let workspace = std::env::current_dir().unwrap();
    RuntimeContext::new(
        workspace.clone(),
        workspace,
        "bash",
        BTreeMap::from([
            ("CONFIGURED".to_owned(), "from-process".to_owned()),
            (
                "PATH".to_owned(),
                "/ambient-removed:/ambient:/configured-last".to_owned(),
            ),
            ("HOST_ONLY".to_owned(), "kept".to_owned()),
        ]),
    )
    .with_user_id(1000)
    .with_workspace_writable(true)
}

#[derive(Clone)]
struct FakeRuntime {
    calls: Arc<Mutex<Vec<(String, ProviderContext)>>>,
    outputs: Arc<Mutex<BTreeMap<String, Result<ProviderRunResult, ProviderRuntimeError>>>>,
}

impl FakeRuntime {
    fn new(outputs: BTreeMap<String, Result<ProviderRunResult, ProviderRuntimeError>>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            outputs: Arc::new(Mutex::new(outputs)),
        }
    }
}

impl ProviderRuntime for FakeRuntime {
    fn run(
        &self,
        provider_id: &str,
        _config: &ProviderConfig,
        context: &ProviderContext,
    ) -> Result<ProviderRunResult, ProviderRuntimeError> {
        self.calls
            .lock()
            .unwrap()
            .push((provider_id.to_owned(), context.clone()));
        self.outputs
            .lock()
            .unwrap()
            .remove(provider_id)
            .expect("test configured an output for every provider")
    }
}

fn result(provider: &str, path: Option<&str>, value: (&str, &str)) -> ProviderRunResult {
    let mut set = BTreeMap::from([(value.0.to_owned(), value.1.to_owned())]);
    if let Some(path) = path {
        set.insert("PATH".to_owned(), path.to_owned());
    }
    ProviderRunResult {
        provider: provider.to_owned(),
        sensitivity: Sensitivity::Public,
        detection: DetectionResult {
            executable: Some(PathBuf::from("/fake/provider")),
            matched_files: Vec::new(),
            applicable: true,
        },
        prepared_steps: 1,
        environment: EnvironmentDelta {
            set,
            unset: BTreeSet::new(),
        },
        diagnostics: Vec::new(),
        receipt: Some(ProviderReceipt {
            provider: provider.to_owned(),
            config_fingerprint: [1; 32],
            workspace_fingerprint: [2; 32],
            version: Some("test".to_owned()),
            completed_at: None,
        }),
    }
}

#[test]
fn materializes_ambient_and_configured_values_with_structured_path_operations() {
    let mut config = config();
    config.providers.clear();
    let materializer = Materializer::new(config);
    let output = materializer.materialize(&context()).unwrap();

    assert_eq!(output.environment.values["CONFIGURED"].value, "from-config");
    assert_eq!(output.environment.values["HOST_ONLY"].value, "kept");
    assert_eq!(
        output.environment.values["PATH"].value,
        "/configured-first:/config-base:/configured-last"
    );
    assert_eq!(output.environment.config_fingerprint.len(), 32);
}

#[test]
fn ambient_precedence_leaves_an_existing_process_value_in_place() {
    let mut config = config();
    config.providers.clear();
    config.environment.configured_value_precedence = ConfiguredValuePrecedence::Ambient;
    let output = Materializer::new(config).materialize(&context()).unwrap();
    assert_eq!(
        output.environment.values["CONFIGURED"].value,
        "from-process"
    );
}

#[test]
fn providers_run_in_dependency_order_and_receive_previous_environment() {
    let fake = FakeRuntime::new(BTreeMap::from([
        (
            "base".to_owned(),
            Ok(result(
                "base",
                Some("/provider-base:/configured-first"),
                ("BASE", "1"),
            )),
        ),
        (
            "project".to_owned(),
            Ok(result("project", None, ("PROJECT", "sees-base"))),
        ),
    ]));
    let calls = fake.calls.clone();
    let output = Materializer::with_provider_runtime(config(), fake)
        .materialize(&context())
        .unwrap();

    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|(provider, _)| provider.as_str())
            .collect::<Vec<_>>(),
        ["base", "project"]
    );
    assert_eq!(calls.lock().unwrap()[1].1.environment["BASE"], "1");
    assert_eq!(output.environment.values["PROJECT"].value, "sees-base");
    assert_eq!(
        output.environment.values["PATH"].value,
        "/provider-base:/configured-first:/config-base:/configured-last"
    );
    assert_eq!(output.environment.provider_receipts.len(), 2);
}

#[test]
fn disabled_provider_is_not_executed_and_is_reported_as_a_typed_diagnostic() {
    let mut config = config();
    config.providers.clear();
    config
        .providers
        .insert("tool".to_owned(), provider(Vec::new(), PathMode::Merge));
    config.features = ValueTree::Map(BTreeMap::from([(
        "tool".to_owned(),
        ValueTree::Map(BTreeMap::from([(
            "enabled".to_owned(),
            ValueTree::Bool(false),
        )])),
    )]));
    let fake = FakeRuntime::new(BTreeMap::new());
    let calls = fake.calls.clone();
    let output = Materializer::with_provider_runtime(config, fake)
        .materialize(&context())
        .unwrap();

    assert!(calls.lock().unwrap().is_empty());
    assert!(matches!(
        output.diagnostics.as_slice(),
        [MaterializationDiagnostic::ProviderDisabled { provider }] if provider == "tool"
    ));
}

#[test]
fn provider_failures_keep_the_original_structured_error_as_source() {
    let mut config = config();
    config.providers.retain(|id, _| id == "base");
    let fake = FakeRuntime::new(BTreeMap::from([(
        "base".to_owned(),
        Err(ProviderRuntimeError::Model(
            ModelError::MissingDefaultShell {
                shell: "missing".to_owned(),
            },
        )),
    )]));
    let error = Materializer::with_provider_runtime(config, fake)
        .materialize(&context())
        .unwrap_err();

    assert!(matches!(
        &error,
        CoreError::Provider {
            provider, source
        } if provider == "base"
            && matches!(
                source.as_ref(),
                ProviderRuntimeError::Model(ModelError::MissingDefaultShell { shell })
                    if shell == "missing"
            )
    ));
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn context_without_shell_uses_the_profile_default() {
    let mut config = config();
    config.providers.clear();
    let workspace = std::env::current_dir().unwrap();
    let context = RuntimeContext::without_shell(workspace.clone(), workspace, BTreeMap::new());
    let output = Materializer::new(config).materialize(&context).unwrap();
    output.environment.validate().unwrap();
}

#[test]
fn invalid_context_is_rejected_before_provider_execution() {
    let mut config = config();
    config.providers.clear();
    let context = RuntimeContext::new("/tmp/workspace", "/tmp/other", "bash", BTreeMap::new());
    let error = Materializer::new(config).materialize(&context).unwrap_err();
    assert!(matches!(error, CoreError::Context(_)));
}

#[test]
fn provider_sensitive_values_remain_sensitive_in_materialized_environment() {
    let mut config_with_provider_sensitivity = config();
    config_with_provider_sensitivity
        .providers
        .retain(|id, _| id == "base");
    config_with_provider_sensitivity
        .providers
        .get_mut("base")
        .unwrap()
        .sensitivity = Sensitivity::Secret;
    let fake = FakeRuntime::new(BTreeMap::from([(
        "base".to_owned(),
        Ok(result("base", None, ("TOKEN", "secret"))),
    )]));
    let output = Materializer::with_provider_runtime(config_with_provider_sensitivity, fake)
        .materialize(&context())
        .unwrap();
    assert_eq!(
        output.environment.values["TOKEN"].sensitivity,
        Sensitivity::Public
    );
    // The runtime result controls output sensitivity; the provider config is
    // intentionally not consulted a second time at the model boundary.
    let mut config = config();
    config.providers.retain(|id, _| id == "base");
    let fake = FakeRuntime::new(BTreeMap::from([(
        "base".to_owned(),
        Ok(ProviderRunResult {
            sensitivity: Sensitivity::Secret,
            ..result("base", None, ("TOKEN", "secret"))
        }),
    )]));
    let output = Materializer::with_provider_runtime(config, fake)
        .materialize(&context())
        .unwrap();
    assert_eq!(
        output.environment.values["TOKEN"].sensitivity,
        Sensitivity::Secret
    );
}

#[test]
fn config_provenance_is_attached_to_configured_environment_values() {
    let mut config = config();
    config.providers.clear();
    config.provenance.insert(
        "environment.variables.CONFIGURED",
        Origin::profile("base"),
        Sensitivity::Public,
    );
    let output = Materializer::new(config).materialize(&context()).unwrap();
    assert_eq!(
        output.environment.values["CONFIGURED"].origin,
        Some(Origin::profile("base"))
    );
}

#[test]
fn conditional_environment_variables_set_and_remove_sccache_wrapper() {
    let mut config = config();
    config.providers.clear();
    config.environment.conditional_variables.insert(
        "RUSTC_WRAPPER".to_owned(),
        ConditionalEnvironmentVariable {
            value: "sccache".to_owned(),
            when: "features.sccache.enabled && !features.sccache.disabled".to_owned(),
        },
    );
    config.provenance.insert(
        "environment.conditional_variables.RUSTC_WRAPPER",
        Origin::profile("rust"),
        Sensitivity::Public,
    );
    config.features = ValueTree::Map(BTreeMap::from([(
        "sccache".to_owned(),
        ValueTree::Map(BTreeMap::from([
            ("enabled".to_owned(), ValueTree::Bool(true)),
            ("disabled".to_owned(), ValueTree::Bool(false)),
        ])),
    )]));

    let mut enabled_context = context();
    enabled_context
        .process_environment
        .insert("RUSTC_WRAPPER".to_owned(), "ambient-wrapper".to_owned());
    let output = Materializer::new(config.clone())
        .materialize(&enabled_context)
        .unwrap();
    assert_eq!(output.environment.values["RUSTC_WRAPPER"].value, "sccache");
    assert_eq!(
        output.environment.values["RUSTC_WRAPPER"].origin,
        Some(Origin::profile("rust"))
    );

    config.features = ValueTree::Map(BTreeMap::from([(
        "sccache".to_owned(),
        ValueTree::Map(BTreeMap::from([
            ("enabled".to_owned(), ValueTree::Bool(true)),
            ("disabled".to_owned(), ValueTree::Bool(true)),
        ])),
    )]));
    let output = Materializer::new(config)
        .materialize(&enabled_context)
        .unwrap();
    assert!(!output.environment.values.contains_key("RUSTC_WRAPPER"));
}
