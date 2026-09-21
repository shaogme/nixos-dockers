use super::*;
use std::collections::BTreeMap;

fn config(actions: Vec<Action>) -> BootstrapConfig {
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
            runtime: "/usr/bin/dev-env".to_owned(),
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
                    .then_some(InputNamespace::Host),
                default: None,
                allow_outside_workspace: input_type == InputType::Path,
            },
        )
    })
    .collect()
}

fn action(id: &str, kind: ActionKind) -> Action {
    Action::new(id, kind, Origin::image("base"))
}

#[test]
fn uid_pair_and_typed_inputs_are_parsed_without_shell_logic() {
    let input = BootstrapInput {
        target: "identity.uid".to_owned(),
        input_type: InputType::UidPair,
        aliases: vec!["HOST_UID".to_owned()],
        runtime: true,
        format: Some("uid[:gid]".to_owned()),
        namespace: Some(InputNamespace::Host),
        default: None,
        allow_outside_workspace: false,
    };
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

    let gid = BootstrapInput {
        input_type: InputType::Gid,
        ..input.clone()
    };
    assert_eq!(gid.parse_value("42").unwrap(), ParsedInput::Gid(42));
    assert!(gid.parse_value("-1").is_err());

    let boolean = BootstrapInput {
        input_type: InputType::Bool,
        ..input
    };
    assert_eq!(boolean.parse_value("1").unwrap(), ParsedInput::Bool(true));
    assert_eq!(
        boolean.parse_value("false").unwrap(),
        ParsedInput::Bool(false)
    );
    assert!(boolean.parse_value("maybe").is_err());
}

#[test]
fn identity_uid_gid_inputs_require_and_agree_on_namespace() {
    let mut config = crate::tests::config(Vec::new());
    config.inputs.get_mut("HOST_UID").unwrap().namespace = None;
    assert!(matches!(
        config.validate(),
        Err(ModelError::Invalid { location, .. }) if location.ends_with("HOST_UID.namespace")
    ));

    let mut config = crate::tests::config(Vec::new());
    config.inputs.get_mut("HOST_GID").unwrap().namespace = Some(InputNamespace::Container);
    assert!(matches!(
        config.validate(),
        Err(ModelError::Invalid { location, .. }) if location == "bootstrap.identity"
    ));
}

#[test]
fn path_input_rejects_traversal_and_shell_fragments() {
    let input = BootstrapInput {
        target: "identity.home".to_owned(),
        input_type: InputType::Path,
        aliases: vec![],
        runtime: true,
        format: None,
        namespace: None,
        default: None,
        allow_outside_workspace: true,
    };
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
fn plan_adds_identity_dependency_and_orders_phases_deterministically() {
    let mut identity = action("identity", ActionKind::IdentityResolve);
    identity.run_as = RunAs::Root;
    let mut link = action("link", ActionKind::FilesystemEnsureSymlink);
    link.link = Some("${identity.home}/.config/app".to_owned());
    link.target = Some("/data/app".to_owned());
    link.run_as = RunAs::Target;
    let mut workspace = action("workspace", ActionKind::FilesystemEnsureDir);
    workspace.path = Some("/workspace".to_owned());
    workspace.run_as = RunAs::Root;
    let plan = config(vec![link, workspace, identity])
        .build_plan()
        .unwrap();
    assert_eq!(
        plan.ids().collect::<Vec<_>>(),
        vec!["identity", "workspace", "link"]
    );
    assert_eq!(plan.actions()[2].depends_on, vec!["identity".to_owned()]);
}

#[test]
fn plan_rejects_missing_dependency_cycles_and_duplicate_ids() {
    let mut first = action("first", ActionKind::FilesystemEnsureDir);
    first.path = Some("/workspace/one".to_owned());
    first.depends_on = vec!["missing".to_owned()];
    assert!(matches!(
        config(vec![first]).build_plan(),
        Err(ModelError::MissingDependency { .. })
    ));

    let mut a = action("a", ActionKind::FilesystemEnsureDir);
    a.path = Some("/workspace/a".to_owned());
    a.depends_on = vec!["b".to_owned()];
    let mut b = action("b", ActionKind::FilesystemEnsureDir);
    b.path = Some("/workspace/b".to_owned());
    b.depends_on = vec!["a".to_owned()];
    assert!(matches!(
        config(vec![a, b]).build_plan(),
        Err(ModelError::DependencyCycle(_))
    ));

    let mut duplicate = action("same", ActionKind::FilesystemEnsureDir);
    duplicate.path = Some("/workspace/two".to_owned());
    let mut duplicate_again = action("same", ActionKind::FilesystemEnsureDir);
    duplicate_again.path = Some("/workspace/three".to_owned());
    assert!(matches!(
        config(vec![duplicate, duplicate_again]).build_plan(),
        Err(ModelError::DuplicateActionId(_))
    ));
}

#[test]
fn workspace_actions_are_limited_to_safe_target_user_actions() {
    let mut workspace = action("workspace", ActionKind::FilesystemEnsureDir);
    workspace.origin = Origin::workspace("repo");
    workspace.path = Some("/workspace/.cache".to_owned());
    workspace.run_as = RunAs::Target;
    let mut bootstrap = config(vec![workspace]);
    bootstrap.allow_workspace_overlay = true;
    bootstrap
        .policy
        .workspace_safe_action_kinds
        .insert(ActionKind::FilesystemEnsureDir);
    assert!(bootstrap.build_plan().is_ok());

    let mut root = action("root", ActionKind::FilesystemEnsureDir);
    root.origin = Origin::workspace("repo");
    root.path = Some("/etc/unsafe".to_owned());
    root.run_as = RunAs::Root;
    bootstrap.actions = vec![root];
    assert!(matches!(
        bootstrap.build_plan(),
        Err(ModelError::TrustViolation { .. })
    ));
}

#[test]
fn rendered_paths_are_revalidated_after_interpolation() {
    let template = PathTemplate::new("${identity.home}/.config/tool").unwrap();
    let mut values = BTreeMap::new();
    values.insert("identity.home".to_owned(), "/home/dev".to_owned());
    assert_eq!(
        template.render(&values).unwrap(),
        "/home/dev/.config/tool".to_owned()
    );
    values.insert("identity.home".to_owned(), "/home/../root".to_owned());
    assert!(template.render(&values).is_err());
}

#[test]
fn privilege_drop_and_handoff_are_terminal_and_ordered() {
    let mut identity = action("identity", ActionKind::IdentityResolve);
    identity.run_as = RunAs::Root;
    let mut directory = action("directory", ActionKind::FilesystemEnsureDir);
    directory.path = Some("/var/lib/app".to_owned());
    directory.run_as = RunAs::Root;
    let mut drop = action("drop", ActionKind::ProcessDropPrivileges);
    drop.run_as = RunAs::Root;
    let handoff = action("handoff", ActionKind::HandoffExec);

    let plan = config(vec![handoff, drop, directory, identity])
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
    identity.run_as = RunAs::Root;
    let mut ssh = action("ssh", ActionKind::ServiceSshPrepare);
    ssh.run_as = RunAs::Root;
    ssh.host_key_dir = Some("/etc/ssh".to_owned());
    ssh.authorized_keys_dir = Some("${identity.home}/.ssh".to_owned());
    ssh.runtime_dir = Some("/run/sshd".to_owned());
    ssh.host_key_types = Some(vec!["ed25519".to_owned()]);
    ssh.content = Some("ssh-ed25519 AAAAfixture\n".to_owned());

    let plan = config(vec![ssh, identity])
        .build_plan()
        .expect("SSH action should build a static plan");
    assert_eq!(plan.ids().collect::<Vec<_>>(), vec!["identity", "ssh"]);
    assert_eq!(plan.actions()[1].depends_on, vec!["identity".to_owned()]);
    assert_eq!(plan.actions()[1].phase, PlanPhase::Root);
    assert_eq!(
        plan.actions()[1].effect,
        PlanEffect::ServiceSshPrepare {
            host_key_dir: "/etc/ssh".to_owned(),
            authorized_keys_dir: "${identity.home}/.ssh".to_owned(),
            runtime_dir: "/run/sshd".to_owned(),
            host_key_types: vec!["ed25519".to_owned()],
            authorized_keys: true,
        }
    );
}

#[test]
fn ssh_prepare_rejects_unsupported_key_types_and_conflicting_key_sources() {
    let mut ssh = action("ssh", ActionKind::ServiceSshPrepare);
    ssh.run_as = RunAs::Root;
    ssh.host_key_dir = Some("/etc/ssh".to_owned());
    ssh.authorized_keys_dir = Some("/etc/ssh/authorized_keys".to_owned());
    ssh.runtime_dir = Some("/run/sshd".to_owned());
    ssh.host_key_types = Some(vec!["dsa".to_owned()]);
    assert!(matches!(
        config(vec![ssh]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("unsupported SSH host key type")
    ));

    let mut ssh = action("ssh", ActionKind::ServiceSshPrepare);
    ssh.run_as = RunAs::Root;
    ssh.host_key_dir = Some("/etc/ssh".to_owned());
    ssh.authorized_keys_dir = Some("/etc/ssh/authorized_keys".to_owned());
    ssh.runtime_dir = Some("/run/sshd".to_owned());
    ssh.content = Some("key\n".to_owned());
    ssh.authorized_keys_source = Some("/tmp/authorized_keys".to_owned());
    assert!(matches!(
        config(vec![ssh]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("mutually exclusive")
    ));
}

#[test]
fn cgroup_v2_init_action_model_and_plan_effects() {
    let mut cg = action("cg", ActionKind::CgroupV2Init);
    cg.run_as = RunAs::Root;
    cg.path = Some("/sys/fs/cgroup".to_owned());
    cg.subgroup = Some("init".to_owned());
    cg.controllers = Some(vec!["cpu".to_owned(), "memory".to_owned()]);

    let plan = config(vec![cg])
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
            subgroup: Some("init".to_owned()),
            controllers: Some(vec!["cpu".to_owned(), "memory".to_owned()]),
        }
    );

    // Tests bind_mount mode and shadow_path
    let mut cg_bind = action("cg_bind", ActionKind::CgroupV2Init);
    cg_bind.run_as = RunAs::Root;
    cg_bind.mount_mode = Some("bind_mount".to_owned());
    cg_bind.shadow_path = Some("/run/cgroup".to_owned());
    let plan_bind = config(vec![cg_bind])
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
        }
    );

    // Rejects invalid mount_mode
    let mut cg_bad_mode = action("cg", ActionKind::CgroupV2Init);
    cg_bad_mode.run_as = RunAs::Root;
    cg_bad_mode.mount_mode = Some("invalid_mode".to_owned());
    assert!(matches!(
        config(vec![cg_bad_mode]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("invalid mount_mode")
    ));

    // Rejects non-root
    let mut cg_user = action("cg", ActionKind::CgroupV2Init);
    cg_user.run_as = RunAs::Target;
    assert!(matches!(
        config(vec![cg_user]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("must run as root")
    ));

    // Rejects invalid subgroup names
    let mut cg_bad_subgroup = action("cg", ActionKind::CgroupV2Init);
    cg_bad_subgroup.run_as = RunAs::Root;
    cg_bad_subgroup.subgroup = Some("foo/bar".to_owned());
    assert!(matches!(
        config(vec![cg_bad_subgroup]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("subgroup")
    ));

    // Rejects empty controllers list
    let mut cg_empty_controllers = action("cg", ActionKind::CgroupV2Init);
    cg_empty_controllers.run_as = RunAs::Root;
    cg_empty_controllers.controllers = Some(vec![]);
    assert!(matches!(
        config(vec![cg_empty_controllers]).build_plan(),
        Err(ModelError::Invalid { message, .. }) if message.contains("controllers")
    ));
}
