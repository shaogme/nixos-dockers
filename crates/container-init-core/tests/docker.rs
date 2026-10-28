use container_init_bootstrap_model::{
    Action, ActionKind, BootstrapConfig, HandoffConfig, IdentityConfig, Origin, RunAs,
};
use container_init_core::{
    ExecutionOptions, PlanExecutor, PosixSystem, RuntimeContext, SshCapability,
};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::Path};
use tempfile::TempDir;

fn fixture_config(root: &Path, actions: Vec<Action>) -> BootstrapConfig {
    let (uid, gid) = PosixSystem::new().current_ids();
    let mut identity = IdentityConfig::new();
    identity.set_default_user(Some("root".to_owned()));
    identity.set_default_uid(Some(uid));
    identity.set_default_gid(Some(gid));

    let mut handoff = HandoffConfig::new("/bin/sh");
    handoff.set_exec_prefix(vec!["-c".to_owned()]);
    handoff.set_initial_exec_prefix(vec!["-c".to_owned()]);
    handoff.set_shell_prefix(vec!["-c".to_owned(), "true".to_owned()]);

    BootstrapConfig::new(
        root.display().to_string(),
        identity,
        handoff,
        BTreeMap::new(),
        actions,
    )
}

fn action(id: &str, kind: ActionKind) -> Action {
    Action::new(id, kind, Origin::image("docker-fixture"))
}

#[test]
fn executes_real_linux_filesystem_primitives_as_root() {
    assert_eq!(std::env::consts::OS, "linux");
    assert_eq!(
        PosixSystem::new().current_ids().0,
        0,
        "Docker fixture must run as root"
    );

    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");
    let file = state.join("marker");
    let link = temp.path().join("state-link");
    let mut resolve = action("resolve", ActionKind::IdentityResolve);
    resolve.set_run_as(RunAs::Root);
    let mut directory = action("directory", ActionKind::FilesystemEnsureDir);
    directory.set_path(Some(state.display().to_string()));
    directory.set_mode(Some("0750".to_owned()));
    directory.set_owner(Some("root".to_owned()));
    directory.set_run_as(RunAs::Root);
    directory.set_depends_on(vec!["resolve".to_owned()]);
    let mut marker = action("marker", ActionKind::FilesystemEnsureFile);
    marker.set_path(Some(file.display().to_string()));
    marker.set_content(Some("docker integration\n".to_owned()));
    marker.set_mode(Some("0640".to_owned()));
    marker.set_owner(Some("root".to_owned()));
    marker.set_run_as(RunAs::Root);
    marker.set_depends_on(vec!["directory".to_owned()]);
    let mut symlink = action("link", ActionKind::FilesystemEnsureSymlink);
    symlink.set_link(Some(link.display().to_string()));
    symlink.set_target(Some(file.display().to_string()));
    symlink.set_owner(Some("root".to_owned()));
    symlink.set_run_as(RunAs::Root);
    symlink.set_depends_on(vec!["marker".to_owned()]);
    let mut chmod = action("chmod", ActionKind::FilesystemChmod);
    chmod.set_path(Some(state.display().to_string()));
    chmod.set_mode(Some("0700".to_owned()));
    chmod.set_run_as(RunAs::Root);
    chmod.set_depends_on(vec!["link".to_owned()]);

    let config = fixture_config(
        temp.path(),
        vec![chmod, symlink, marker, directory, resolve],
    );
    let plan = config.build_plan().unwrap();
    let runner = PlanExecutor::new(config, RuntimeContext::new(temp.path())).with_options(
        ExecutionOptions::default().with_receipt_path(temp.path().join("receipt.json")),
    );
    let report = runner.execute_plan(&plan).unwrap();
    assert!(report.succeeded());
    assert_eq!(fs::read_to_string(&file).unwrap(), "docker integration\n");
    assert_eq!(fs::read_link(&link).unwrap(), file);
    assert_eq!(
        fs::metadata(state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(temp.path().join("receipt.json").is_file());
}

#[test]
fn executes_real_openssh_preparation_and_is_idempotent() {
    assert_eq!(std::env::consts::OS, "linux");
    assert_eq!(
        PosixSystem::new().current_ids().0,
        0,
        "Docker fixture must run as root"
    );
    if !SshCapability::default().available() {
        return;
    }

    let temp = TempDir::new().unwrap();
    let host_keys = temp.path().join("etc/ssh");
    let authorized_keys = temp.path().join("etc/ssh/authorized_keys");
    let runtime = temp.path().join("run/sshd");
    let mut ssh = action("ssh", ActionKind::ServiceSshPrepare);
    ssh.set_run_as(RunAs::Root);
    ssh.set_host_key_dir(Some(host_keys.display().to_string()));
    ssh.set_authorized_keys_dir(Some(authorized_keys.display().to_string()));
    ssh.set_runtime_dir(Some(runtime.display().to_string()));
    ssh.set_host_key_types(Some(vec!["ed25519".to_owned()]));
    ssh.set_content(Some("ssh-ed25519 AAAAdocker-test\n".to_owned()));

    let mut resolve = action("resolve", ActionKind::IdentityResolve);
    resolve.set_run_as(RunAs::Root);
    let config = fixture_config(temp.path(), vec![ssh, resolve]);
    let plan = config.build_plan().unwrap();
    let runner = PlanExecutor::new(config, RuntimeContext::new(temp.path()))
        .with_options(ExecutionOptions::default().with_ssh(SshCapability::default()));
    let first = runner.execute_plan(&plan).unwrap();
    assert!(first.succeeded());
    let private = host_keys.join("ssh_host_ed25519_key");
    assert!(private.is_file());
    assert!(host_keys.join("ssh_host_ed25519_key.pub").is_file());
    assert!(runtime.is_dir());
    assert_eq!(
        fs::read_to_string(authorized_keys.join("root")).unwrap(),
        "ssh-ed25519 AAAAdocker-test\n"
    );
    let private_bytes = fs::read(&private).unwrap();

    let second = runner.execute_plan(&plan).unwrap();
    assert!(second.succeeded());
    assert_eq!(fs::read(private).unwrap(), private_bytes);
    assert!(second
        .outcome("ssh")
        .unwrap()
        .message
        .contains("0 host key(s) generated"));
}

#[test]
fn executes_real_cgroup_v2_init_as_root() {
    assert_eq!(std::env::consts::OS, "linux");
    assert_eq!(
        PosixSystem::new().current_ids().0,
        0,
        "Docker fixture must run as root"
    );

    let cgroup_base = Path::new("/sys/fs/cgroup");
    if !cgroup_base.join("cgroup.controllers").exists() {
        return;
    }
    // Skip if cgroup filesystem is mounted read-only (non-privileged container)
    let test_probe = cgroup_base.join("probe_cgroup_rw");
    if fs::create_dir(&test_probe).is_err() {
        return;
    }
    let _ = fs::remove_dir(&test_probe);

    let temp = TempDir::new().unwrap();
    let mut resolve = action("resolve", ActionKind::IdentityResolve);
    resolve.set_run_as(RunAs::Root);

    // Test 1: Real cgroup v2 init on /sys/fs/cgroup with default subgroup "libpod_parent"
    let mut cg = action("cg-init", ActionKind::CgroupV2Init);
    cg.set_run_as(RunAs::Root);
    cg.set_subgroup(Some("libpod_parent".to_owned()));
    cg.set_depends_on(vec!["resolve".to_owned()]);

    let config = fixture_config(temp.path(), vec![cg, resolve.clone()]);
    let plan = config.build_plan().unwrap();
    let runner = PlanExecutor::new(config.clone(), RuntimeContext::new(temp.path())).with_options(
        ExecutionOptions::default().with_receipt_path(temp.path().join("receipt.json")),
    );
    let report = runner.execute_plan(&plan).unwrap();
    assert!(report.succeeded());

    // Verify /sys/fs/cgroup/init exists
    assert!(cgroup_base.join("init").is_dir());

    // Verify controllers in subtree_control
    let controllers = fs::read_to_string(cgroup_base.join("cgroup.controllers")).unwrap();
    let subtree = fs::read_to_string(cgroup_base.join("cgroup.subtree_control")).unwrap();
    for ctrl in controllers.split_whitespace() {
        assert!(
            subtree.contains(ctrl),
            "expected {ctrl} in subtree_control, got: {subtree}"
        );
    }

    // Verify receipt was written
    let receipt = fs::read_to_string(temp.path().join("receipt.json")).unwrap();
    assert!(receipt.contains("\"cg-init\""));

    // Verify idempotency
    let report2 = runner.execute_plan(&plan).unwrap();
    assert!(report2.succeeded());

    // Test 2: Nested child cgroup hierarchy with custom subgroup
    let child_cgroup = cgroup_base.join("docker_test_child_cg");
    fs::create_dir_all(&child_cgroup).unwrap();

    let mut nested_cg = action("nested-cg", ActionKind::CgroupV2Init);
    nested_cg.set_run_as(RunAs::Root);
    nested_cg.set_path(Some(child_cgroup.display().to_string()));
    nested_cg.set_subgroup(Some("child_worker".to_owned()));
    nested_cg.set_controllers(Some(vec!["memory".to_owned(), "pids".to_owned()]));
    nested_cg.set_depends_on(vec!["resolve".to_owned()]);

    let nested_config = fixture_config(temp.path(), vec![nested_cg, resolve]);
    let nested_plan = nested_config.build_plan().unwrap();
    let nested_runner = PlanExecutor::new(nested_config, RuntimeContext::new(temp.path()))
        .with_options(
            ExecutionOptions::default().with_receipt_path(temp.path().join("receipt_nested.json")),
        );
    let nested_report = nested_runner.execute_plan(&nested_plan).unwrap();
    assert!(nested_report.succeeded());

    assert!(child_cgroup.join("child_worker").is_dir());
    let child_subtree = fs::read_to_string(child_cgroup.join("cgroup.subtree_control")).unwrap();
    assert!(child_subtree.contains("memory"));
    assert!(child_subtree.contains("pids"));

    // Cleanup child cgroup
    let _ = fs::remove_dir(child_cgroup.join("child_worker"));
    let _ = fs::remove_dir(&child_cgroup);
}

#[test]
fn executes_real_cgroup_v2_init_bind_mount_shadowing_as_root() {
    let posix_system = PosixSystem::new();
    if posix_system.current_ids().0 != 0 {
        return;
    }

    let temp = TempDir::new().unwrap();
    let shadow_path = temp.path().join("shadow_cg");
    let target_path = temp.path().join("target_cg");
    fs::create_dir_all(&target_path).unwrap();

    let mut resolve = action("resolve", ActionKind::IdentityResolve);
    resolve.set_run_as(RunAs::Root);

    let mut cg = action("cg-bind", ActionKind::CgroupV2Init);
    cg.set_run_as(RunAs::Root);
    cg.set_mount_mode(Some("bind_mount".to_owned()));
    cg.set_shadow_path(Some(shadow_path.display().to_string()));
    cg.set_path(Some(target_path.display().to_string()));
    cg.set_subgroup(Some("libpod_parent".to_owned()));
    cg.set_controllers(Some(vec!["pids".to_owned()]));
    cg.set_depends_on(vec!["resolve".to_owned()]);

    let config = fixture_config(temp.path(), vec![cg, resolve]);
    let plan = config.build_plan().unwrap();
    let runner = PlanExecutor::new(config, RuntimeContext::new(temp.path()));
    let report = runner.execute_plan(&plan).unwrap();
    assert!(report.succeeded());

    // Verify shadow_path and target_path both reflect the initialized subgroup and subtree_control
    assert!(shadow_path.join("init").is_dir());
    assert!(target_path.join("init").is_dir());
    let target_subtree = fs::read_to_string(target_path.join("cgroup.subtree_control")).unwrap();
    assert!(target_subtree.contains("pids"));
}

#[test]
fn executes_real_cgroup_v2_init_bind_mount_shadowing_unprivileged_non_root() {
    let posix_system = PosixSystem::new();
    if posix_system.current_ids().0 != 0 {
        return;
    }

    let temp = TempDir::new().unwrap();
    let shadow_path = temp.path().join("non_existent_shadow/cg");
    let target_path = Path::new("/sys/fs/cgroup");

    let mut resolve = action("resolve", ActionKind::IdentityResolve);
    resolve.set_run_as(RunAs::Root);

    let mut cg = action("cg-bind", ActionKind::CgroupV2Init);
    cg.set_run_as(RunAs::Root);
    cg.set_mount_mode(Some("bind_mount".to_owned()));
    cg.set_shadow_path(Some(shadow_path.display().to_string()));
    cg.set_path(Some(target_path.display().to_string()));
    cg.set_depends_on(vec!["resolve".to_owned()]);

    let mut config = fixture_config(temp.path(), vec![cg, resolve]);
    config
        .identity_mut()
        .set_default_user(Some("nobody".to_owned()));
    config.identity_mut().set_default_uid(Some(65534));
    config.identity_mut().set_default_gid(Some(65534));

    let plan = config.build_plan().unwrap();
    let runner = PlanExecutor::new(config, RuntimeContext::new(temp.path()));
    let report = runner.execute_plan(&plan).unwrap();
    assert!(report.succeeded());
}

#[test]
fn executes_real_home_ownership_reconciliation_between_root_and_dev() {
    use std::os::unix::fs::MetadataExt;

    assert_eq!(std::env::consts::OS, "linux");
    assert_eq!(
        PosixSystem::new().current_ids().0,
        0,
        "Docker fixture must run as root"
    );

    let temp = TempDir::new().unwrap();
    let user_home = temp.path().join("home/dev");

    // 1. Bootstrap as dev (UID 1000, GID 1000)
    let mut resolve_dev = action("resolve", ActionKind::IdentityResolve);
    resolve_dev.set_run_as(RunAs::Root);
    let mut ensure_home_dev = action("ensure-home", ActionKind::IdentityEnsureHome);
    ensure_home_dev.set_path(Some(user_home.display().to_string()));
    ensure_home_dev.set_owner(Some("identity.target".to_owned()));
    ensure_home_dev.set_mode(Some("0755".to_owned()));
    ensure_home_dev.set_run_as(RunAs::Root);
    ensure_home_dev.set_depends_on(vec!["resolve".to_owned()]);

    let mut config_dev = fixture_config(temp.path(), vec![ensure_home_dev, resolve_dev]);
    config_dev.identity_mut().set_default_uid(Some(1000));
    config_dev.identity_mut().set_default_gid(Some(1000));
    config_dev
        .identity_mut()
        .set_default_user(Some("dev".to_owned()));
    config_dev
        .identity_mut()
        .set_default_home(Some(user_home.display().to_string()));

    let plan_dev = config_dev.build_plan().unwrap();
    let runner_dev = PlanExecutor::new(config_dev.clone(), RuntimeContext::new(temp.path()));
    let report_dev = runner_dev.execute_plan(&plan_dev).unwrap();
    assert!(report_dev.succeeded());

    // Verify /home/dev is owned by dev (1000:1000)
    let meta_dev = fs::metadata(&user_home).unwrap();
    assert_eq!(
        (meta_dev.uid(), meta_dev.gid()),
        (1000, 1000),
        "Home must be owned by dev (1000:1000)"
    );

    // 2. Now enter/bootstrap as root (UID 0, GID 0)
    let mut resolve_root = action("resolve", ActionKind::IdentityResolve);
    resolve_root.set_run_as(RunAs::Root);
    let mut ensure_home_root = action("ensure-home", ActionKind::IdentityEnsureHome);
    ensure_home_root.set_path(Some(user_home.display().to_string()));
    ensure_home_root.set_owner(Some("identity.target".to_owned()));
    ensure_home_root.set_mode(Some("0755".to_owned()));
    ensure_home_root.set_run_as(RunAs::Root);
    ensure_home_root.set_depends_on(vec!["resolve".to_owned()]);

    let mut config_root = fixture_config(temp.path(), vec![ensure_home_root, resolve_root]);
    config_root.identity_mut().set_default_uid(Some(0));
    config_root.identity_mut().set_default_gid(Some(0));
    config_root
        .identity_mut()
        .set_default_user(Some("root".to_owned()));
    config_root
        .identity_mut()
        .set_default_home(Some(user_home.display().to_string()));

    let plan_root = config_root.build_plan().unwrap();
    let runner_root = PlanExecutor::new(config_root, RuntimeContext::new(temp.path()));
    let report_root = runner_root.execute_plan(&plan_root).unwrap();
    assert!(report_root.succeeded());

    // Verify /home/dev is now automatically reconciled and owned by root (0:0)
    let meta_root = fs::metadata(&user_home).unwrap();
    assert_eq!(
        (meta_root.uid(), meta_root.gid()),
        (0, 0),
        "Home must be reconciled to root (0:0)"
    );

    // 3. Switch back to dev, verify ownership is reconciled back to dev (1000:1000)
    let report_dev2 = runner_dev.execute_plan(&plan_dev).unwrap();
    assert!(report_dev2.succeeded());
    let meta_dev2 = fs::metadata(&user_home).unwrap();
    assert_eq!(
        (meta_dev2.uid(), meta_dev2.gid()),
        (1000, 1000),
        "Home must be reconciled back to dev (1000:1000)"
    );
}
