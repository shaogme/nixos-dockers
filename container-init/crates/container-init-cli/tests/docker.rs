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

#[test]
fn runs_cli_cgroup_v2_init_bind_mount_shadowing_as_root() {
    let temp = TempDir::new().unwrap();
    let profiles = temp.path().join("profiles");
    let workspace = temp.path().join("workspace");
    let lock = temp.path().join("lock");
    let receipt = temp.path().join("receipt.json");
    let handoff = workspace.join("handoff");
    let shadow_cgroup = temp.path().join("shadow_cg");
    let target_cgroup = temp.path().join("target_cg");
    fs::create_dir_all(&profiles).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&target_cgroup).unwrap();

    fs::write(
        profiles.join("10-cgroup-bind.toml"),
        format!(
            r#"
schema = 1
id = "cgroup-bind"

[bootstrap]
schema = 1
mode = "strict"
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
id = "cgroup-bind-init"
kind = "cgroup.v2_init"
run_as = "root"
mount_mode = "bind_mount"
shadow_path = "{}"
path = "{}"
subgroup = "worker"
depends_on = ["resolve"]
"#,
            workspace.display(),
            shadow_cgroup.display(),
            target_cgroup.display(),
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
        "cgroup-bind",
        "--workspace",
        workspace.to_str().unwrap(),
        "--lock-path",
        lock.to_str().unwrap(),
        "--receipt-path",
        receipt.to_str().unwrap(),
    ];

    let script = format!("printf 'cgroup bind handoff\\n' > {}", handoff.display());
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
    assert!(target_cgroup.join("worker").is_dir());
    assert_eq!(
        fs::read_to_string(&handoff).unwrap(),
        "cgroup bind handoff\n"
    );
    let receipt_str = fs::read_to_string(&receipt).unwrap();
    assert!(receipt_str.contains("\"cgroup-bind-init\""));
}

#[test]
fn cli_reconciles_home_ownership_between_root_and_dev() {
    use std::os::unix::fs::MetadataExt;

    assert_eq!(std::env::consts::OS, "linux");
    assert_eq!(
        container_init_core::PosixSystem::new().current_ids().0,
        0,
        "Docker fixture must run as root"
    );

    let temp = TempDir::new().unwrap();
    let profiles = temp.path().join("profiles");
    let workspace = temp.path().join("workspace");
    let user_home = temp.path().join("home/user");
    let lock = temp.path().join("bootstrap.lock");
    let receipt = temp.path().join("receipt.json");
    fs::create_dir(&profiles).unwrap();
    fs::create_dir(&workspace).unwrap();

    fs::write(
        profiles.join("reconcile.toml"),
        format!(
            r#"
schema = 1
id = "reconcile"

[bootstrap]
workspace_root = "{}"

[bootstrap.identity]
default_user = "dev"
default_uid = 1000
default_gid = 1000
default_home = "{}"
auto_mapping = false
run_as_root_input = "RUN_AS_ROOT"

[bootstrap.inputs.RUN_AS_ROOT]
target = "identity.run_as_root"
type = "bool"
runtime = true
default = false

[bootstrap.handoff]
runtime = "/bin/sh"
exec_prefix = ["-c"]
shell_prefix = ["-c", "true"]

[[bootstrap.actions]]
id = "resolve"
kind = "identity.resolve"
run_as = "root"

[[bootstrap.actions]]
id = "ensure-home"
kind = "identity.ensure_home"
path = "{}"
mode = "0755"
owner = "identity.target"
run_as = "root"
depends_on = ["resolve"]
"#,
            workspace.display(),
            user_home.display(),
            user_home.display()
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

    let common_args = [
        "--profiles-dir",
        profiles.to_str().unwrap(),
        "--profile",
        "reconcile",
        "--workspace",
        workspace.to_str().unwrap(),
        "--lock-path",
        lock.to_str().unwrap(),
        "--receipt-path",
        receipt.to_str().unwrap(),
    ];

    // 1. Run container-init as dev (default, RUN_AS_ROOT is false)
    let run_dev = Command::new(&binary)
        .args(common_args)
        .args(["run", "--", "true"])
        .output()
        .unwrap();
    assert!(
        run_dev.status.success(),
        "{}",
        String::from_utf8_lossy(&run_dev.stderr)
    );
    let meta_dev = fs::metadata(&user_home).unwrap();
    assert_eq!(
        (meta_dev.uid(), meta_dev.gid()),
        (1000, 1000),
        "Home must be owned by dev (1000:1000)"
    );

    // 2. Run container-init with RUN_AS_ROOT=1
    let run_root = Command::new(&binary)
        .args(common_args)
        .env("RUN_AS_ROOT", "1")
        .args(["run", "--", "true"])
        .output()
        .unwrap();
    assert!(
        run_root.status.success(),
        "{}",
        String::from_utf8_lossy(&run_root.stderr)
    );
    let meta_root = fs::metadata(&user_home).unwrap();
    assert_eq!(
        (meta_root.uid(), meta_root.gid()),
        (0, 0),
        "Home must be reconciled to root (0:0)"
    );

    // 3. Run container-init again as dev (RUN_AS_ROOT=0)
    let run_dev2 = Command::new(&binary)
        .args(common_args)
        .env("RUN_AS_ROOT", "0")
        .args(["run", "--", "true"])
        .output()
        .unwrap();
    assert!(
        run_dev2.status.success(),
        "{}",
        String::from_utf8_lossy(&run_dev2.stderr)
    );
    let meta_dev2 = fs::metadata(&user_home).unwrap();
    assert_eq!(
        (meta_dev2.uid(), meta_dev2.gid()),
        (1000, 1000),
        "Home must be reconciled back to dev (1000:1000)"
    );

    // 4. Exec with RUN_AS_ROOT=1 (exec promotes because of identity drift)
    let exec_root = Command::new(&binary)
        .args(common_args)
        .env("RUN_AS_ROOT", "1")
        .args(["exec", "--", "true"])
        .output()
        .unwrap();
    assert!(
        exec_root.status.success(),
        "{}",
        String::from_utf8_lossy(&exec_root.stderr)
    );
    let meta_root2 = fs::metadata(&user_home).unwrap();
    assert_eq!(
        (meta_root2.uid(), meta_root2.gid()),
        (0, 0),
        "Exec must reconcile home to root (0:0) on identity drift"
    );

    // 5. Exec with RUN_AS_ROOT=0 (exec promotes because identity drifted to dev)
    let exec_dev = Command::new(&binary)
        .args(common_args)
        .env("RUN_AS_ROOT", "0")
        .args(["exec", "--", "true"])
        .output()
        .unwrap();
    assert!(
        exec_dev.status.success(),
        "{}",
        String::from_utf8_lossy(&exec_dev.stderr)
    );
    let meta_dev3 = fs::metadata(&user_home).unwrap();
    assert_eq!(
        (meta_dev3.uid(), meta_dev3.gid()),
        (1000, 1000),
        "Exec must reconcile home back to dev (1000:1000) on identity drift"
    );

    // 6. Exec again with RUN_AS_ROOT=0 (no drift: already dev)
    // Remove receipt first to prove it wasn't regenerated / actions were skipped
    fs::remove_file(&receipt).unwrap();
    let exec_dev_nodrift = Command::new(&binary)
        .args(common_args)
        .env("RUN_AS_ROOT", "0")
        .args(["exec", "--", "true"])
        .output()
        .unwrap();
    assert!(
        exec_dev_nodrift.status.success(),
        "{}",
        String::from_utf8_lossy(&exec_dev_nodrift.stderr)
    );
    assert!(
        !receipt.exists(),
        "Exec with no drift must not run bootstrap actions or write receipt"
    );
}
