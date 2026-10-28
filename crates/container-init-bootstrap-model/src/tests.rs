use std::collections::BTreeMap;

use crate::{
    Action, ActionKind, BootstrapConfig, BootstrapInput, Condition, HandoffConfig, IdentityConfig,
    InputNamespace, InputType, ModelError, Origin, ParsedInput, PathTemplate, PlanEffect,
    PlanPhase, RunAs,
};

fn bootstrap_config(actions: Vec<Action>) -> BootstrapConfig {
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

fn action(id: &str, kind: ActionKind) -> Action {
    Action::new(id, kind, Origin::image("base"))
}

#[test]
fn uid_pair_and_typed_inputs_are_parsed_without_shell_logic() {
    let mut input = BootstrapInput::new("identity.uid", InputType::UidPair);
    input.set_aliases(vec!["HOST_UID".to_owned()]);
    input.set_runtime(true);
    input.set_format(Some("uid[:gid]".to_owned()));
    input.set_namespace(Some(InputNamespace::Host));
    assert_eq!(
        input.parse_value("1000:1001").unwrap(),
        ParsedInput::UidPair {
            uid: 1000,
            gid: Some(1001)
        }
    );
    assert_eq!(
        input.parse_value("1000").unwrap(),
        ParsedInput::UidPair {
            uid: 1000,
            gid: None
        }
    );
    assert!(input.parse_value("1000:1001:1002").is_err());
    assert!(input.parse_value("1000:bad").is_err());

    let mut gid = input.clone();
    gid.set_input_type(InputType::Gid);
    assert_eq!(gid.parse_value("42").unwrap(), ParsedInput::Gid(42));
    assert!(gid.parse_value("-1").is_err());

    let mut boolean = input;
    boolean.set_input_type(InputType::Bool);
    assert_eq!(boolean.parse_value("1").unwrap(), ParsedInput::Bool(true));
    assert_eq!(
        boolean.parse_value("false").unwrap(),
        ParsedInput::Bool(false)
    );
    assert!(boolean.parse_value("maybe").is_err());
}

#[test]
fn identity_uid_gid_inputs_require_and_agree_on_namespace() {
    let mut config = bootstrap_config(Vec::new());
    config
        .inputs_mut()
        .get_mut("HOST_UID")
        .unwrap()
        .set_namespace(None);
    assert!(matches!(
        config.validate(),
        Err(ModelError::Invalid { location, .. }) if location.ends_with("HOST_UID.namespace")
    ));

    let mut config = bootstrap_config(Vec::new());
    config
        .inputs_mut()
        .get_mut("HOST_GID")
        .unwrap()
        .set_namespace(Some(InputNamespace::Container));
    assert!(matches!(
        config.validate(),
        Err(ModelError::Invalid { location, .. }) if location == "bootstrap.identity"
    ));
}

#[test]
fn path_input_rejects_traversal_and_shell_fragments() {
    let mut input = BootstrapInput::new("identity.home", InputType::Path);
    input.set_runtime(true);
    input.set_allow_outside_workspace(true);
    assert_eq!(
        input.parse_value("/home/alice").unwrap(),
        ParsedInput::Path("/home/alice".to_owned())
    );
    assert!(input.parse_value("/tmp/../root").is_err());
    assert!(input.parse_value("$(touch /tmp/pwned)").is_err());
    assert!(input.parse_value("${identity.home}/.cache").is_err());
}

#[test]
fn condition_parser_only_accepts_restricted_forms() {
    assert_eq!(
        Condition::parse("feature:services.ssh && input_set(HOST_UID)").unwrap(),
        Condition::And(vec![
            Condition::Feature("services.ssh".to_owned()),
            Condition::InputSet("HOST_UID".to_owned()),
        ])
    );
    assert!(Condition::parse("exists(/workspace) || writable(${context.cwd})").is_ok());
    assert!(Condition::parse("identity.uid == 0").is_ok());
    assert!(Condition::parse("context.path_exists_or_create").is_ok());
    assert!(Condition::parse("$(id)").is_err());
    assert!(Condition::parse("exists(/tmp); rm -rf /").is_err());
}

#[test]
fn startup_actions_reject_runtime_inputs_identity_and_environment() {
    let mut input_action = action("input-path", ActionKind::FilesystemEnsureDir);
    input_action.set_path(Some("${input.CONTAINER_HOME}/cache".to_owned()));
    input_action.set_run_as(RunAs::Root);
    let error = bootstrap_config(vec![input_action]).validate().unwrap_err();
    assert!(matches!(
        error,
        ModelError::Invalid { ref location, ref message }
            if location == "bootstrap.actions.input-path.path"
                && message.contains("runtime input CONTAINER_HOME")
    ));

    let mut identity_action = action("identity-owner", ActionKind::FilesystemEnsureDir);
    identity_action.set_path(Some("/run/example".to_owned()));
    identity_action.set_owner(Some("identity.target".to_owned()));
    identity_action.set_run_as(RunAs::Root);
    let error = bootstrap_config(vec![identity_action])
        .validate()
        .unwrap_err();
    assert!(matches!(
        error,
        ModelError::Invalid { ref location, ref message }
            if location == "bootstrap.actions.identity-owner.owner"
                && message.contains("request identity")
    ));

    let mut environment_action = action("environment-condition", ActionKind::FilesystemEnsureDir);
    environment_action.set_path(Some("/run/example".to_owned()));
    environment_action.set_when(Some("env.ENABLE == '1'".to_owned()));
    environment_action.set_run_as(RunAs::Root);
    let error = bootstrap_config(vec![environment_action])
        .validate()
        .unwrap_err();
    assert!(matches!(
        error,
        ModelError::Invalid { ref location, ref message }
            if location == "bootstrap.actions.environment-condition.when"
                && message.contains("runtime environment env.ENABLE")
    ));
}

#[test]
fn plan_adds_identity_dependency_and_orders_phases_deterministically() {
    let mut identity = action("identity", ActionKind::IdentityResolve);
    identity.set_run_as(RunAs::Root);
    let mut link = action("link", ActionKind::FilesystemEnsureSymlink);
    link.set_link(Some("${identity.home}/.config/app".to_owned()));
    link.set_target(Some("/data/app".to_owned()));
    link.set_run_as(RunAs::Target);
    link.set_origin(Origin::workspace("workspace"));
    let mut workspace = action("workspace", ActionKind::FilesystemEnsureDir);
    workspace.set_path(Some("/workspace".to_owned()));
    workspace.set_run_as(RunAs::Root);
    let mut bootstrap = bootstrap_config(vec![link, workspace, identity]);
    bootstrap.set_allow_workspace_overlay(true);
    bootstrap
        .policy_mut()
        .workspace_safe_action_kinds
        .insert(ActionKind::FilesystemEnsureSymlink);
    let plan = bootstrap.build_plan().unwrap();
    assert_eq!(
        plan.ids().collect::<Vec<_>>(),
        vec!["identity", "workspace", "link"]
    );
    assert_eq!(plan.actions()[2].depends_on, vec!["identity".to_owned()]);
}

#[test]
fn plan_rejects_missing_dependency_cycles_and_duplicate_ids() {
    let mut first = action("first", ActionKind::FilesystemEnsureDir);
    first.set_path(Some("/workspace/one".to_owned()));
    first.set_depends_on(vec!["missing".to_owned()]);
    assert!(matches!(
        bootstrap_config(vec![first]).build_plan(),
        Err(ModelError::MissingDependency { .. })
    ));

    let mut a = action("a", ActionKind::FilesystemEnsureDir);
    a.set_path(Some("/workspace/a".to_owned()));
    a.set_depends_on(vec!["b".to_owned()]);
    let mut b = action("b", ActionKind::FilesystemEnsureDir);
    b.set_path(Some("/workspace/b".to_owned()));
    b.set_depends_on(vec!["a".to_owned()]);
    assert!(matches!(
        bootstrap_config(vec![a, b]).build_plan(),
        Err(ModelError::DependencyCycle(_))
    ));

    let mut duplicate = action("same", ActionKind::FilesystemEnsureDir);
    duplicate.set_path(Some("/workspace/two".to_owned()));
    let mut duplicate_again = action("same", ActionKind::FilesystemEnsureDir);
    duplicate_again.set_path(Some("/workspace/three".to_owned()));
    assert!(matches!(
        bootstrap_config(vec![duplicate, duplicate_again]).build_plan(),
        Err(ModelError::DuplicateActionId(_))
    ));
}

#[test]
fn workspace_actions_are_limited_to_safe_target_user_actions() {
    let mut workspace = action("workspace", ActionKind::FilesystemEnsureDir);
    workspace.set_origin(Origin::workspace("repo"));
    workspace.set_path(Some("/workspace/.cache".to_owned()));
    workspace.set_run_as(RunAs::Target);
    let mut bootstrap = bootstrap_config(vec![workspace]);
    bootstrap.set_allow_workspace_overlay(true);
    bootstrap
        .policy_mut()
        .workspace_safe_action_kinds
        .insert(ActionKind::FilesystemEnsureDir);
    assert!(bootstrap.build_plan().is_ok());

    let mut root = action("root", ActionKind::FilesystemEnsureDir);
    root.set_origin(Origin::workspace("repo"));
    root.set_path(Some("/etc/unsafe".to_owned()));
    root.set_run_as(RunAs::Root);
    bootstrap.set_actions(vec![root]);
    assert!(matches!(
        bootstrap.build_plan(),
        Err(ModelError::TrustViolation { .. })
    ));
}

#[test]
fn rendered_paths_are_revalidated_after_interpolation() {
    let template = PathTemplate::new("${identity.home}/.config/tool").unwrap();
    let mut values = BTreeMap::new();
    values.insert("identity.home".to_owned(), "/home/user".to_owned());
    assert_eq!(
        template.render(&values).unwrap(),
        "/home/user/.config/tool".to_owned()
    );
    values.insert("identity.home".to_owned(), "/home/../root".to_owned());
    assert!(template.render(&values).is_err());
}

#[test]
fn privilege_drop_and_handoff_are_terminal_and_ordered() {
    let mut identity = action("identity", ActionKind::IdentityResolve);
    identity.set_run_as(RunAs::Root);
    let mut directory = action("directory", ActionKind::FilesystemEnsureDir);
    directory.set_path(Some("/var/lib/app".to_owned()));
    directory.set_run_as(RunAs::Root);
    let mut drop = action("drop", ActionKind::ProcessDropPrivileges);
    drop.set_run_as(RunAs::Root);
    let handoff = action("handoff", ActionKind::HandoffExec);

    let plan = bootstrap_config(vec![handoff, drop, directory, identity])
        .build_plan()
        .unwrap();
    assert_eq!(
        plan.ids().collect::<Vec<_>>(),
        vec!["identity", "directory", "drop", "handoff"]
    );
    assert_eq!(plan.actions()[2].phase, PlanPhase::Handoff);
    assert_eq!(plan.actions()[3].phase, PlanPhase::Handoff);
}

#[test]
fn ssh_prepare_is_a_root_capability_with_identity_dependency_and_safe_fields() {
    let mut identity = action("identity", ActionKind::IdentityResolve);
    identity.set_run_as(RunAs::Root);
    let mut ssh = action("ssh", ActionKind::ServiceSshPrepare);
    ssh.set_run_as(RunAs::Root);
    ssh.set_host_key_dir(Some("/etc/ssh".to_owned()));
    ssh.set_authorized_keys_dir(Some("/home/user/.ssh".to_owned()));
    ssh.set_runtime_dir(Some("/run/sshd".to_owned()));
    ssh.set_host_key_types(Some(vec!["ed25519".to_owned()]));
    ssh.set_content(Some("ssh-ed25519 AAAAfixture\n".to_owned()));

    let plan = bootstrap_config(vec![ssh, identity])
        .build_plan()
        .expect("SSH action should build a static plan");
    assert_eq!(plan.ids().collect::<Vec<_>>(), vec!["identity", "ssh"]);
    assert_eq!(plan.actions()[1].depends_on, vec!["identity".to_owned()]);
    assert_eq!(plan.actions()[1].phase, PlanPhase::Root);
    assert_eq!(
        plan.actions()[1].effect,
        PlanEffect::ServiceSshPrepare {
            host_key_dir: "/etc/ssh".to_owned(),
            authorized_keys_dir: "/home/user/.ssh".to_owned(),
            runtime_dir: "/run/sshd".to_owned(),
            host_key_types: vec!["ed25519".to_owned()],
            authorized_keys: true,
        }
    );
}

#[test]
fn ssh_prepare_rejects_unsupported_key_types_and_conflicting_key_sources() {
    let mut ssh = action("ssh", ActionKind::ServiceSshPrepare);
    ssh.set_run_as(RunAs::Root);
    ssh.set_host_key_dir(Some("/etc/ssh".to_owned()));
    ssh.set_authorized_keys_dir(Some("/etc/ssh/authorized_keys".to_owned()));
    ssh.set_runtime_dir(Some("/run/sshd".to_owned()));
    ssh.set_host_key_types(Some(vec!["dsa".to_owned()]));
    assert!(matches!(
        bootstrap_config(vec![ssh]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("unsupported SSH host key type")
    ));

    let mut ssh = action("ssh", ActionKind::ServiceSshPrepare);
    ssh.set_run_as(RunAs::Root);
    ssh.set_host_key_dir(Some("/etc/ssh".to_owned()));
    ssh.set_authorized_keys_dir(Some("/etc/ssh/authorized_keys".to_owned()));
    ssh.set_runtime_dir(Some("/run/sshd".to_owned()));
    ssh.set_content(Some("key\n".to_owned()));
    ssh.set_authorized_keys_source(Some("/tmp/authorized_keys".to_owned()));
    assert!(matches!(
        bootstrap_config(vec![ssh]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("mutually exclusive")
    ));
}

#[test]
fn cgroup_v2_init_action_model_and_plan_effects() {
    let mut cg = action("cg", ActionKind::CgroupV2Init);
    cg.set_run_as(RunAs::Root);
    cg.set_path(Some("/sys/fs/cgroup".to_owned()));
    cg.set_subgroup(Some("libpod_parent".to_owned()));
    cg.set_controllers(Some(vec!["cpu".to_owned(), "memory".to_owned()]));
    cg.set_optional_controllers(Some(vec!["io".to_owned()]));

    let plan = bootstrap_config(vec![cg])
        .build_plan()
        .expect("cgroup init action should build plan");
    assert_eq!(plan.ids().collect::<Vec<_>>(), vec!["cg"]);
    assert_eq!(plan.actions()[0].phase, PlanPhase::Root);
    assert_eq!(
        plan.actions()[0].effect,
        PlanEffect::CgroupV2Init {
            path: Some("/sys/fs/cgroup".to_owned()),
            mount_mode: "default".to_owned(),
            shadow_path: None,
            subgroup: Some("libpod_parent".to_owned()),
            controllers: Some(vec!["cpu".to_owned(), "memory".to_owned()]),
            optional_controllers: Some(vec!["io".to_owned()]),
            subgroup_type: None,
            subgroup_controllers: None,
            subgroup_controller_values: None,
        }
    );

    // Tests bind_mount mode and shadow_path
    let mut cg_bind = action("cg_bind", ActionKind::CgroupV2Init);
    cg_bind.set_run_as(RunAs::Root);
    cg_bind.set_mount_mode(Some("bind_mount".to_owned()));
    cg_bind.set_shadow_path(Some("/run/cgroup".to_owned()));
    let plan_bind = bootstrap_config(vec![cg_bind])
        .build_plan()
        .expect("cgroup init with bind_mount should build plan");
    assert_eq!(
        plan_bind.actions()[0].effect,
        PlanEffect::CgroupV2Init {
            path: None,
            mount_mode: "bind_mount".to_owned(),
            shadow_path: Some("/run/cgroup".to_owned()),
            subgroup: None,
            controllers: None,
            optional_controllers: None,
            subgroup_type: None,
            subgroup_controllers: None,
            subgroup_controller_values: None,
        }
    );

    // Rejects invalid mount_mode
    let mut cg_bad_mode = action("cg", ActionKind::CgroupV2Init);
    cg_bad_mode.set_run_as(RunAs::Root);
    cg_bad_mode.set_mount_mode(Some("invalid_mode".to_owned()));
    assert!(matches!(
        bootstrap_config(vec![cg_bad_mode]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("invalid mount_mode")
    ));

    // Rejects non-root
    let mut cg_user = action("cg", ActionKind::CgroupV2Init);
    cg_user.set_run_as(RunAs::Target);
    assert!(matches!(
        bootstrap_config(vec![cg_user]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("must run as root")
    ));

    // Rejects invalid subgroup names
    let mut cg_bad_subgroup = action("cg", ActionKind::CgroupV2Init);
    cg_bad_subgroup.set_run_as(RunAs::Root);
    cg_bad_subgroup.set_subgroup(Some("foo/bar".to_owned()));
    assert!(matches!(
        bootstrap_config(vec![cg_bad_subgroup]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("subgroup")
    ));

    // Rejects empty controllers list
    let mut cg_empty_controllers = action("cg", ActionKind::CgroupV2Init);
    cg_empty_controllers.set_run_as(RunAs::Root);
    cg_empty_controllers.set_controllers(Some(vec![]));
    assert!(matches!(
        bootstrap_config(vec![cg_empty_controllers]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("controllers")
    ));

    // Required and optional sets must not overlap.
    let mut cg_duplicate_controllers = action("cg", ActionKind::CgroupV2Init);
    cg_duplicate_controllers.set_run_as(RunAs::Root);
    cg_duplicate_controllers.set_controllers(Some(vec!["cpu".to_owned()]));
    cg_duplicate_controllers.set_optional_controllers(Some(vec!["cpu".to_owned()]));
    assert!(matches!(
        bootstrap_config(vec![cg_duplicate_controllers]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("duplicate controller")
    ));
}
