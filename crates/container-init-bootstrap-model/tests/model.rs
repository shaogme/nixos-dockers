use container_init_bootstrap_model::{
    Action, ActionKind, BootstrapConfig, BootstrapInput, HandoffConfig, IdentityConfig,
    InputNamespace, InputType, ModelError, Origin, RunAs,
};
use serde_json::{to_value, Value};
use std::collections::BTreeMap;

fn minimal_config(actions: Vec<Action>) -> BootstrapConfig {
    let mut identity = IdentityConfig::new();
    identity.set_default_user(Some("dev".to_owned()));
    identity.set_default_uid(Some(1000));
    identity.set_default_gid(Some(1000));
    identity.set_auto_mapping(true);
    identity.set_run_as_root_input(Some("RUN_AS_ROOT".to_owned()));
    identity.set_uid_input(Some("HOST_UID".to_owned()));
    identity.set_gid_input(Some("HOST_GID".to_owned()));
    identity.set_home_input(Some("CONTAINER_HOME".to_owned()));
    let mut handoff = HandoffConfig::new("/usr/bin/runtime");
    handoff.set_exec_prefix(vec!["exec".to_owned(), "--".to_owned()]);
    handoff.set_initial_exec_prefix(vec!["exec".to_owned(), "--".to_owned()]);
    handoff.set_shell_prefix(vec!["shell".to_owned()]);
    BootstrapConfig::new("/workspace", identity, handoff, declared_inputs(), actions)
}

fn declared_inputs() -> BTreeMap<String, BootstrapInput> {
    [
        (
            "RUN_AS_ROOT",
            input("identity.run_as_root", InputType::Bool),
        ),
        ("HOST_UID", input("identity.uid", InputType::UidPair)),
        ("HOST_GID", input("identity.gid", InputType::Gid)),
        ("CONTAINER_HOME", input("identity.home", InputType::Path)),
    ]
    .into_iter()
    .map(|(name, input)| (name.to_owned(), input))
    .collect()
}

fn input(target: &str, input_type: InputType) -> BootstrapInput {
    let mut input = BootstrapInput::new(target, input_type);
    input.set_runtime(true);
    input.set_namespace(
        (input_type == InputType::UidPair || input_type == InputType::Gid)
            .then_some(InputNamespace::Host),
    );
    input.set_allow_outside_workspace(input_type == InputType::Path);
    input
}

#[test]
fn flat_dsl_action_model_keeps_provider_concerns_out() {
    let mut resolve = Action::new(
        "resolve",
        ActionKind::IdentityResolve,
        Origin::image("base"),
    );
    resolve.set_run_as(RunAs::Root);
    let mut directory = Action::new(
        "workspace",
        ActionKind::FilesystemEnsureDir,
        Origin::image("base"),
    );
    directory.set_path(Some("${bootstrap.workspace_root}/.state".to_owned()));
    directory.set_run_as(RunAs::Root);

    let config = minimal_config(vec![directory, resolve]);
    let plan = config
        .build_plan()
        .expect("model should build a static plan");
    assert_eq!(plan.ids().collect::<Vec<_>>(), vec!["resolve", "workspace"]);
}

#[test]
fn handoff_prefixes_and_action_count_obey_resource_budgets() {
    let mut oversized_prefix = minimal_config(Vec::new());
    oversized_prefix
        .handoff_mut()
        .set_exec_prefix(vec!["x".to_owned(); 65]);
    assert!(matches!(
        oversized_prefix.validate(),
        Err(ModelError::Invalid { .. })
    ));

    let mut oversized_prefix_bytes = minimal_config(Vec::new());
    oversized_prefix_bytes
        .handoff_mut()
        .set_exec_prefix(vec!["x".repeat(64 * 1024); 5]);
    assert!(matches!(
        oversized_prefix_bytes.validate(),
        Err(ModelError::Invalid { .. })
    ));

    let actions = (0..8_193)
        .map(|index| {
            Action::new(
                format!("action-{index}"),
                ActionKind::FilesystemEnsureDir,
                Origin::image("fixture"),
            )
        })
        .collect();
    assert!(matches!(
        minimal_config(actions).validate(),
        Err(ModelError::Invalid { .. })
    ));
}

#[test]
fn root_action_from_workspace_reports_provenance_and_remediation_context() {
    let mut action = Action::new(
        "unsafe-root",
        ActionKind::FilesystemEnsureDir,
        Origin::workspace("project-overlay"),
    );
    action.set_path(Some("/etc/project".to_owned()));
    action.set_run_as(RunAs::Root);

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
    action.set_path(Some("/var/lib/app".to_owned()));
    action.set_parent_mode(Some("0755".to_owned()));
    action.set_run_as(RunAs::Root);

    let action_json = to_value(&action).expect("action should be JSON serializable");
    assert!(action_json.get("origin").is_none());

    let plan = minimal_config(vec![action])
        .build_plan()
        .expect("plan should serialize");
    let json = to_value(plan).expect("plan should be JSON serializable");
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
