use bootstrap_model::{
    Action, ActionKind, BootstrapConfig, BootstrapInput, BootstrapMode, BootstrapPolicy,
    HandoffConfig, IdentityConfig, InputType, NonInteractivePolicy, Origin, RunAs,
    BOOTSTRAP_SCHEMA_V1,
};
use serde_json::Value;
use std::collections::BTreeMap;

fn minimal_config(actions: Vec<Action>) -> BootstrapConfig {
    BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_V1,
        mode: BootstrapMode::Strict,
        workspace_root: "/workspace".to_owned(),
        allow_workspace_overlay: false,
        non_interactive: NonInteractivePolicy::Deny,
        identity: IdentityConfig {
            default_user: Some("dev".to_owned()),
            default_uid: Some(1000),
            default_gid: Some(1000),
            auto_mapping: true,
            run_as_root_input: Some("RUN_AS_ROOT".to_owned()),
            uid_input: Some("HOST_UID".to_owned()),
            gid_input: Some("HOST_GID".to_owned()),
            home_input: Some("CONTAINER_HOME".to_owned()),
        },
        handoff: HandoffConfig {
            runtime: "/usr/bin/runtime".to_owned(),
            exec_prefix: vec!["exec".to_owned(), "--".to_owned()],
            shell_prefix: vec!["shell".to_owned()],
            ssh_daemon: None,
            login_shell: None,
        },
        policy: BootstrapPolicy::default(),
        inputs: declared_inputs(),
        actions,
    }
}

fn declared_inputs() -> BTreeMap<String, BootstrapInput> {
    [
        ("RUN_AS_ROOT", "identity.run_as_root", InputType::Bool),
        ("HOST_UID", "identity.uid", InputType::UidPair),
        ("HOST_GID", "identity.gid", InputType::Gid),
        ("CONTAINER_HOME", "identity.home", InputType::Path),
    ]
    .into_iter()
    .map(|(name, target, input_type)| {
        (
            name.to_owned(),
            BootstrapInput {
                target: target.to_owned(),
                input_type,
                aliases: Vec::new(),
                runtime: true,
                format: None,
                namespace: (input_type == InputType::UidPair || input_type == InputType::Gid)
                    .then_some(bootstrap_model::InputNamespace::Host),
                default: None,
                allow_outside_workspace: input_type == InputType::Path,
            },
        )
    })
    .collect()
}

#[test]
fn flat_dsl_action_model_keeps_provider_concerns_out() {
    let mut resolve = Action::new(
        "resolve",
        ActionKind::IdentityResolve,
        Origin::image("base"),
    );
    resolve.run_as = RunAs::Root;
    let mut directory = Action::new(
        "workspace",
        ActionKind::FilesystemEnsureDir,
        Origin::image("base"),
    );
    directory.path = Some("${bootstrap.workspace_root}/.state".to_owned());
    directory.run_as = RunAs::Root;

    let config = minimal_config(vec![directory, resolve]);
    let plan = config
        .build_plan()
        .expect("model should build a static plan");
    assert_eq!(plan.ids().collect::<Vec<_>>(), vec!["resolve", "workspace"]);
}

#[test]
fn root_action_from_workspace_reports_provenance_and_remediation_context() {
    let mut action = Action::new(
        "unsafe-root",
        ActionKind::FilesystemEnsureDir,
        Origin::workspace("project-overlay"),
    );
    action.path = Some("/etc/project".to_owned());
    action.run_as = RunAs::Root;

    let error = minimal_config(vec![action])
        .build_plan()
        .expect_err("workspace must not gain root actions");
    let rendered = error.to_string();
    assert!(rendered.contains("unsafe-root"));
    assert!(rendered.contains("WorkspaceOverlay"));
    assert!(rendered.contains("root actions require"));
}

#[test]
fn flat_action_json_omits_loader_metadata_while_plan_json_keeps_provenance() {
    let mut action = Action::new(
        "directory",
        ActionKind::FilesystemEnsureDir,
        Origin::image("base"),
    );
    action.path = Some("/var/lib/app".to_owned());
    action.parent_mode = Some("0755".to_owned());
    action.run_as = RunAs::Root;

    let action_json = serde_json::to_value(&action).expect("action should be JSON serializable");
    assert!(action_json.get("origin").is_none());

    let plan = minimal_config(vec![action])
        .build_plan()
        .expect("plan should serialize");
    let json = serde_json::to_value(plan).expect("plan should be JSON serializable");
    assert_eq!(
        json["actions"][0]["id"],
        Value::String("directory".to_owned())
    );
    assert_eq!(
        json["actions"][0]["kind"],
        Value::String("filesystem_ensure_dir".to_owned())
    );
    assert_eq!(
        json["actions"][0]["effect"]["filesystem_ensure_dir"]["path"],
        Value::String("/var/lib/app".to_owned())
    );
    assert!(json["actions"][0].get("origin").is_some());
    assert_eq!(json["actions"][0]["origin"]["source"], "image_profile");
}
