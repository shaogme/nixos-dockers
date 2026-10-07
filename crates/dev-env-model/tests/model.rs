use dev_env_model::{
    Condition, ConditionalEnvironmentVariable, ConfiguredValuePrecedence, EnvValue,
    EnvironmentConfig, EnvironmentPath, FailurePolicy, InputError, InputSpec, InputType,
    InputValue, Layer, MaterializedEnv, MergePolicy, ModelError, Origin, OverrideOperation,
    OverrideSpec, PathMode, PathRenderError, PathTemplate, PolicyConfig, PrepareStep,
    ProfileDocument, ProfileSet, ProviderConfig, ResolvedConfig, Sensitivity, ShellConfig,
    ShellEnvEntry, ShellEnvFormat, ShellKind, ShellSelection, UntrustedWorkspacePolicy, ValueTree,
    WorkspaceConfig, WorkspaceSearch, DEV_ENV_SCHEMA_V1,
};
use std::collections::BTreeMap;

fn shell(command: &str) -> ShellConfig {
    ShellConfig {
        command: command.to_owned(),
        kind: ShellKind::Posix,
        login_args: vec!["-l".to_owned()],
        interactive_args: vec!["-i".to_owned()],
        command_arg: Some("-c".to_owned()),
    }
}

fn provider(depends_on: Vec<String>) -> ProviderConfig {
    ProviderConfig {
        executable: "fake-provider".to_owned(),
        detect_files: vec!["devbox.json".to_owned()],
        missing: dev_env_model::MissingProviderPolicy::Ignore,
        depends_on,
        prepare: vec![PrepareStep {
            argv: vec!["install".to_owned()],
            when: Some(
                "workspace.config-present && features.devbox.auto_init != 'never'".to_owned(),
            ),
            failure: FailurePolicy::Warn,
            timeout_ms: Some(1_000),
            sensitivity: Sensitivity::Public,
        }],
        shellenv: Some(dev_env_model::ShellEnvConfig {
            argv: vec!["shellenv".to_owned(), "{shell}".to_owned()],
            format: ShellEnvFormat::Shell,
            failure: FailurePolicy::Error,
            path_mode: PathMode::Merge,
            timeout_ms: Some(1_000),
        }),
        sensitivity: Sensitivity::Public,
    }
}

fn resolved_config() -> ResolvedConfig {
    let mut shells = BTreeMap::new();
    shells.insert("bash".to_owned(), shell("/bin/bash.real"));
    let mut providers = BTreeMap::new();
    providers.insert("base".to_owned(), provider(Vec::new()));
    providers.insert("project".to_owned(), provider(vec!["base".to_owned()]));
    ResolvedConfig {
        workspace: WorkspaceConfig {
            root: "/workspace".to_owned(),
            search: WorkspaceSearch::Upward,
        },
        shell: ShellSelection {
            default: "bash".to_owned(),
        },
        shells,
        environment: EnvironmentConfig {
            inherit_process: true,
            configured_value_precedence: ConfiguredValuePrecedence::Locked,
            variables: BTreeMap::from([("XDG_DATA_HOME".to_owned(), "/data".to_owned())]),
            conditional_variables: BTreeMap::new(),
            path: EnvironmentPath {
                prepend: vec!["/opt/bin".to_owned()],
                append: vec!["/usr/local/bin".to_owned()],
                remove: Vec::new(),
            },
        },
        features: ValueTree::Map(BTreeMap::from([(
            "devbox".to_owned(),
            ValueTree::Map(BTreeMap::from([(
                "auto_init".to_owned(),
                ValueTree::String("never".to_owned()),
            )])),
        )])),
        providers,
        inputs: BTreeMap::new(),
        policy: PolicyConfig::default(),
        provenance: Default::default(),
    }
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
    let document: ProfileDocument = toml::from_str(source).expect("schema sample should parse");
    document.validate().expect("schema sample should validate");
    assert_eq!(document.id, "coding-images");
    assert_eq!(document.config.shell.as_ref().unwrap().default, "bash");
    assert_eq!(document.inputs["DEVBOX_AUTO_INIT"].values[1], "if-missing");
}

#[test]
fn schema_rejects_unknown_fields_at_the_typed_boundary() {
    let result: Result<ProfileDocument, _> = toml::from_str(
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
    let base = ProfileDocument {
        schema: DEV_ENV_SCHEMA_V1,
        id: "base".to_owned(),
        extends: Vec::new(),
        policy: PolicyConfig::default(),
        config: Default::default(),
        overrides: BTreeMap::new(),
        inputs: BTreeMap::new(),
    };
    let child = ProfileDocument {
        schema: DEV_ENV_SCHEMA_V1,
        id: "child".to_owned(),
        extends: vec!["base".to_owned()],
        ..base.clone()
    };
    let set = ProfileSet::new(vec![child.clone(), base.clone()], "child").unwrap();
    assert_eq!(
        set.chain()
            .unwrap()
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        vec!["base", "child"]
    );

    let missing = ProfileDocument {
        extends: vec!["absent".to_owned()],
        ..child.clone()
    };
    assert!(matches!(
        ProfileSet::new(vec![missing], "child"),
        Err(ModelError::MissingParentProfile { .. })
    ));

    let mut cycle_a = base.clone();
    cycle_a.id = "a".to_owned();
    cycle_a.extends = vec!["b".to_owned()];
    let mut cycle_b = base;
    cycle_b.id = "b".to_owned();
    cycle_b.extends = vec!["a".to_owned()];
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

    let mut invalid = config.clone();
    invalid.shell.default = "missing".to_owned();
    assert!(matches!(
        invalid.validate(),
        Err(ModelError::MissingDefaultShell { .. })
    ));

    let mut cyclic = resolved_config();
    cyclic.providers.get_mut("base").unwrap().depends_on = vec!["project".to_owned()];
    assert!(matches!(
        cyclic.provider_order(),
        Err(ModelError::ProviderDependencyCycle { .. })
    ));
}

#[test]
fn typed_inputs_apply_aliases_without_special_casing_devbox() {
    let input = InputSpec {
        target: "features.devbox.auto_init".to_owned(),
        input_type: InputType::Enum,
        values: vec![
            "never".to_owned(),
            "if-missing".to_owned(),
            "ask".to_owned(),
        ],
        aliases: BTreeMap::from([
            ("0".to_owned(), "never".to_owned()),
            ("1".to_owned(), "if-missing".to_owned()),
            ("true".to_owned(), "if-missing".to_owned()),
        ]),
        runtime: true,
        export_as: Some("DEVBOX_AUTO_INIT".to_owned()),
        default: Some(InputValue::String("never".to_owned())),
        sensitivity: Sensitivity::Public,
    };
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

    let integer = InputSpec {
        target: "limits.jobs".to_owned(),
        input_type: InputType::Integer,
        values: Vec::new(),
        aliases: BTreeMap::new(),
        runtime: false,
        export_as: None,
        default: Some(InputValue::Integer(4)),
        sensitivity: Sensitivity::Public,
    };
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

    let path = EnvironmentPath {
        prepend: vec!["/a".to_owned(), "/b".to_owned(), "/a".to_owned()],
        append: vec!["/b".to_owned(), "/c".to_owned()],
        remove: vec!["/b".to_owned()],
    };
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
        EnvValue {
            value: "secret-value".to_owned(),
            origin: Some(Origin::environment("TOKEN")),
            sensitivity: Sensitivity::Secret,
            provider: Some("vault".to_owned()),
        },
    );
    let environment = MaterializedEnv::new(values, [7; 32]);
    environment.validate().unwrap();
    assert_eq!(
        environment.get("TOKEN").unwrap().sensitivity,
        Sensitivity::Secret
    );
    let json = serde_json::to_value(&environment).unwrap();
    assert_eq!(json["values"]["TOKEN"]["sensitivity"], "secret");
    assert_eq!(
        json["values"]["TOKEN"]["origin"]["source"]["kind"],
        "environment"
    );
}

#[test]
fn policy_patterns_match_only_the_declared_namespace() {
    let policy = PolicyConfig {
        workspace_can_override: vec!["environment.variables.*".to_owned()],
        untrusted_workspace: UntrustedWorkspacePolicy::Deny,
        ..PolicyConfig::default()
    };
    assert!(policy.allows_workspace_override("environment.variables.CARGO_HOME"));
    assert!(!policy.allows_workspace_override("environment.path.prepend"));
    assert!(!policy.allows_workspace_override("environment.variables"));
}

#[test]
fn invalid_structured_values_are_reported_as_typed_errors() {
    let mut env = EnvironmentConfig::default();
    env.variables
        .insert("bad-name".to_owned(), "value".to_owned());
    assert!(matches!(
        env.validate(),
        Err(ModelError::InvalidEnvironmentName { .. })
    ));

    let mut spec = InputSpec {
        target: "features.mode".to_owned(),
        input_type: InputType::Enum,
        values: vec!["one".to_owned()],
        aliases: BTreeMap::from([("x".to_owned(), "missing".to_owned())]),
        runtime: true,
        export_as: None,
        default: None,
        sensitivity: Sensitivity::Public,
    };
    assert!(matches!(
        spec.validate("MODE"),
        Err(ModelError::InvalidInput {
            reason: InputError::AliasTargetMissing { .. },
            ..
        })
    ));
    spec.aliases.clear();
    assert!(spec.validate("mode").is_err());
}

#[test]
fn conditional_environment_variables_validate_conditions_and_names() {
    let mut environment = EnvironmentConfig::default();
    environment.conditional_variables.insert(
        "RUSTC_WRAPPER".to_owned(),
        ConditionalEnvironmentVariable {
            value: "sccache".to_owned(),
            when: "features.sccache.enabled && !features.sccache.disabled".to_owned(),
        },
    );
    environment.validate().unwrap();

    environment
        .variables
        .insert("RUSTC_WRAPPER".to_owned(), "other".to_owned());
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
    let valid = OverrideSpec {
        op: OverrideOperation::Set,
        value: Some(ValueTree::String("if-missing".to_owned())),
        values: Vec::new(),
        reason: "repository policy".to_owned(),
    };
    valid.validate("features.devbox.auto_init").unwrap();

    let missing_reason = OverrideSpec {
        reason: String::new(),
        ..valid.clone()
    };
    assert!(matches!(
        missing_reason.validate("features.devbox.auto_init"),
        Err(ModelError::InvalidOverride { .. })
    ));

    let bad_path = OverrideSpec {
        op: OverrideOperation::Append,
        value: None,
        values: vec![ValueTree::String("relative/bin".to_owned())],
        reason: "bad path".to_owned(),
    };
    assert!(matches!(
        bad_path.validate("environment.path.append"),
        Err(ModelError::InvalidPath { .. })
    ));

    let policy = PolicyConfig {
        merge: MergePolicy::PreferChild,
        ..PolicyConfig::default()
    };
    policy.validate_for_layer(Layer::Profile).unwrap();
    assert!(matches!(
        policy.validate_for_layer(Layer::Workspace),
        Err(ModelError::PreferChildPolicyNotTrusted)
    ));
}
