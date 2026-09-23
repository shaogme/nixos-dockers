use bootstrap_model::{
    Action, ActionKind, BootstrapConfig, BootstrapMode, BootstrapPolicy, HandoffConfig,
    IdentityConfig, NonInteractivePolicy, Origin, PlanPhase, RunAs,
};
use container_init_core::{
    ActionStatus, CoreError, ExecutionOptions, IdentityResolver, IdentitySource, PlanExecutor,
    PosixSystem, RuntimeContext, SshCapability, WorkspaceObservation, WorkspaceStatus,
};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn config(workspace: &Path, actions: Vec<Action>) -> BootstrapConfig {
    let (uid, gid) = PosixSystem::new().current_ids();
    BootstrapConfig {
        schema: 1,
        mode: BootstrapMode::Strict,
        workspace_root: workspace.display().to_string(),
        allow_workspace_overlay: false,
        non_interactive: NonInteractivePolicy::Deny,
        identity: IdentityConfig {
            default_user: None,
            default_uid: Some(uid),
            default_gid: Some(gid),
            default_home: None,
            auto_mapping: false,
            run_as_root_input: None,
            uid_input: None,
            gid_input: None,
            home_input: None,
        },
        handoff: HandoffConfig {
            runtime: "/bin/sh".to_owned(),
            exec_prefix: vec!["-c".to_owned()],
            shell_prefix: vec!["-c".to_owned(), "true".to_owned()],
            ssh_daemon: None,
            login_shell: None,
        },
        policy: BootstrapPolicy::default(),
        inputs: BTreeMap::new(),
        actions,
    }
}

fn action(id: &str, kind: ActionKind) -> Action {
    Action::new(id, kind, Origin::image("test-profile"))
}

fn root_action(id: &str, kind: ActionKind) -> Action {
    let mut action = action(id, kind);
    action.run_as = RunAs::Root;
    action
}

fn executor(config: BootstrapConfig) -> PlanExecutor {
    let cwd = config.workspace_root.clone();
    PlanExecutor::new(config, RuntimeContext::new(cwd))
}

fn identity_input_config(workspace: &Path) -> BootstrapConfig {
    let mut config = config(workspace, Vec::new());
    config.identity.default_user = Some("dev".to_owned());
    config.identity.default_uid = Some(1000);
    config.identity.default_gid = Some(1000);
    config.identity.auto_mapping = true;
    config.identity.uid_input = Some("HOST_UID".to_owned());
    config.identity.gid_input = Some("HOST_GID".to_owned());
    config.inputs = [
        (
            "HOST_UID".to_owned(),
            bootstrap_model::BootstrapInput {
                target: "identity.uid".to_owned(),
                input_type: bootstrap_model::InputType::UidPair,
                aliases: Vec::new(),
                runtime: true,
                format: Some("uid[:gid]".to_owned()),
                namespace: Some(bootstrap_model::InputNamespace::Host),
                default: None,
                allow_outside_workspace: false,
            },
        ),
        (
            "HOST_GID".to_owned(),
            bootstrap_model::BootstrapInput {
                target: "identity.gid".to_owned(),
                input_type: bootstrap_model::InputType::Gid,
                aliases: Vec::new(),
                runtime: true,
                format: None,
                namespace: Some(bootstrap_model::InputNamespace::Host),
                default: None,
                allow_outside_workspace: false,
            },
        ),
    ]
    .into_iter()
    .collect();
    config
}

#[test]
fn host_namespace_inputs_are_translated_before_identity_selection() {
    let temp = TempDir::new().unwrap();
    let config = identity_input_config(temp.path());
    let posix = PosixSystem::new()
        .with_namespace_map_contents("100000 2000 1000\n", "200000 2000 1000\n")
        .unwrap();
    let context = RuntimeContext::new(temp.path())
        .with_environment([("HOST_UID", "2000:2000"), ("HOST_GID", "2000")]);
    let identity = IdentityResolver::with_posix(posix)
        .resolve(&config, &context)
        .unwrap();
    assert_eq!((identity.uid, identity.gid), (100000, 200000));
    assert_eq!(identity.user, "dev");
    assert_eq!(identity.uid_source, IdentitySource::ExplicitHost);
    assert_eq!(identity.gid_source, IdentitySource::ExplicitHost);
    assert_eq!(identity.workspace, WorkspaceStatus::NotMounted);
}

#[test]
fn mapped_host_root_is_canonical_root_and_unmapped_ids_fail() {
    let temp = TempDir::new().unwrap();
    let config = identity_input_config(temp.path());
    let posix = PosixSystem::new()
        .with_namespace_map_contents("0 1000 1\n", "0 1000 1\n")
        .unwrap();
    let context = RuntimeContext::new(temp.path())
        .with_environment([("HOST_UID", "1000"), ("HOST_GID", "1000")]);
    let identity = IdentityResolver::with_posix(posix.clone())
        .resolve(&config, &context)
        .unwrap();
    assert_eq!((identity.uid, identity.gid), (0, 0));
    assert_eq!(
        (identity.user, identity.home),
        ("root".to_owned(), Path::new("/home/user").to_path_buf())
    );

    let error = IdentityResolver::with_posix(posix)
        .resolve(
            &config,
            &RuntimeContext::new(temp.path())
                .with_environment([("HOST_UID", "2000"), ("HOST_GID", "2000")]),
        )
        .unwrap_err();
    assert!(matches!(error, CoreError::Identity { .. }));
}

#[test]
fn workspace_owner_is_used_only_when_mount_observation_is_explicitly_mounted() {
    let temp = TempDir::new().unwrap();
    let mut config = config(temp.path(), Vec::new());
    config.identity.default_user = Some("dev".to_owned());
    config.identity.default_uid = Some(1000);
    config.identity.default_gid = Some(1000);
    config.identity.auto_mapping = true;

    let not_mounted = IdentityResolver::new()
        .resolve(
            &config,
            &RuntimeContext::new(temp.path()).with_workspace_observation(
                WorkspaceObservation::NotMounted {
                    reason: "rootfs directory".to_owned(),
                },
            ),
        )
        .unwrap();
    assert_eq!((not_mounted.uid, not_mounted.gid), (1000, 1000));
    assert_eq!(not_mounted.uid_source, IdentitySource::ProfileDefault);

    let mounted = IdentityResolver::new()
        .resolve(
            &config,
            &RuntimeContext::new(temp.path())
                .with_workspace_observation(WorkspaceObservation::mounted(0, 0, temp.path(), 7)),
        )
        .unwrap();
    assert_eq!((mounted.uid, mounted.gid), (0, 0));
    assert_eq!(mounted.user, "root");
    assert_eq!(mounted.home, Path::new("/home/user"));
    assert_eq!(mounted.uid_source, IdentitySource::WorkspaceMount);
}

fn fake_ssh_keygen(root: &Path) -> std::path::PathBuf {
    let path = root.join("fake-ssh-keygen");
    fs::write(
        &path,
        "#!/bin/sh\nset -eu\noutput=\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"-f\" ]; then shift; output=$1; fi\n  shift\ndone\nprintf '%s\\n' '-----BEGIN OPENSSH PRIVATE KEY-----' > \"$output\"\nprintf '%s\\n' 'ssh-ed25519 AAAAfake' > \"$output.pub\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

#[test]
fn filesystem_actions_are_idempotent_and_atomic() {
    let temp = TempDir::new().unwrap();
    let directory = temp.path().join("state");
    let file = directory.join("config");
    let link = temp.path().join("current");

    let mut ensure_dir = action("directory", ActionKind::FilesystemEnsureDir);
    ensure_dir.path = Some(directory.display().to_string());
    ensure_dir.mode = Some("0750".to_owned());
    let mut ensure_file = action("file", ActionKind::FilesystemEnsureFile);
    ensure_file.path = Some(file.display().to_string());
    ensure_file.content = Some("stable configuration\n".to_owned());
    ensure_file.mode = Some("0640".to_owned());
    ensure_file.depends_on = vec!["directory".to_owned()];
    let mut ensure_link = action("link", ActionKind::FilesystemEnsureSymlink);
    ensure_link.link = Some(link.display().to_string());
    ensure_link.target = Some(file.display().to_string());
    ensure_link.depends_on = vec!["file".to_owned()];

    let config = config(temp.path(), vec![ensure_link, ensure_file, ensure_dir]);
    let plan = config.build_plan().unwrap();
    assert_eq!(
        plan.ids().collect::<Vec<_>>(),
        ["directory", "file", "link"]
    );
    assert_eq!(plan.actions()[0].phase, PlanPhase::Current);

    let runner = executor(config);
    let first = runner.execute_plan(&plan).unwrap();
    assert!(first.succeeded());
    assert_eq!(fs::read_to_string(&file).unwrap(), "stable configuration\n");
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_link(&link).unwrap(), file);
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o750
    );

    let second = runner.execute_plan(&plan).unwrap();
    assert!(second.succeeded());
    assert_eq!(
        second.outcome("directory").unwrap().status,
        ActionStatus::Succeeded
    );
}

#[test]
fn rendered_identity_paths_and_runtime_input_precedence_are_safe() {
    let temp = TempDir::new().unwrap();
    let mut config = config(temp.path(), vec![]);
    config.identity.default_user = Some("dev".to_owned());
    config.identity.default_uid = Some(1234);
    config.identity.default_gid = Some(1234);
    config.identity.uid_input = Some("HOST_UID".to_owned());
    config.identity.gid_input = Some("HOST_GID".to_owned());
    config.identity.home_input = Some("CONTAINER_HOME".to_owned());
    config.inputs = [
        (
            "HOST_UID".to_owned(),
            bootstrap_model::BootstrapInput {
                target: "identity.uid".to_owned(),
                input_type: bootstrap_model::InputType::UidPair,
                aliases: vec!["LEGACY_UID".to_owned()],
                runtime: true,
                format: Some("uid[:gid]".to_owned()),
                namespace: Some(bootstrap_model::InputNamespace::Container),
                default: None,
                allow_outside_workspace: false,
            },
        ),
        (
            "HOST_GID".to_owned(),
            bootstrap_model::BootstrapInput {
                target: "identity.gid".to_owned(),
                input_type: bootstrap_model::InputType::Gid,
                aliases: vec![],
                runtime: true,
                format: None,
                namespace: Some(bootstrap_model::InputNamespace::Container),
                default: None,
                allow_outside_workspace: false,
            },
        ),
        (
            "CONTAINER_HOME".to_owned(),
            bootstrap_model::BootstrapInput {
                target: "identity.home".to_owned(),
                input_type: bootstrap_model::InputType::Path,
                aliases: vec![],
                runtime: true,
                format: None,
                namespace: None,
                default: None,
                allow_outside_workspace: true,
            },
        ),
    ]
    .into_iter()
    .collect();
    let context = RuntimeContext::new(temp.path())
        .with_environment([
            ("HOST_UID".to_owned(), "1001:1002".to_owned()),
            ("HOST_GID".to_owned(), "1003".to_owned()),
            (
                "CONTAINER_HOME".to_owned(),
                "/tmp/container-home".to_owned(),
            ),
        ])
        .with_cli_input("HOST_UID", "1004:1005");
    let identity = IdentityResolver::new().resolve(&config, &context).unwrap();
    assert_eq!(identity.uid, 1004);
    assert_eq!(identity.gid, 1003);
    assert_eq!(identity.home, Path::new("/tmp/container-home"));

    let unsafe_home = RuntimeContext::new(temp.path())
        .with_environment([("CONTAINER_HOME".to_owned(), "/tmp/not-inside".to_owned())]);
    let mut restricted = config.clone();
    restricted
        .inputs
        .get_mut("CONTAINER_HOME")
        .unwrap()
        .allow_outside_workspace = false;
    let error = IdentityResolver::new()
        .resolve(&restricted, &unsafe_home)
        .unwrap_err();
    assert!(matches!(error, CoreError::Identity { .. }));
}

#[test]
fn conditions_and_failure_policies_do_not_run_dependents() {
    let temp = TempDir::new().unwrap();
    let collision = temp.path().join("collision");
    fs::write(&collision, "real file").unwrap();
    let skipped_path = temp.path().join("not-created");
    let dependent_path = temp.path().join("dependent");

    let mut skipped = action("skipped", ActionKind::FilesystemEnsureDir);
    skipped.path = Some(skipped_path.display().to_string());
    skipped.when = Some("false".to_owned());
    let mut dependent = action("dependent", ActionKind::FilesystemEnsureDir);
    dependent.path = Some(dependent_path.display().to_string());
    dependent.depends_on = vec!["skipped".to_owned()];
    let mut warned = action("warned", ActionKind::FilesystemEnsureSymlink);
    warned.link = Some(collision.display().to_string());
    warned.target = Some(temp.path().join("target").display().to_string());
    warned.failure = bootstrap_model::FailurePolicy::Warn;
    let mut warned_dependent = action("warned-dependent", ActionKind::FilesystemEnsureDir);
    warned_dependent.path = Some(temp.path().join("warned-dependent").display().to_string());
    warned_dependent.depends_on = vec!["warned".to_owned()];

    let config = config(
        temp.path(),
        vec![warned_dependent, dependent, warned, skipped],
    );
    let plan = config.build_plan().unwrap();
    let report = executor(config).execute_plan(&plan).unwrap();
    assert_eq!(
        report.outcome("skipped").unwrap().status,
        ActionStatus::SkippedCondition
    );
    assert_eq!(
        report.outcome("dependent").unwrap().status,
        ActionStatus::SkippedDependency
    );
    assert_eq!(
        report.outcome("warned").unwrap().status,
        ActionStatus::FailedWarn
    );
    assert_eq!(
        report.outcome("warned-dependent").unwrap().status,
        ActionStatus::SkippedDependency
    );
    assert!(matches!(
        report.outcome("warned").unwrap().error.as_ref(),
        Some(CoreError::Annotated { source, .. })
            if matches!(source.as_ref(), CoreError::Action { .. })
    ));
    assert!(!skipped_path.exists());
    assert!(!dependent_path.exists());
}

#[test]
fn passwd_mapping_shell_update_and_receipt_are_auditable() {
    if PosixSystem::new().current_ids().0 != 0 {
        return;
    }
    let temp = TempDir::new().unwrap();
    let passwd = temp.path().join("passwd");
    let group = temp.path().join("group");
    fs::write(&passwd, "fixture:x:2000:2000::/home/fixture:/bin/sh\n").unwrap();
    fs::write(&group, "\n").unwrap();
    let identity = root_action("identity", ActionKind::IdentityResolve);
    let mut map = root_action("map", ActionKind::IdentityMapUser);
    map.depends_on = vec!["identity".to_owned()];
    let mut shell = root_action("shell", ActionKind::ProcessSetUserShell);
    shell.user = Some("identity.target".to_owned());
    shell.shell = Some("/usr/bin/test-shell".to_owned());
    shell.depends_on = vec!["map".to_owned()];
    let mut config = config(temp.path(), vec![shell, map, identity]);
    config.identity.default_user = Some("fixture".to_owned());
    config.identity.default_uid = Some(2100);
    config.identity.default_gid = Some(2101);
    let receipt = temp.path().join("receipt.json");
    let options = ExecutionOptions::default()
        .with_posix(PosixSystem::with_account_files(&passwd, &group))
        .with_receipt_path(&receipt);
    let plan = config.build_plan().unwrap();
    let report = PlanExecutor::new(config, RuntimeContext::new(temp.path()))
        .with_options(options)
        .execute_plan(&plan)
        .unwrap();
    assert!(report.succeeded());
    let passwd_contents = fs::read_to_string(&passwd).unwrap();
    assert!(passwd_contents.contains("fixture:x:2100:2101::/home/user:/usr/bin/test-shell"));
    assert!(fs::read_to_string(&group)
        .unwrap()
        .contains("fixture:x:2101:"));
    let receipt_contents = fs::read_to_string(receipt).unwrap();
    assert!(receipt_contents.contains("\"map\""));
    assert!(!receipt_contents.contains("test-shell"));
}

#[test]
fn lock_supports_timeout_and_retry_and_handoff_argv_is_structured() {
    let temp = TempDir::new().unwrap();
    let lock_path = temp.path().join("bootstrap.lock");
    let lock = container_init_core::BootstrapLock::acquire(&lock_path).unwrap();
    let second = container_init_core::BootstrapLock::try_acquire(&lock_path).unwrap_err();
    assert_eq!(second.class(), container_init_core::ErrorClass::Lock);
    drop(lock);
    let _lock = container_init_core::BootstrapLock::acquire(&lock_path).unwrap();

    let config = config(temp.path(), vec![]);
    let runner = executor(config);
    let command = runner
        .build_handoff_command(&["echo".to_owned(), "safe value".to_owned()])
        .unwrap();
    assert_eq!(command.argv(), ["/bin/sh", "-c", "echo", "safe value"]);
}

#[test]
fn bootstrap_lock_retries_and_succeeds_when_released() {
    let temp = TempDir::new().unwrap();
    let lock_path = temp.path().join("bootstrap-retry.lock");
    let (tx, rx) = std::sync::mpsc::channel();
    let thread_path = lock_path.clone();

    let handle = std::thread::spawn(move || {
        let lock = container_init_core::BootstrapLock::acquire(&thread_path).unwrap();
        tx.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(lock);
    });

    rx.recv().unwrap();
    let started = std::time::Instant::now();
    let lock = container_init_core::BootstrapLock::acquire(&lock_path).unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(30));
    drop(lock);
    handle.join().unwrap();
}

#[test]
fn prepare_exec_resolves_identity_and_builds_handoff_without_lock() {
    let temp = TempDir::new().unwrap();
    let config = config(temp.path(), vec![]);
    let (uid, gid) = PosixSystem::new().current_ids();
    let runner = executor(config);
    let (identity, command, root_service) = runner
        .prepare_exec(&["echo".to_owned(), "direct exec".to_owned()])
        .unwrap();
    assert_eq!(identity.uid, uid);
    assert_eq!(identity.gid, gid);
    assert!(!root_service);
    assert_eq!(command.argv(), ["/bin/sh", "-c", "echo", "direct exec"]);
}

#[test]
fn executor_delegates_privilege_drop_to_the_posix_backend() {
    if PosixSystem::new().current_ids().0 != 0 {
        return;
    }
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("executor_drops_privileges_in_child")
        .env("CONTAINER_INIT_CORE_DROP_CHILD", "1")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn executor_drops_privileges_in_child() {
    if std::env::var_os("CONTAINER_INIT_CORE_DROP_CHILD").is_none() {
        return;
    }
    let resolve = root_action("resolve", ActionKind::IdentityResolve);
    let mut drop = root_action("drop", ActionKind::ProcessDropPrivileges);
    drop.depends_on = vec!["resolve".to_owned()];
    let mut config = config(Path::new("/tmp"), vec![drop, resolve]);
    config.identity.default_user = Some("nobody".to_owned());
    config.identity.default_uid = Some(65_534);
    config.identity.default_gid = Some(65_534);
    let plan = config.build_plan().unwrap();
    let report = executor(config).execute_plan(&plan).unwrap();
    assert!(report.succeeded());
    assert_eq!(PosixSystem::new().current_ids(), (65_534, 65_534));
}

#[test]
fn root_service_handoff_keeps_the_bootstrap_process_privileged() {
    if PosixSystem::new().current_ids().0 != 0 {
        return;
    }

    let resolve = root_action("resolve", ActionKind::IdentityResolve);
    let mut drop = root_action("drop", ActionKind::ProcessDropPrivileges);
    drop.depends_on = vec!["resolve".to_owned()];
    let mut handoff = root_action("handoff", ActionKind::HandoffExec);
    handoff.depends_on = vec!["drop".to_owned()];
    let mut config = config(Path::new("/tmp"), vec![handoff, drop, resolve]);
    config.identity.default_user = Some("nobody".to_owned());
    config.identity.default_uid = Some(65_534);
    config.identity.default_gid = Some(65_534);
    config.handoff.ssh_daemon = Some("/bin/sshd".to_owned());
    let plan = config.build_plan().unwrap();

    let report = executor(config)
        .execute(&plan, &["/bin/sshd".to_owned(), "-D".to_owned()])
        .unwrap();
    assert!(report.succeeded());
    assert_eq!(
        report.outcome("drop").unwrap().message,
        "privilege drop skipped for root service handoff"
    );
}

#[test]
fn unexpected_symlink_components_are_rejected_before_mutation() {
    let temp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), temp.path().join("escape")).unwrap();
    let mut action = action("unsafe", ActionKind::FilesystemEnsureDir);
    action.path = Some(temp.path().join("escape/new").display().to_string());
    let resolve = root_action("resolve", ActionKind::IdentityResolve);
    let config = config(temp.path(), vec![action, resolve]);
    let plan = config.build_plan().unwrap();
    let error = executor(config).execute_plan(&plan).unwrap_err();
    assert!(matches!(
        error,
        CoreError::Annotated { origin, source, .. }
            if origin.profile == "test-profile"
                && matches!(source.as_ref(), CoreError::Action { .. })
    ));
    assert!(!outside.path().join("new").exists());
}

#[test]
fn ssh_prepare_generates_reconciles_and_preserves_host_keys() {
    if PosixSystem::new().current_ids().0 != 0 {
        return;
    }

    let temp = TempDir::new().unwrap();
    let host_keys = temp.path().join("etc/ssh");
    let authorized_keys = temp.path().join("etc/ssh/authorized_keys");
    let runtime = temp.path().join("run/sshd");
    let authorized_keys_source = temp.path().join("authorized_keys.pub");
    fs::write(&authorized_keys_source, "ssh-ed25519 AAAAunit-test\n").unwrap();
    let mut action = root_action("ssh", ActionKind::ServiceSshPrepare);
    action.host_key_dir = Some(host_keys.display().to_string());
    action.authorized_keys_dir = Some(authorized_keys.display().to_string());
    action.runtime_dir = Some(runtime.display().to_string());
    action.host_key_types = Some(vec!["ed25519".to_owned()]);
    action.authorized_keys_source = Some(authorized_keys_source.display().to_string());

    let resolve = root_action("resolve", ActionKind::IdentityResolve);
    let config = config(temp.path(), vec![action, resolve]);
    let plan = config.build_plan().unwrap();
    let runner = executor(config).with_options(
        ExecutionOptions::default().with_ssh(SshCapability::new(fake_ssh_keygen(temp.path()))),
    );

    let first = runner.execute_plan(&plan).unwrap();
    assert!(first.succeeded());
    let private = host_keys.join("ssh_host_ed25519_key");
    let public = host_keys.join("ssh_host_ed25519_key.pub");
    let private_bytes = fs::read(&private).unwrap();
    assert!(private_bytes.starts_with(b"-----BEGIN OPENSSH PRIVATE KEY-----"));
    assert!(fs::read_to_string(&public)
        .unwrap()
        .starts_with("ssh-ed25519 "));
    assert_eq!(
        fs::metadata(&private).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&public).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(
        fs::metadata(&runtime).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::read_to_string(authorized_keys.join("root")).unwrap(),
        "ssh-ed25519 AAAAunit-test\n"
    );

    let second = runner.execute_plan(&plan).unwrap();
    assert!(second.succeeded());
    assert_eq!(fs::read(&private).unwrap(), private_bytes);
    assert!(second
        .outcome("ssh")
        .unwrap()
        .message
        .contains("0 host key(s) generated"));
}

#[test]
fn ssh_prepare_requires_capability_and_refuses_authorized_key_overwrite() {
    if PosixSystem::new().current_ids().0 != 0 {
        return;
    }

    let temp = TempDir::new().unwrap();
    let mut action = root_action("ssh", ActionKind::ServiceSshPrepare);
    action.host_key_dir = Some(temp.path().join("ssh").display().to_string());
    action.authorized_keys_dir = Some(temp.path().join("authorized").display().to_string());
    action.runtime_dir = Some(temp.path().join("run").display().to_string());
    action.host_key_types = Some(vec!["ed25519".to_owned()]);
    action.content = Some("ssh-ed25519 AAAAone\n".to_owned());

    let resolve = root_action("resolve", ActionKind::IdentityResolve);
    let config = config(temp.path(), vec![action, resolve]);
    let plan = config.build_plan().unwrap();
    let error = executor(config.clone())
        .execute_plan(&plan)
        .expect_err("SSH actions must not run without the capability");
    assert!(error.to_string().contains("enabled service capability"));
    assert!(!temp.path().join("ssh").exists());

    let keygen = fake_ssh_keygen(temp.path());
    let runner = executor(config)
        .with_options(ExecutionOptions::default().with_ssh(SshCapability::new(&keygen)));
    runner.execute_plan(&plan).unwrap();
    let mut changed = runner.config().clone();
    changed.actions[0].content = Some("ssh-ed25519 AAAAtwo\n".to_owned());
    let changed_plan = changed.build_plan().unwrap();
    let error = PlanExecutor::new(changed, RuntimeContext::new(temp.path()))
        .with_options(ExecutionOptions::default().with_ssh(SshCapability::new(&keygen)))
        .execute_plan(&changed_plan)
        .expect_err("existing authorized keys must not be silently replaced");
    assert!(error.to_string().contains("content differs"));
}

#[test]
fn ssh_prepare_rejects_incomplete_or_symlinked_host_keys() {
    if PosixSystem::new().current_ids().0 != 0 {
        return;
    }

    let temp = TempDir::new().unwrap();
    let host_keys = temp.path().join("ssh");
    fs::create_dir_all(&host_keys).unwrap();
    fs::write(host_keys.join("ssh_host_ed25519_key"), "private").unwrap();
    let mut action = root_action("ssh", ActionKind::ServiceSshPrepare);
    action.host_key_dir = Some(host_keys.display().to_string());
    action.authorized_keys_dir = Some(temp.path().join("authorized").display().to_string());
    action.runtime_dir = Some(temp.path().join("run").display().to_string());
    action.host_key_types = Some(vec!["ed25519".to_owned()]);
    let resolve = root_action("resolve", ActionKind::IdentityResolve);
    let config = config(temp.path(), vec![action, resolve]);
    let plan = config.build_plan().unwrap();
    let error = executor(config)
        .with_options(
            ExecutionOptions::default().with_ssh(SshCapability::new(fake_ssh_keygen(temp.path()))),
        )
        .execute_plan(&plan)
        .expect_err("a half-installed host key must be rejected");
    assert!(error.to_string().contains("incomplete"));
}

#[test]
fn cgroup_v2_init_delegates_controllers_and_moves_processes() {
    if PosixSystem::new().current_ids().0 != 0 {
        return;
    }

    let temp = TempDir::new().unwrap();
    let cgroup_dir = temp.path().join("cgroup");
    fs::create_dir_all(&cgroup_dir).unwrap();

    fs::write(
        cgroup_dir.join("cgroup.controllers"),
        "cpu io memory pids\n",
    )
    .unwrap();
    fs::write(cgroup_dir.join("cgroup.procs"), "1001\n1002\n").unwrap();
    fs::write(cgroup_dir.join("cgroup.subtree_control"), "").unwrap();

    let mut action = root_action("cg", ActionKind::CgroupV2Init);
    action.path = Some(cgroup_dir.display().to_string());
    action.subgroup = Some("init".to_string());
    action.controllers = Some(vec!["cpu".to_string(), "memory".to_string()]);

    let config = config(temp.path(), vec![action]);
    let plan = config.build_plan().unwrap();
    let report = executor(config.clone()).execute_plan(&plan).unwrap();
    assert!(report.succeeded());

    // Verify subgroup was created
    assert!(cgroup_dir.join("init").exists());

    // Verify subtree_control has the requested controllers enabled
    let subtree_content = fs::read_to_string(cgroup_dir.join("cgroup.subtree_control")).unwrap();
    assert!(subtree_content.contains("+cpu"));
    assert!(subtree_content.contains("+memory"));
    assert!(!subtree_content.contains("+io"));

    // Verify procs file in subgroup received pids
    let subgroup_procs = fs::read_to_string(cgroup_dir.join("init/cgroup.procs")).unwrap();
    assert!(!subgroup_procs.is_empty());

    // Idempotency: execute again
    let report2 = executor(config).execute_plan(&plan).unwrap();
    assert!(report2.succeeded());
}

#[test]
fn cgroup_v2_init_fails_on_unavailable_controller_or_missing_cgroup() {
    if PosixSystem::new().current_ids().0 != 0 {
        return;
    }

    let temp = TempDir::new().unwrap();
    let cgroup_dir = temp.path().join("cgroup");
    fs::create_dir_all(&cgroup_dir).unwrap();

    // 1. Missing cgroup.controllers
    let mut action = root_action("cg", ActionKind::CgroupV2Init);
    action.path = Some(cgroup_dir.display().to_string());
    let cfg = config(temp.path(), vec![action.clone()]);
    let plan = cfg.build_plan().unwrap();
    let err = executor(cfg).execute_plan(&plan).unwrap_err();
    assert!(err.to_string().contains("not a cgroup v2 hierarchy"));

    // 2. Request controller not in cgroup.controllers
    fs::write(cgroup_dir.join("cgroup.controllers"), "cpu memory\n").unwrap();
    fs::write(cgroup_dir.join("cgroup.subtree_control"), "").unwrap();
    action.controllers = Some(vec!["unsupported_controller".to_string()]);
    let cfg2 = config(temp.path(), vec![action]);
    let plan2 = cfg2.build_plan().unwrap();
    let err2 = executor(cfg2).execute_plan(&plan2).unwrap_err();
    assert!(err2.to_string().contains("not available"));
}

#[test]
fn cgroup_v2_init_bind_mount_mode_shadows_and_delegates() {
    if PosixSystem::new().current_ids().0 != 0 {
        return;
    }

    let temp = TempDir::new().unwrap();
    let target_dir = temp.path().join("sys_cgroup");
    let shadow_dir = temp.path().join("run_cgroup");
    fs::create_dir_all(&target_dir).unwrap();
    fs::create_dir_all(&shadow_dir).unwrap();

    fs::write(
        shadow_dir.join("cgroup.controllers"),
        "cpu io memory pids\n",
    )
    .unwrap();
    fs::write(shadow_dir.join("cgroup.procs"), "2001\n").unwrap();
    fs::write(shadow_dir.join("cgroup.subtree_control"), "").unwrap();

    let mut action = root_action("cg_bind", ActionKind::CgroupV2Init);
    action.path = Some(target_dir.display().to_string());
    action.shadow_path = Some(shadow_dir.display().to_string());
    action.mount_mode = Some("bind_mount".to_string());
    action.subgroup = Some("init".to_string());
    action.controllers = Some(vec!["cpu".to_string(), "pids".to_string()]);

    let config = config(temp.path(), vec![action]);
    let plan = config.build_plan().unwrap();
    let report = executor(config).execute_plan(&plan).unwrap();
    assert!(report.succeeded());

    // Verify shadow subgroup created
    assert!(shadow_dir.join("init").exists());

    // Verify shadow subtree_control
    let subtree = fs::read_to_string(shadow_dir.join("cgroup.subtree_control")).unwrap();
    assert!(subtree.contains("+cpu"));
    assert!(subtree.contains("+pids"));
}
