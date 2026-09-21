use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn cli_runs_a_real_linux_bootstrap_plan_and_handoff() {
    assert_eq!(std::env::consts::OS, "linux");
    assert_eq!(
        container_init_core::PosixSystem::new().current_ids().0,
        0,
        "Docker fixture must run as root"
    );
    let temp = TempDir::new().unwrap();
    let profiles = temp.path().join("profiles");
    let workspace = temp.path().join("workspace");
    let marker = workspace.join("marker");
    let handoff = workspace.join("handoff");
    let lock = temp.path().join("bootstrap.lock");
    let receipt = temp.path().join("receipt.json");
    fs::create_dir(&profiles).unwrap();
    fs::create_dir(&workspace).unwrap();
    fs::write(
        profiles.join("base.toml"),
        format!(
            r#"
schema = 1
id = "docker"

[bootstrap]
workspace_root = "{}"

[bootstrap.identity]
default_user = "root"
default_uid = 0
default_gid = 0
auto_mapping = false

[bootstrap.handoff]
runtime = "/bin/sh"
exec_prefix = ["-c"]
shell_prefix = ["-c", "true"]

[[bootstrap.actions]]
id = "resolve"
kind = "identity.resolve"
run_as = "root"

[[bootstrap.actions]]
id = "directory"
kind = "filesystem.ensure_dir"
path = "{}/state"
mode = "0750"
owner = "root"
run_as = "root"
depends_on = ["resolve"]

[[bootstrap.actions]]
id = "marker"
kind = "filesystem.ensure_file"
path = "{}"
content = "docker cli\n"
mode = "0640"
owner = "root"
run_as = "root"
depends_on = ["directory"]
"#,
            workspace.display(),
            workspace.display(),
            marker.display()
        ),
    )
    .unwrap();

    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("container-init");
    let plan = Command::new(&binary)
        .args([
            "--profiles-dir",
            profiles.to_str().unwrap(),
            "--profile",
            "docker",
            "--workspace",
            workspace.to_str().unwrap(),
            "--lock-path",
            lock.to_str().unwrap(),
            "plan",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan: Value = serde_json::from_slice(&plan.stdout).unwrap();
    let actions = plan["actions"].as_array().unwrap();
    assert_eq!(
        actions
            .iter()
            .map(|action| action["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["resolve", "directory", "marker"]
    );
    assert!(!marker.exists(), "plan must not execute actions");

    let script = format!("printf 'real handoff\\n' > {}", handoff.display());
    let run = Command::new(&binary)
        .args([
            "--profiles-dir",
            profiles.to_str().unwrap(),
            "--profile",
            "docker",
            "--workspace",
            workspace.to_str().unwrap(),
            "--lock-path",
            lock.to_str().unwrap(),
            "--receipt-path",
            receipt.to_str().unwrap(),
            "run",
            "--",
        ])
        .arg(script)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), "docker cli\n");
    assert_eq!(fs::read_to_string(&handoff).unwrap(), "real handoff\n");
    assert_eq!(
        fs::metadata(workspace.join("state"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert!(fs::read_to_string(receipt).unwrap().contains("\"resolve\""));
}

#[test]
fn cli_runs_a_real_ssh_bootstrap_action_before_handoff() {
    assert_eq!(std::env::consts::OS, "linux");
    assert_eq!(
        container_init_core::PosixSystem::new().current_ids().0,
        0,
        "Docker fixture must run as root"
    );
    if !container_init_core::SshCapability::default().available() {
        return;
    }

    let temp = TempDir::new().unwrap();
    let profiles = temp.path().join("profiles");
    let workspace = temp.path().join("workspace");
    let host_keys = temp.path().join("etc/ssh");
    let authorized_keys = temp.path().join("etc/ssh/authorized_keys");
    let runtime = temp.path().join("run/sshd");
    let handoff = workspace.join("handoff");
    let lock = temp.path().join("bootstrap.lock");
    fs::create_dir(&profiles).unwrap();
    fs::create_dir(&workspace).unwrap();
    fs::write(
        profiles.join("ssh.toml"),
        format!(
            r#"
schema = 1
id = "ssh"

[bootstrap]
workspace_root = "{}"

[bootstrap.identity]
default_user = "root"
default_uid = 0
default_gid = 0

[bootstrap.handoff]
runtime = "/bin/sh"
exec_prefix = ["-c"]
shell_prefix = ["-c", "true"]

[[bootstrap.actions]]
id = "resolve"
kind = "identity.resolve"
run_as = "root"

[[bootstrap.actions]]
id = "prepare-ssh"
kind = "service.ssh.prepare"
run_as = "root"
host_key_dir = "{}"
authorized_keys_dir = "{}"
runtime_dir = "{}"
host_key_types = ["ed25519"]
content = "ssh-ed25519 AAAAdocker-cli-test\n"
depends_on = ["resolve"]
"#,
            workspace.display(),
            host_keys.display(),
            authorized_keys.display(),
            runtime.display()
        ),
    )
    .unwrap();

    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("container-init");
    let common = [
        "--profiles-dir",
        profiles.to_str().unwrap(),
        "--profile",
        "ssh",
        "--workspace",
        workspace.to_str().unwrap(),
        "--lock-path",
        lock.to_str().unwrap(),
    ];
    let plan = Command::new(&binary)
        .args(common)
        .args(["plan", "--json"])
        .output()
        .unwrap();
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan: Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(plan["actions"][1]["id"], "prepare-ssh");
    assert!(!host_keys.exists(), "plan must not generate host keys");

    let script = format!("printf 'docker ssh handoff\\n' > {}", handoff.display());
    let run = Command::new(&binary)
        .args(common)
        .args(["run", "--"])
        .arg(script)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(host_keys.join("ssh_host_ed25519_key").is_file());
    assert!(host_keys.join("ssh_host_ed25519_key.pub").is_file());
    assert_eq!(
        fs::read_to_string(authorized_keys.join("root")).unwrap(),
        "ssh-ed25519 AAAAdocker-cli-test\n"
    );
    assert!(runtime.is_dir());
    assert_eq!(fs::read_to_string(handoff).unwrap(), "docker ssh handoff\n");
}

#[test]
fn cli_runs_a_real_cgroup_v2_init_bootstrap_action_before_handoff() {
    assert_eq!(std::env::consts::OS, "linux");
    assert_eq!(
        container_init_core::PosixSystem::new().current_ids().0,
        0,
        "Docker fixture must run as root"
    );

    let cgroup_base = std::path::Path::new("/sys/fs/cgroup");
    if !cgroup_base.join("cgroup.controllers").exists() {
        return;
    }
    let test_probe = cgroup_base.join("probe_cgroup_cli_rw");
    if fs::create_dir(&test_probe).is_err() {
        return;
    }
    let _ = fs::remove_dir(&test_probe);

    let temp = TempDir::new().unwrap();
    let profiles = temp.path().join("profiles");
    let workspace = temp.path().join("workspace");
    let handoff = workspace.join("handoff");
    let lock = temp.path().join("bootstrap.lock");
    let receipt = temp.path().join("receipt.json");
    fs::create_dir(&profiles).unwrap();
    fs::create_dir(&workspace).unwrap();

    let child_cgroup = cgroup_base.join("docker_cli_test_cg");
    fs::create_dir_all(&child_cgroup).unwrap();

    fs::write(
        profiles.join("cgroup.toml"),
        format!(
            r#"
schema = 1
id = "cgroup"

[bootstrap]
workspace_root = "{}"

[bootstrap.identity]
default_user = "root"
default_uid = 0
default_gid = 0

[bootstrap.handoff]
runtime = "/bin/sh"
exec_prefix = ["-c"]
shell_prefix = ["-c", "true"]

[[bootstrap.actions]]
id = "resolve"
kind = "identity.resolve"
run_as = "root"

[[bootstrap.actions]]
id = "cgroup-init"
kind = "cgroup.v2_init"
run_as = "root"
path = "{}"
subgroup = "worker"
depends_on = ["resolve"]
"#,
            workspace.display(),
            child_cgroup.display()
        ),
    )
    .unwrap();

    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("container-init");
    let common = [
        "--profiles-dir",
        profiles.to_str().unwrap(),
        "--profile",
        "cgroup",
        "--workspace",
        workspace.to_str().unwrap(),
        "--lock-path",
        lock.to_str().unwrap(),
        "--receipt-path",
        receipt.to_str().unwrap(),
    ];

    let plan = Command::new(&binary)
        .args(common)
        .args(["plan", "--json"])
        .output()
        .unwrap();
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan_doc: Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(plan_doc["actions"][1]["id"], "cgroup-init");

    let script = format!("printf 'cgroup handoff\\n' > {}", handoff.display());
    let run = Command::new(&binary)
        .args(common)
        .args(["run", "--"])
        .arg(script)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(child_cgroup.join("worker").is_dir());
    assert_eq!(fs::read_to_string(&handoff).unwrap(), "cgroup handoff\n");
    let receipt_str = fs::read_to_string(&receipt).unwrap();
    assert!(receipt_str.contains("\"cgroup-init\""));

    // Cleanup
    let _ = fs::remove_dir(child_cgroup.join("worker"));
    let _ = fs::remove_dir(&child_cgroup);
}
