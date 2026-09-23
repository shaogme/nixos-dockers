use bootstrap_loader::LoaderError;
use bootstrap_model::ModelError;
use container_init_cli::{Cli, CliCommand, CliError};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn binary() -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("container-init")
}

fn fake_ssh_keygen(root: &Path) -> PathBuf {
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

fn profile_fixture() -> (TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
    let temp = TempDir::new().expect("temporary directory should be created");
    let profiles = temp.path().join("profiles");
    let workspace = temp.path().join("workspace");
    let marker = workspace.join("marker");
    let handoff = workspace.join("handoff");
    fs::create_dir(&profiles).unwrap();
    fs::create_dir(&workspace).unwrap();
    let profile = format!(
        r#"
schema = 1
id = "fixture"

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
id = "marker"
kind = "filesystem.ensure_file"
path = "{}"
content = "created by container-init\n"
mode = "0640"
owner = "root"
run_as = "root"
"#,
        workspace.display(),
        marker.display()
    );
    fs::write(profiles.join("fixture.toml"), profile).unwrap();
    (temp, profiles, workspace, marker, handoff)
}

fn common_args(profiles: &Path, workspace: &Path, lock: &Path) -> Vec<String> {
    vec![
        "--profiles-dir".to_owned(),
        profiles.display().to_string(),
        "--profile".to_owned(),
        "fixture".to_owned(),
        "--workspace".to_owned(),
        workspace.display().to_string(),
        "--lock-path".to_owned(),
        lock.display().to_string(),
    ]
}

#[test]
fn parser_keeps_command_arguments_after_double_dash() {
    let cli = Cli::parse_strings([
        "container-init",
        "--profiles-dir",
        "/profiles",
        "run",
        "--",
        "tool",
        "--profile",
        "value",
    ])
    .unwrap();
    assert_eq!(cli.options.profiles_dir, Some(PathBuf::from("/profiles")));
    assert_eq!(
        cli.command,
        CliCommand::Run {
            command: vec![
                "tool".to_owned(),
                "--profile".to_owned(),
                "value".to_owned()
            ]
        }
    );
}

#[test]
fn plan_is_side_effect_free_and_json_contains_provenance() {
    if container_init_core::PosixSystem::new().current_ids().0 != 0 {
        return;
    }
    let (temp, profiles, workspace, marker, _) = profile_fixture();
    let lock = temp.path().join("lock");
    let output = Command::new(binary())
        .args(common_args(&profiles, &workspace, &lock))
        .args(["plan", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["profile"], "fixture");
    assert_eq!(document["actions"][0]["id"], "marker");
    assert_eq!(document["actions"][0]["origin"]["profile"], "fixture");
    assert!(!marker.exists());
    assert!(!lock.exists());
}

#[test]
fn doctor_checks_the_real_handoff_and_workspace_without_mutation() {
    if container_init_core::PosixSystem::new().current_ids().0 != 0 {
        return;
    }
    let (temp, profiles, workspace, marker, _) = profile_fixture();
    let lock = temp.path().join("lock");
    let output = Command::new(binary())
        .args(common_args(&profiles, &workspace, &lock))
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["ok"], true);
    assert_eq!(document["handoff"]["runtime"], "/bin/sh");
    assert!(document["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| { check["name"] == "workspace" && check["status"] == "pass" }));
    assert!(!marker.exists());
    assert!(!lock.exists());
}

#[test]
fn run_executes_actions_then_handoffs_to_the_declared_runtime() {
    if container_init_core::PosixSystem::new().current_ids().0 != 0 {
        return;
    }
    let (temp, profiles, workspace, marker, handoff) = profile_fixture();
    let lock = temp.path().join("lock");
    let receipt = temp.path().join("receipt.json");
    let script = format!("printf 'handoff\\n' > {}", handoff.display());
    let output = Command::new(binary())
        .args(common_args(&profiles, &workspace, &lock))
        .args(["--receipt-path", receipt.to_str().unwrap(), "run", "--"])
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&marker).unwrap(),
        "created by container-init\n"
    );
    assert_eq!(fs::read_to_string(&handoff).unwrap(), "handoff\n");
    assert_eq!(
        fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let receipt = fs::read_to_string(receipt).unwrap();
    assert!(receipt.contains("\"marker\""));
    assert!(!receipt.contains("created by container-init"));
}

#[test]
fn parser_parses_exec_command_with_double_dash_and_arguments() {
    let cli = Cli::parse_strings([
        "container-init",
        "--profiles-dir",
        "/profiles",
        "exec",
        "--",
        "tool",
        "--flag",
        "value",
    ])
    .unwrap();
    assert_eq!(cli.options.profiles_dir, Some(PathBuf::from("/profiles")));
    assert_eq!(
        cli.command,
        CliCommand::Exec {
            command: vec!["tool".to_owned(), "--flag".to_owned(), "value".to_owned(),]
        }
    );
}

#[test]
fn exec_hands_off_without_bootstrap_actions_or_lock_or_receipt() {
    let (temp, profiles, workspace, marker, handoff) = profile_fixture();
    let lock = temp.path().join("lock");
    let receipt = temp.path().join("receipt.json");
    let script = format!("printf 'exec-handoff\\n' > {}", handoff.display());
    let output = Command::new(binary())
        .args(common_args(&profiles, &workspace, &lock))
        .args(["--receipt-path", receipt.to_str().unwrap(), "exec", "--"])
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!marker.exists(), "exec must not execute bootstrap actions");
    assert!(
        !receipt.exists(),
        "exec must not write an execution receipt"
    );
    assert!(!lock.exists(), "exec must not create or acquire a lock");
    assert_eq!(fs::read_to_string(&handoff).unwrap(), "exec-handoff\n");
}

#[test]
fn exec_supports_parallel_execution_without_lock_contention() {
    let (temp, profiles, workspace, _marker, _handoff) = profile_fixture();
    let lock = temp.path().join("lock");
    let mut handles = Vec::new();
    for i in 0..4 {
        let profiles = profiles.clone();
        let workspace = workspace.clone();
        let lock = lock.clone();
        let out_file = temp.path().join(format!("parallel-{i}"));
        handles.push(std::thread::spawn(move || {
            let script = format!("printf '{i}\\n' > {}", out_file.display());
            let output = Command::new(binary())
                .args(common_args(&profiles, &workspace, &lock))
                .args(["exec", "--"])
                .arg(script)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "parallel exec {i} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(fs::read_to_string(&out_file).unwrap(), format!("{i}\n"));
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn run_exports_resolved_identity_environment_before_non_root_handoff() {
    if container_init_core::PosixSystem::new().current_ids().0 != 0 {
        return;
    }

    let temp = TempDir::new().unwrap();
    let profiles = temp.path().join("profiles");
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    let lock = temp.path().join("lock");
    fs::create_dir(&profiles).unwrap();
    fs::create_dir(&workspace).unwrap();
    fs::write(
        profiles.join("identity.toml"),
        format!(
            r#"
schema = 1
id = "identity"

[bootstrap]
workspace_root = "{}"

[bootstrap.identity]
default_user = "nobody"
default_uid = 65534
default_gid = 65534
auto_mapping = false
home_input = "CONTAINER_HOME"

[bootstrap.inputs.CONTAINER_HOME]
target = "identity.home"
type = "path"
runtime = false
default = "{}"
allow_outside_workspace = true

[bootstrap.handoff]
runtime = "/bin/sh"
exec_prefix = ["-c"]
shell_prefix = ["-c", "true"]

[[bootstrap.actions]]
id = "resolve"
kind = "identity.resolve"
run_as = "root"

[[bootstrap.actions]]
id = "home"
kind = "identity.ensure_home"
path = "{}"
mode = "0755"
owner = "identity.target"
run_as = "root"
depends_on = ["resolve"]

[[bootstrap.actions]]
id = "drop"
kind = "process.drop_privileges"
run_as = "root"
depends_on = ["home"]

[[bootstrap.actions]]
id = "handoff"
kind = "handoff.exec"
run_as = "current"
depends_on = ["drop"]
"#,
            workspace.display(),
            home.display(),
            home.display()
        ),
    )
    .unwrap();
    let script =
        "printf 'HOME=%s USER=%s LOGNAME=%s UID=%s GID=%s\\n' \"$HOME\" \"$USER\" \"$LOGNAME\" \"$(id -u)\" \"$(id -g)\"";
    let output = Command::new(binary())
        .args([
            "--profiles-dir",
            profiles.to_str().unwrap(),
            "--profile",
            "identity",
            "--workspace",
            workspace.to_str().unwrap(),
            "--lock-path",
            lock.to_str().unwrap(),
            "run",
            "--",
        ])
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "HOME={} USER=nobody LOGNAME=nobody UID=65534 GID=65534\n",
            home.display()
        )
    );
}

#[test]
fn cli_enables_the_ssh_capability_for_a_declared_service_action() {
    if container_init_core::PosixSystem::new().current_ids().0 != 0 {
        return;
    }

    let temp = TempDir::new().unwrap();
    let profiles = temp.path().join("profiles");
    let workspace = temp.path().join("workspace");
    let host_keys = temp.path().join("ssh");
    let authorized_keys = temp.path().join("authorized");
    let runtime = temp.path().join("run");
    let keygen = fake_ssh_keygen(temp.path());
    let handoff = workspace.join("handoff");
    let lock = temp.path().join("lock");
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
id = "ssh"
kind = "service.ssh.prepare"
run_as = "root"
host_key_dir = "{}"
authorized_keys_dir = "{}"
runtime_dir = "{}"
host_key_types = ["ed25519"]
ssh_keygen = "{}"
content = "ssh-ed25519 AAAAcli-test\n"
"#,
            workspace.display(),
            host_keys.display(),
            authorized_keys.display(),
            runtime.display(),
            keygen.display()
        ),
    )
    .unwrap();

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
    let plan = Command::new(binary())
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
    assert_eq!(plan["actions"][1]["id"], "ssh");
    assert_eq!(
        plan["actions"][1]["effect"]["service_ssh_prepare"]["authorized_keys"],
        true
    );
    assert!(!host_keys.exists());

    let doctor = Command::new(binary())
        .args(common)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let doctor: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(doctor["ok"], true);
    assert_eq!(
        doctor["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["name"] == "action.ssh")
            .unwrap()["status"],
        "pass"
    );

    let script = format!("printf 'ssh handoff\\n' > {}", handoff.display());
    let run = Command::new(binary())
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
    assert_eq!(
        fs::read_to_string(authorized_keys.join("root")).unwrap(),
        "ssh-ed25519 AAAAcli-test\n"
    );
    assert!(runtime.is_dir());
    assert_eq!(fs::read_to_string(handoff).unwrap(), "ssh handoff\n");
}

#[test]
fn undeclared_runtime_input_is_a_configuration_error() {
    let (temp, profiles, workspace, _, _) = profile_fixture();
    let lock = temp.path().join("lock");
    let output = Command::new(binary())
        .args(common_args(&profiles, &workspace, &lock))
        .args(["--input", "NOT_DECLARED=value", "plan"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(65));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not declared"));
}

#[test]
fn default_profile_file_is_used_when_no_profile_flag_is_given() {
    let (temp, profiles, workspace, _, _) = profile_fixture();
    let default_profile = temp.path().join("default-profile");
    fs::write(&default_profile, "fixture\n").unwrap();
    let output = Command::new(binary())
        .args([
            "--profiles-dir",
            profiles.to_str().unwrap(),
            "--default-profile",
            default_profile.to_str().unwrap(),
            "--workspace",
            workspace.to_str().unwrap(),
            "plan",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["profile"], "fixture");
}

#[test]
fn trust_exit_code_uses_the_structured_error_variant() {
    let trust_error = CliError::Loader(LoaderError::Model(ModelError::TrustViolation {
        action: "root-action".to_owned(),
        source: bootstrap_model::SourceKind::UserOverlay,
        message: "root actions require a trusted profile".to_owned(),
    }));
    assert_eq!(trust_error.exit_code(), 66);

    let ordinary_error = CliError::Loader(LoaderError::Model(ModelError::Invalid {
        location: "bootstrap.action".to_owned(),
        message: "contains the word TrustViolation but is not one".to_owned(),
    }));
    assert_eq!(ordinary_error.exit_code(), 65);
}

#[test]
fn cli_plans_cgroup_v2_init_action() {
    if container_init_core::PosixSystem::new().current_ids().0 != 0 {
        return;
    }
    let (temp, profiles, workspace, _, _) = profile_fixture();
    let lock = temp.path().join("lock");
    let cgroup_dir = temp.path().join("cgroup");
    fs::create_dir_all(&cgroup_dir).unwrap();
    fs::write(cgroup_dir.join("cgroup.controllers"), "cpu memory\n").unwrap();
    fs::write(cgroup_dir.join("cgroup.procs"), "").unwrap();
    fs::write(cgroup_dir.join("cgroup.subtree_control"), "").unwrap();

    let profile_content = format!(
        r#"
schema = 1
id = "fixture-cgroup"

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
id = "cg-init"
kind = "cgroup.v2_init"
path = "{}"
subgroup = "init"
controllers = ["cpu"]
run_as = "root"
"#,
        workspace.display(),
        cgroup_dir.display(),
    );
    fs::write(profiles.join("10-cgroup.toml"), profile_content).unwrap();

    let output = Command::new(binary())
        .args([
            "--profiles-dir",
            profiles.to_str().unwrap(),
            "--profile",
            "fixture-cgroup",
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
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["profile"], "fixture-cgroup");
    let cg_action = document["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "cg-init")
        .expect("cg-init action should be planned");
    assert_eq!(cg_action["kind"], "cgroup_v2_init");
    assert_eq!(cg_action["run_as"], "root");
}

#[test]
fn plans_cgroup_v2_init_bind_mount_mode_profile() {
    let temp = TempDir::new().unwrap();
    let workspace = temp.path().join("workspace");
    let profiles = temp.path().join("profiles.d");
    let lock = temp.path().join("lock");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&profiles).unwrap();

    let profile_content = format!(
        r#"
schema = 1
id = "fixture-cgroup-bind"

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
id = "cg-bind-init"
kind = "cgroup.v2_init"
mount_mode = "bind_mount"
shadow_path = "/run/cgroup_custom"
path = "/sys/fs/cgroup"
subgroup = "init"
controllers = ["cpu", "memory"]
run_as = "root"
"#,
        workspace.display(),
    );
    fs::write(profiles.join("10-cgroup-bind.toml"), profile_content).unwrap();

    let output = Command::new(binary())
        .args([
            "--profiles-dir",
            profiles.to_str().unwrap(),
            "--profile",
            "fixture-cgroup-bind",
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
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["profile"], "fixture-cgroup-bind");
    let cg_action = document["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "cg-bind-init")
        .expect("cg-bind-init action should be planned");
    assert_eq!(cg_action["kind"], "cgroup_v2_init");
    assert_eq!(
        cg_action["effect"]["cgroup_v2_init"]["mount_mode"],
        "bind_mount"
    );
    assert_eq!(
        cg_action["effect"]["cgroup_v2_init"]["shadow_path"],
        "/run/cgroup_custom"
    );
}
