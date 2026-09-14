use bootstrap_model::{
    Action, ActionKind, BootstrapConfig, BootstrapMode, BootstrapPolicy, HandoffConfig,
    IdentityConfig, NonInteractivePolicy, Origin, RunAs,
};
use container_init_core::{
    ExecutionOptions, PlanExecutor, PosixSystem, RuntimeContext, SshCapability,
};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::TempDir;

fn fixture_config(root: &Path, actions: Vec<Action>) -> BootstrapConfig {
    let (uid, gid) = PosixSystem::new().current_ids();
    BootstrapConfig {
        schema: 1,
        mode: BootstrapMode::Strict,
        workspace_root: root.display().to_string(),
        allow_workspace_overlay: false,
        non_interactive: NonInteractivePolicy::Deny,
        identity: IdentityConfig {
            default_user: Some("root".to_owned()),
            default_uid: Some(uid),
            default_gid: Some(gid),
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
    resolve.run_as = RunAs::Root;
    let mut directory = action("directory", ActionKind::FilesystemEnsureDir);
    directory.path = Some(state.display().to_string());
    directory.mode = Some("0750".to_owned());
    directory.owner = Some("root".to_owned());
    directory.run_as = RunAs::Root;
    directory.depends_on = vec!["resolve".to_owned()];
    let mut marker = action("marker", ActionKind::FilesystemEnsureFile);
    marker.path = Some(file.display().to_string());
    marker.content = Some("docker integration\n".to_owned());
    marker.mode = Some("0640".to_owned());
    marker.owner = Some("root".to_owned());
    marker.run_as = RunAs::Root;
    marker.depends_on = vec!["directory".to_owned()];
    let mut symlink = action("link", ActionKind::FilesystemEnsureSymlink);
    symlink.link = Some(link.display().to_string());
    symlink.target = Some(file.display().to_string());
    symlink.owner = Some("root".to_owned());
    symlink.run_as = RunAs::Root;
    symlink.depends_on = vec!["marker".to_owned()];
    let mut chmod = action("chmod", ActionKind::FilesystemChmod);
    chmod.path = Some(state.display().to_string());
    chmod.mode = Some("0700".to_owned());
    chmod.run_as = RunAs::Root;
    chmod.depends_on = vec!["link".to_owned()];

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
    ssh.run_as = RunAs::Root;
    ssh.host_key_dir = Some(host_keys.display().to_string());
    ssh.authorized_keys_dir = Some(authorized_keys.display().to_string());
    ssh.runtime_dir = Some(runtime.display().to_string());
    ssh.host_key_types = Some(vec!["ed25519".to_owned()]);
    ssh.content = Some("ssh-ed25519 AAAAdocker-test\n".to_owned());

    let mut resolve = action("resolve", ActionKind::IdentityResolve);
    resolve.run_as = RunAs::Root;
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
