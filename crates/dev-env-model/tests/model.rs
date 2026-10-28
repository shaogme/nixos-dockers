use dev_env_model::{
    Condition, ConditionalEnvironmentVariable, ConfiguredValuePrecedence, EnvValue,
    EnvironmentConfig, EnvironmentPath, FailurePolicy, InputError, InputSpec, InputType,
    InputValue, Layer, MaterializedEnv, MergePolicy, ModelError, Origin, OverrideOperation,
    OverrideSpec, PathMode, PathRenderError, PathTemplate, PolicyConfig, PrepareStep,
    ProfileDocument, ProfileSet, ProviderConfig, ResolvedConfig, Sensitivity, ShellConfig,
    ShellEnvEntry, ShellEnvFormat, ShellKind, ShellSelection, UntrustedWorkspacePolicy, ValueTree,
    WorkspaceConfig,
};
use serde_json::to_value;
use std::collections::BTreeMap;
use toml::from_str;

fn shell(command: &str) -> ShellConfig {
    ShellConfig::new(
        command,
        ShellKind::Posix,
        vec!["-l".to_owned()],
        vec!["-i".to_owned()],
        Some("-c".to_owned()),
    )
}

fn provider(depends_on: Vec<String>) -> ProviderConfig {
    ProviderConfig::new(
        "fake-provider",
        vec!["devbox.json".to_owned()],
        dev_env_model::MissingProviderPolicy::Ignore,
        depends_on,
        vec![PrepareStep::new(
            vec!["install".to_owned()],
            Some("workspace.config-present && features.devbox.auto_init != 'never'".to_owned()),
            FailurePolicy::Warn,
            Some(1_000),
            Sensitivity::Public,
        )],
    )
    .with_shellenv(dev_env_model::ShellEnvConfig::new(
        vec!["shellenv".to_owned(), "{shell}".to_owned()],
        ShellEnvFormat::Shell,
        FailurePolicy::Error,
        PathMode::Merge,
        Some(1_000),
    ))
}

fn resolved_config() -> ResolvedConfig {
    let mut shells = BTreeMap::new();
    shells.insert("bash".to_owned(), shell("/bin/bash.real"));
    let mut providers = BTreeMap::new();
    providers.insert("base".to_owned(), provider(Vec::new()));
    providers.insert("project".to_owned(), provider(vec!["base".to_owned()]));
    ResolvedConfig::new(
        WorkspaceConfig::new("/workspace"),
        ShellSelection::new("bash"),
        EnvironmentConfig::new(
            true,
            ConfiguredValuePrecedence::Locked,
            BTreeMap::from([("XDG_DATA_HOME".to_owned(), "/data".to_owned())]),
            BTreeMap::new(),
            EnvironmentPath::new(
                vec!["/opt/bin".to_owned()],
                vec!["/usr/local/bin".to_owned()],
                Vec::new(),
            ),
        ),
    )
    .with_shells(shells)
    .with_providers(providers)
    .with_features(ValueTree::Map(BTreeMap::from([(
        "devbox".to_owned(),
        ValueTree::Map(BTreeMap::from([(
            "auto_init".to_owned(),
            ValueTree::String("never".to_owned()),
        )])),
    )])))
}

#[test]
fn schema_sample_deserializes_and_validates_as_a_profile_document() {
    let source = r#"
schema = 1
id = "coding-images"
extends = ["nixos-docker"]

[policy]
merge = "strict"
workspace_can_override = ["features.devbox.auto_init", "environment.variables.*", "shell.default"]
unknown_input = "error"
untrusted_workspace = "prompt"

[config.workspace]
root = "/workspace"
search = "upward"

[config.shell]
default = "bash"

[config.shells.bash]
command = "/bin/bash.real"
kind = "posix"
login_args = ["-l"]
interactive_args = ["-i"]
command_arg = "-c"

[config.environment]
inherit_process = true
configured_value_precedence = "locked"

[config.environment.variables]
XDG_DATA_HOME = "/data"

[config.environment.path]
prepend = ["/usr/local/share/mise/shims"]
append = ["/usr/local/bin"]

[config.features.devbox]
enabled = true
auto_init = "never"

[config.providers.devbox]
executable = "devbox"
detect_files = ["devbox.json"]
missing = "ignore"
shellenv = { argv = ["shellenv", "--init-hook"], format = "shell", failure = "warn" }

[[config.providers.devbox.prepare]]
argv = ["install"]
when = "workspace.config-present"
failure = "warn"

[inputs.DEVBOX_AUTO_INIT]
target = "features.devbox.auto_init"
type = "enum"
values = ["never", "if-missing", "ask"]
aliases = { "0" = "never", "1" = "if-missing", "true" = "if-missing" }
runtime = true
export_as = "DEVBOX_AUTO_INIT"
"#;
    let document: ProfileDocument = from_str(source).expect("schema sample should parse");
    document.validate().expect("schema sample should validate");
    assert_eq!(document.id(), "coding-images");
    assert_eq!(
        document.config().shell().as_ref().unwrap().default(),
        "bash"
    );
    assert_eq!(
        document.inputs()["DEVBOX_AUTO_INIT"].values()[1],
        "if-missing"
    );
}

#[test]
fn schema_rejects_unknown_fields_at_the_typed_boundary() {
    let result: Result<ProfileDocument, _> = from_str(
        r#"
schema = 1
id = "base"

[config]
not_a_config_field = true
"#,
    );
    assert!(result.is_err());
}

#[test]
fn profile_set_resolves_dfs_and_rejects_missing_and_cyclic_parents() {
    let base = ProfileDocument::new("base");
    let child = ProfileDocument::new("child").with_extends(vec!["base".to_owned()]);
    let set = ProfileSet::new(vec![child.clone(), base.clone()], "child").unwrap();
    assert_eq!(
        set.chain()
            .unwrap()
            .iter()
            .map(|item| item.id())
            .collect::<Vec<_>>(),
        vec!["base", "child"]
    );

    let missing = child.clone().with_extends(vec!["absent".to_owned()]);
    assert!(matches!(
        ProfileSet::new(vec![missing], "child"),
        Err(ModelError::MissingParentProfile { .. })
    ));

    let cycle_a = base.clone().with_id("a").with_extends(vec!["b".to_owned()]);
    let cycle_b = base.with_id("b").with_extends(vec!["a".to_owned()]);
    assert!(matches!(
        ProfileSet::new(vec![cycle_a, cycle_b], "a"),
        Err(ModelError::ProfileCycle { .. })
    ));
}

#[test]
fn resolved_config_validates_shells_and_orders_provider_dependencies() {
    let config = resolved_config();
    config.validate().unwrap();
    assert_eq!(config.provider_order().unwrap(), vec!["base", "project"]);

    let invalid = config.clone().with_shell(ShellSelection::new("missing"));
    assert!(matches!(
        invalid.validate(),
        Err(ModelError::MissingDefaultShell { .. })
    ));

    let config = resolved_config();
    let providers = config
        .providers()
        .iter()
        .map(|(id, provider)| {
            let provider = if id == "base" {
                provider.clone().with_depends_on(vec!["project".to_owned()])
            } else {
                provider.clone()
            };
            (id.clone(), provider)
        })
        .collect();
    let cyclic = config.with_providers(providers);
    assert!(matches!(
        cyclic.provider_order(),
        Err(ModelError::ProviderDependencyCycle { .. })
    ));
}

#[test]
fn typed_inputs_apply_aliases_without_special_casing_devbox() {
    let input = InputSpec::new("features.devbox.auto_init", InputType::Enum)
        .with_values(vec![
            "never".to_owned(),
            "if-missing".to_owned(),
            "ask".to_owned(),
        ])
        .with_aliases(BTreeMap::from([
            ("0".to_owned(), "never".to_owned()),
            ("1".to_owned(), "if-missing".to_owned()),
            ("true".to_owned(), "if-missing".to_owned()),
        ]))
        .with_runtime(true)
        .with_export_as("DEVBOX_AUTO_INIT")
        .with_default(InputValue::String("never".to_owned()));
    input.validate("DEVBOX_AUTO_INIT").unwrap();
    assert_eq!(
        input.parse_runtime_value("1").unwrap().as_str(),
        Some("if-missing")
    );
    assert!(matches!(
        input.parse_runtime_value("other"),
        Err(ModelError::InvalidInput {
            reason: InputError::InvalidEnumValue { .. },
            ..
        })
    ));

    let integer =
        InputSpec::new("limits.jobs", InputType::Integer).with_default(InputValue::Integer(4));
    assert_eq!(
        integer.parse_value("4").unwrap(),
        dev_env_model::ParsedInput::Integer(4)
    );
    assert!(matches!(
        integer.parse_runtime_value("4"),
        Err(ModelError::InvalidInput {
            reason: InputError::NotRuntime,
            ..
        })
    ));
}

#[test]
fn path_and_path_operations_are_safe_and_deterministic() {
    let template = PathTemplate::new("${workspace.root}/.cache").unwrap();
    let values = BTreeMap::from([("workspace.root".to_owned(), "/workspace".to_owned())]);
    assert_eq!(template.render(&values).unwrap(), "/workspace/.cache");
    assert!(matches!(
        template.render(&BTreeMap::new()),
        Err(PathRenderError::MissingValue { .. })
    ));
    assert!(matches!(
        PathTemplate::new("${workspace.root}/../escape"),
        Err(ModelError::InvalidPath { .. })
    ));

    let path = EnvironmentPath::new(
        vec!["/a".to_owned(), "/b".to_owned(), "/a".to_owned()],
        vec!["/b".to_owned(), "/c".to_owned()],
        vec!["/b".to_owned()],
    );
    let resolved = path
        .resolve(&["/base".to_owned(), "/a".to_owned()])
        .unwrap();
    assert_eq!(resolved, vec!["/a", "/base", "/c"]);
}

#[test]
fn restricted_condition_and_shellenv_parsers_never_execute_shell_syntax() {
    assert_eq!(
        Condition::parse("workspace.config-present && features.devbox.auto_init != 'never'")
            .unwrap(),
        Condition::And(vec![
            Condition::Reference("workspace.config-present".to_owned()),
            Condition::NotEqual(
                dev_env_model::ConditionValue::Reference("features.devbox.auto_init".to_owned()),
                dev_env_model::ConditionValue::Literal("never".to_owned()),
            ),
        ])
    );
    assert!(Condition::parse("$(touch /tmp/pwned)").is_err());
    assert!(Condition::parse("workspace.writable; rm -rf /").is_err());

    assert_eq!(
        ShellEnvEntry::parse("export FOO='hello world'\nBAR=plain\nunset OLD\n").unwrap(),
        vec![
            ShellEnvEntry::Set {
                name: "FOO".to_owned(),
                value: "hello world".to_owned(),
            },
            ShellEnvEntry::Set {
                name: "BAR".to_owned(),
                value: "plain".to_owned(),
            },
            ShellEnvEntry::Unset {
                name: "OLD".to_owned(),
            },
        ]
    );
    assert_eq!(
        ShellEnvEntry::parse(
            "export AR=\"ar\";\nDEVBOX_NIX_ENV_PATH_abcdef='/opt/bin';\nhash -r\n",
        )
        .unwrap(),
        vec![
            ShellEnvEntry::Set {
                name: "AR".to_owned(),
                value: "ar".to_owned(),
            },
            ShellEnvEntry::Set {
                name: "DEVBOX_NIX_ENV_PATH_abcdef".to_owned(),
                value: "/opt/bin".to_owned(),
            },
        ]
    );
    assert_eq!(
        ShellEnvEntry::parse("export BUILD_PHASE=\"line one;\nline two\";\n").unwrap(),
        vec![ShellEnvEntry::Set {
            name: "BUILD_PHASE".to_owned(),
            value: "line one;\nline two".to_owned(),
        }]
    );
    for output in [
        "export FOO=$(touch /tmp/pwned)",
        "FOO=ok; touch /tmp/pwned",
        "FOO=`id`",
        "function evil() { true; }",
    ] {
        assert!(ShellEnvEntry::parse(output).is_err(), "accepted {output:?}");
    }
}

#[test]
fn materialized_environment_keeps_provenance_and_sensitivity_structured() {
    let mut values = BTreeMap::new();
    values.insert(
        "TOKEN".to_owned(),
        EnvValue::new(
            "secret-value",
            Some(Origin::environment("TOKEN")),
            Sensitivity::Secret,
            Some("vault".to_owned()),
        ),
    );
    let environment = MaterializedEnv::new(values, [7; 32]);
    environment.validate().unwrap();
    assert_eq!(
        environment.get("TOKEN").unwrap().sensitivity(),
        Sensitivity::Secret
    );
    let json = to_value(&environment).unwrap();
    assert_eq!(json["values"]["TOKEN"]["sensitivity"], "secret");
    assert_eq!(
        json["values"]["TOKEN"]["origin"]["source"]["kind"],
        "environment"
    );
}

#[test]
fn policy_patterns_match_only_the_declared_namespace() {
    let policy = PolicyConfig::default()
        .with_workspace_can_override(vec!["environment.variables.*".to_owned()])
        .with_untrusted_workspace(UntrustedWorkspacePolicy::Deny);
    assert!(policy.allows_workspace_override("environment.variables.CARGO_HOME"));
    assert!(!policy.allows_workspace_override("environment.path.prepend"));
    assert!(!policy.allows_workspace_override("environment.variables"));
}

#[test]
fn invalid_structured_values_are_reported_as_typed_errors() {
    let env = EnvironmentConfig::default().with_variables(BTreeMap::from([(
        "bad-name".to_owned(),
        "value".to_owned(),
    )]));
    assert!(matches!(
        env.validate(),
        Err(ModelError::InvalidEnvironmentName { .. })
    ));

    let mut spec = InputSpec::new("features.mode", InputType::Enum)
        .with_values(vec!["one".to_owned()])
        .with_aliases(BTreeMap::from([("x".to_owned(), "missing".to_owned())]))
        .with_runtime(true);
    assert!(matches!(
        spec.validate("MODE"),
        Err(ModelError::InvalidInput {
            reason: InputError::AliasTargetMissing { .. },
            ..
        })
    ));
    spec = spec.with_aliases(BTreeMap::new());
    assert!(spec.validate("mode").is_err());
}

#[test]
fn conditional_environment_variables_validate_conditions_and_names() {
    let mut environment =
        EnvironmentConfig::default().with_conditional_variables(BTreeMap::from([(
            "RUSTC_WRAPPER".to_owned(),
            ConditionalEnvironmentVariable::new(
                "sccache",
                "features.sccache.enabled && !features.sccache.disabled",
            ),
        )]));
    environment.validate().unwrap();

    environment = environment.with_variables(BTreeMap::from([(
        "RUSTC_WRAPPER".to_owned(),
        "other".to_owned(),
    )]));
    assert!(matches!(
        environment.validate(),
        Err(ModelError::InvalidValue {
            reason: dev_env_model::ModelErrorReason::Duplicate,
            ..
        })
    ));
}

#[test]
fn explicit_overrides_and_trust_policy_are_validated_without_string_errors() {
    let valid = OverrideSpec::new(OverrideOperation::Set, "repository policy")
        .with_value(ValueTree::String("if-missing".to_owned()));
    valid.validate("features.devbox.auto_init").unwrap();

    let missing_reason = OverrideSpec::new(OverrideOperation::Set, "")
        .with_value(ValueTree::String("if-missing".to_owned()));
    assert!(matches!(
        missing_reason.validate("features.devbox.auto_init"),
        Err(ModelError::InvalidOverride { .. })
    ));

    let bad_path = OverrideSpec::new(OverrideOperation::Append, "bad path")
        .with_values(vec![ValueTree::String("relative/bin".to_owned())]);
    assert!(matches!(
        bad_path.validate("environment.path.append"),
        Err(ModelError::InvalidPath { .. })
    ));

    let policy = PolicyConfig::default().with_merge(MergePolicy::PreferChild);
    policy.validate_for_layer(Layer::Profile).unwrap();
    assert!(matches!(
        policy.validate_for_layer(Layer::Workspace),
        Err(ModelError::PreferChildPolicyNotTrusted)
    ));
}
