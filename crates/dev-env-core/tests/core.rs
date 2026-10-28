use dev_env_core::{
    CoreError, MaterializationDiagnostic, Materializer, ProviderJob, ProviderSupervisor,
    RuntimeContext,
};
use dev_env_model::{
    ConditionalEnvironmentVariable, ConfiguredValuePrecedence, EnvironmentConfig, EnvironmentPath,
    FailurePolicy, MissingProviderPolicy, ModelError, Origin, PathMode, PrepareStep,
    ProviderConfig, ProviderReceipt, ResolvedConfig, Sensitivity, ShellConfig, ShellEnvConfig,
    ShellEnvFormat, ShellKind, ShellSelection, ValueTree, WorkspaceConfig,
};
use dev_env_provider::{
    DetectionResult, EnvironmentDelta, ProviderContext, ProviderRunResult, ProviderRunResultParts,
    ProviderRuntimeError,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    env::current_dir,
    error::Error,
    path::PathBuf,
    sync::{Arc, Mutex},
};

fn shell() -> ShellConfig {
    ShellConfig::new(
        "/bin/sh",
        ShellKind::Posix,
        vec!["-l".to_owned()],
        vec!["-i".to_owned()],
        Some("-c".to_owned()),
    )
}

fn provider(depends_on: Vec<String>, path_mode: PathMode) -> ProviderConfig {
    ProviderConfig::new(
        "fake-provider",
        Vec::new(),
        MissingProviderPolicy::Ignore,
        depends_on,
        vec![PrepareStep::new(
            vec!["prepare".to_owned()],
            Some("provider.enabled".to_owned()),
            FailurePolicy::Error,
            Some(1_000),
            Sensitivity::Public,
        )],
    )
    .with_shellenv(ShellEnvConfig::new(
        vec!["env".to_owned()],
        ShellEnvFormat::Json,
        FailurePolicy::Error,
        path_mode,
        Some(1_000),
    ))
}

fn only_provider(config: ResolvedConfig, id: &str) -> ResolvedConfig {
    let providers = config
        .providers()
        .iter()
        .filter(|(provider_id, _)| provider_id.as_str() == id)
        .map(|(provider_id, provider)| (provider_id.clone(), provider.clone()))
        .collect();
    config.with_providers(providers)
}

fn config() -> ResolvedConfig {
    let workspace = current_dir().unwrap();
    let mut shells = BTreeMap::new();
    shells.insert("bash".to_owned(), shell());
    let mut providers = BTreeMap::new();
    providers.insert("base".to_owned(), provider(Vec::new(), PathMode::Merge));
    providers.insert(
        "project".to_owned(),
        provider(vec!["base".to_owned()], PathMode::Merge),
    );
    ResolvedConfig::new(
        WorkspaceConfig::new(workspace.display().to_string()),
        ShellSelection::new("bash"),
        EnvironmentConfig::new(
            true,
            ConfiguredValuePrecedence::Locked,
            BTreeMap::from([
                ("CONFIGURED".to_owned(), "from-config".to_owned()),
                ("PATH".to_owned(), "/config-base".to_owned()),
            ]),
            BTreeMap::new(),
            EnvironmentPath::new(
                vec!["/configured-first".to_owned()],
                vec![
                    "/configured-last".to_owned(),
                    "/configured-first".to_owned(),
                ],
                vec!["/ambient-removed".to_owned()],
            ),
        ),
    )
    .with_shells(shells)
    .with_providers(providers)
    .with_features(ValueTree::Map(BTreeMap::new()))
}

fn context() -> RuntimeContext {
    let workspace = current_dir().unwrap();
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

impl ProviderSupervisor for FakeRuntime {
    fn prepare(&self, job: &ProviderJob) -> Result<ProviderRunResult, ProviderRuntimeError> {
        let provider_id = job.provider_id();
        self.calls
            .lock()
            .unwrap()
            .push((provider_id.to_owned(), job.context().clone()));
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
    ProviderRunResult::new(ProviderRunResultParts {
        provider: provider.to_owned(),
        sensitivity: Sensitivity::Public,
        detection: DetectionResult {
            executable: Some(PathBuf::from("/fake/provider")),
            matched_files: Vec::new(),
            applicable: true,
        },
        prepared_steps: 1,
        environment: EnvironmentDelta::new(set, BTreeSet::new()),
        diagnostics: Vec::new(),
        receipt: Some(ProviderReceipt {
            provider: provider.to_owned(),
            config_fingerprint: [1; 32],
            workspace_fingerprint: [2; 32],
            version: Some("test".to_owned()),
            completed_at: None,
        }),
    })
}

#[test]
fn materializes_ambient_and_configured_values_with_structured_path_operations() {
    let config = config().with_providers(BTreeMap::new());
    let materializer = Materializer::new(config);
    let output = materializer.materialize(&context()).unwrap();

    assert_eq!(
        output.environment().values()["CONFIGURED"].value(),
        "from-config"
    );
    assert_eq!(output.environment().values()["HOST_ONLY"].value(), "kept");
    assert_eq!(
        output.environment().values()["PATH"].value(),
        "/configured-first:/config-base:/configured-last"
    );
    assert_eq!(output.environment().config_fingerprint().len(), 32);
}

#[test]
fn ambient_precedence_leaves_an_existing_process_value_in_place() {
    let config = config().with_providers(BTreeMap::new());
    let environment = config
        .environment()
        .clone()
        .with_configured_value_precedence(ConfiguredValuePrecedence::Ambient);
    let config = config.with_environment(environment);
    let output = Materializer::new(config).materialize(&context()).unwrap();
    assert_eq!(
        output.environment().values()["CONFIGURED"].value(),
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
    let output = Materializer::with_provider_supervisor(config(), fake)
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
    assert_eq!(calls.lock().unwrap()[1].1.environment()["BASE"], "1");
    assert_eq!(
        output.environment().values()["PROJECT"].value(),
        "sees-base"
    );
    assert_eq!(
        output.environment().values()["PATH"].value(),
        "/provider-base:/configured-first:/config-base:/configured-last"
    );
    assert_eq!(output.environment().provider_receipts().len(), 2);
}

#[test]
fn disabled_provider_is_not_executed_and_is_reported_as_a_typed_diagnostic() {
    let config = config()
        .with_providers(BTreeMap::from([(
            "tool".to_owned(),
            provider(Vec::new(), PathMode::Merge),
        )]))
        .with_features(ValueTree::Map(BTreeMap::from([(
            "tool".to_owned(),
            ValueTree::Map(BTreeMap::from([(
                "enabled".to_owned(),
                ValueTree::Bool(false),
            )])),
        )])));
    let fake = FakeRuntime::new(BTreeMap::new());
    let calls = fake.calls.clone();
    let output = Materializer::with_provider_supervisor(config, fake)
        .materialize(&context())
        .unwrap();

    assert!(calls.lock().unwrap().is_empty());
    assert!(matches!(
        output.diagnostics(),
        [MaterializationDiagnostic::ProviderDisabled { provider }] if provider == "tool"
    ));
}

#[test]
fn provider_failures_keep_the_original_structured_error_as_source() {
    let config = only_provider(config(), "base");
    let fake = FakeRuntime::new(BTreeMap::from([(
        "base".to_owned(),
        Err(ProviderRuntimeError::Model(
            ModelError::MissingDefaultShell {
                shell: "missing".to_owned(),
            },
        )),
    )]));
    let error = Materializer::with_provider_supervisor(config, fake)
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
    assert!(Error::source(&error).is_some());
}

#[test]
fn context_without_shell_uses_the_profile_default() {
    let config = config().with_providers(BTreeMap::new());
    let workspace = current_dir().unwrap();
    let context = RuntimeContext::without_shell(workspace.clone(), workspace, BTreeMap::new());
    let output = Materializer::new(config).materialize(&context).unwrap();
    output.environment().validate().unwrap();
}

#[test]
fn invalid_context_is_rejected_before_provider_execution() {
    let config = config().with_providers(BTreeMap::new());
    let context = RuntimeContext::new("/tmp/workspace", "/tmp/other", "bash", BTreeMap::new());
    let error = Materializer::new(config).materialize(&context).unwrap_err();
    assert!(matches!(error, CoreError::Context(_)));
}

#[test]
fn provider_sensitive_values_remain_sensitive_in_materialized_environment() {
    let base_provider = config()
        .providers()
        .get("base")
        .expect("base provider exists")
        .clone()
        .with_sensitivity(Sensitivity::Secret);
    let config_with_provider_sensitivity =
        config().with_providers(BTreeMap::from([("base".to_owned(), base_provider)]));
    let fake = FakeRuntime::new(BTreeMap::from([(
        "base".to_owned(),
        Ok(result("base", None, ("TOKEN", "secret"))),
    )]));
    let output = Materializer::with_provider_supervisor(config_with_provider_sensitivity, fake)
        .materialize(&context())
        .unwrap();
    assert_eq!(
        output.environment().values()["TOKEN"].sensitivity(),
        Sensitivity::Public
    );
    // The runtime result controls output sensitivity; the provider config is
    // intentionally not consulted a second time at the model boundary.
    let config = only_provider(config(), "base");
    let fake = FakeRuntime::new(BTreeMap::from([(
        "base".to_owned(),
        Ok(result("base", None, ("TOKEN", "secret")).with_sensitivity(Sensitivity::Secret)),
    )]));
    let output = Materializer::with_provider_supervisor(config, fake)
        .materialize(&context())
        .unwrap();
    assert_eq!(
        output.environment().values()["TOKEN"].sensitivity(),
        Sensitivity::Secret
    );
}

#[test]
fn config_provenance_is_attached_to_configured_environment_values() {
    let config = config().with_providers(BTreeMap::new());
    let mut provenance = config.provenance().clone();
    provenance.insert(
        "environment.variables.CONFIGURED",
        Origin::profile("base"),
        Sensitivity::Public,
    );
    let config = config.with_provenance(provenance);
    let output = Materializer::new(config).materialize(&context()).unwrap();
    assert_eq!(
        output.environment().values()["CONFIGURED"].origin(),
        Some(&Origin::profile("base"))
    );
}

#[test]
fn conditional_environment_variables_set_and_remove_sccache_wrapper() {
    let config = config().with_providers(BTreeMap::new());
    let environment = config
        .environment()
        .clone()
        .with_conditional_variable(
            "RUSTC_WRAPPER",
            ConditionalEnvironmentVariable::new(
                "sccache",
                "features.sccache.enabled && !features.sccache.disabled",
            ),
        )
        .with_conditional_variable(
            "CARGO_INCREMENTAL",
            ConditionalEnvironmentVariable::new(
                "0",
                "!features.sccache.enabled || features.sccache.disabled",
            ),
        );
    let mut provenance = config.provenance().clone();
    provenance.insert(
        "environment.conditional_variables.RUSTC_WRAPPER",
        Origin::profile("rust"),
        Sensitivity::Public,
    );
    let config = config
        .with_environment(environment)
        .with_provenance(provenance)
        .with_features(ValueTree::Map(BTreeMap::from([(
            "sccache".to_owned(),
            ValueTree::Map(BTreeMap::from([
                ("enabled".to_owned(), ValueTree::Bool(true)),
                ("disabled".to_owned(), ValueTree::Bool(false)),
            ])),
        )])));

    let mut enabled_context = context();
    enabled_context
        .process_environment_mut()
        .insert("RUSTC_WRAPPER".to_owned(), "ambient-wrapper".to_owned());
    enabled_context
        .process_environment_mut()
        .insert("CARGO_INCREMENTAL".to_owned(), "1".to_owned());
    let output = Materializer::new(config.clone())
        .materialize(&enabled_context)
        .unwrap();
    assert_eq!(
        output.environment().values()["RUSTC_WRAPPER"].value(),
        "sccache"
    );
    assert!(!output
        .environment()
        .values()
        .contains_key("CARGO_INCREMENTAL"));
    assert_eq!(
        output.environment().values()["RUSTC_WRAPPER"].origin(),
        Some(&Origin::profile("rust"))
    );

    let config = config.with_features(ValueTree::Map(BTreeMap::from([(
        "sccache".to_owned(),
        ValueTree::Map(BTreeMap::from([
            ("enabled".to_owned(), ValueTree::Bool(true)),
            ("disabled".to_owned(), ValueTree::Bool(true)),
        ])),
    )])));
    let output = Materializer::new(config)
        .materialize(&enabled_context)
        .unwrap();
    assert!(!output.environment().values().contains_key("RUSTC_WRAPPER"));
    assert_eq!(
        output.environment().values()["CARGO_INCREMENTAL"].value(),
        "0"
    );
}
