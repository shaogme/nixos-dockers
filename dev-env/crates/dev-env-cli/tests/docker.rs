use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    root: PathBuf,
    profiles: PathBuf,
    default_profile: PathBuf,
    trust_file: PathBuf,
    workspace: PathBuf,
    cwd: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "dev-env-cli-docker-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let profiles = root.join("profiles");
        let workspace = root.join("workspace");
        let cwd = workspace.join("nested");
        let default_profile = root.join("default-profile");
        let trust_file = root.join("trusted-hashes");
        let provider = root.join("fake-provider");
        let configured_bash = root.join("configured-bash");

        fs::create_dir_all(&profiles).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(workspace.join("project.config"), "enabled\n").unwrap();
        fs::write(&default_profile, "docker\n").unwrap();
        write_provider(&provider);
        #[cfg(unix)]
        std::os::unix::fs::symlink("/bin/bash", &configured_bash).unwrap();
        fs::write(
            profiles.join("00-docker.toml"),
            profile_contents(&workspace, &provider, &configured_bash, &root),
        )
        .unwrap();

        Self {
            root,
            profiles,
            default_profile,
            trust_file,
            workspace,
            cwd,
        }
    }

    fn command(&self) -> Command {
        let runtime = self.root.join("runtime");
        fs::create_dir_all(&runtime).unwrap();

        let mut command = Command::new(env!("CARGO_BIN_EXE_dev-env"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.join("home"))
            .env("XDG_RUNTIME_DIR", runtime)
            .env("DEV_ENV_HOST_LEAK", "ambient-from-docker")
            .env("DEVENV_PROFILES_DIR", &self.profiles)
            .env("DEVENV_DEFAULT_PROFILE_FILE", &self.default_profile)
            .env("DEVENV_TRUST_FILE", &self.trust_file)
            .current_dir(&self.cwd);
        command
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.command().args(arguments).output().unwrap()
    }

    fn run_login_shell(&self, script: &str) -> Output {
        use std::os::unix::process::CommandExt;

        let mut command = self.command();
        command.arg0("/usr/bin/dev-env-login-shell");
        command.args(["-c", script]);
        command.output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn profile_contents(
    workspace: &Path,
    provider: &Path,
    configured_bash: &Path,
    root: &Path,
) -> String {
    format!(
        r#"schema = 1
id = "docker"

[policy]
workspace_can_override = ["environment.variables.*"]
cli_can_override = ["environment.variables.*"]

[config.workspace]
root = "{}"
search = "upward"

[config.shell]
default = "bash"

[config.shells.sh]
command = "/bin/sh"
kind = "posix"
interactive_args = ["-i"]
login_args = ["-l"]
command_arg = "-c"

[config.shells.bash]
command = "{}"
kind = "posix"
interactive_args = ["-i"]
login_args = ["-l"]
command_arg = "-c"

[config.environment]
inherit_process = false
configured_value_precedence = "locked"

[config.environment.variables]
BASE_VALUE = "from-cli-docker"
PATH = "/usr/bin:/bin"
PROVIDER_LOG = "{}/provider.log"
PROVIDER_MODE = "ok"

[config.environment.path]
prepend = ["/opt/base/bin"]
append = ["/usr/local/bin"]

[config.providers.fake]
executable = "{}"
detect_files = ["project.config"]
missing = "error"

[[config.providers.fake.prepare]]
argv = ["prepare", "{{shell}}", "{{workspace}}", "literal;$(not-a-command)"]
when = "workspace.config-present"
failure = "error"
timeout_ms = 1000

[config.providers.fake.shellenv]
argv = ["env", "{{shell}}"]
format = "json"
failure = "error"
path_mode = "merge"
timeout_ms = 1000
"#,
        workspace.display(),
        configured_bash.display(),
        root.display(),
        provider.display()
    )
}

fn write_provider(path: &Path) {
    let mut file = fs::File::create(path).unwrap();
    file.write_all(
        br#"#!/bin/sh
set -eu

case "${1-}" in
  prepare)
    printf '%s\n' "$#" "$1" "$2" "$3" "$4" > "$PROVIDER_LOG"
    ;;
  env)
    if [ -n "${DEV_ENV_HOST_LEAK+x}" ]; then
      leak="$DEV_ENV_HOST_LEAK"
    else
      leak=absent
    fi
    if [ "${PROVIDER_MODE:-ok}" = reject ]; then
      printf 'export BAD=$(touch %s)\n' "$REJECT_MARKER"
      exit 0
    fi
    printf '%s\n' '{"FROM_PROVIDER":"from-real-provider","PROVIDER_SHELL":"'$2'","LEAK_CHECK":"'$leak'","SPECIAL":"literal;$(not-a-command)","PATH":"/opt/provider/bin:/usr/bin:/bin"}'
    ;;
  *)
    echo "unexpected provider operation: $1" >&2
    exit 64
    ;;
esac
"#,
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed with {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn json_environment(output: &Output) -> BTreeMap<String, String> {
    assert_success(output);
    serde_json::from_slice::<BTreeMap<String, String>>(&output.stdout).unwrap()
}

fn nul_environment(output: &Output) -> BTreeMap<String, String> {
    assert_success(output);
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let entry = String::from_utf8(entry.to_vec()).unwrap();
            let (name, value) = entry.split_once('=').unwrap();
            (name.to_owned(), value.to_owned())
        })
        .collect()
}

fn assert_materialized_projection(environment: &BTreeMap<String, String>, shell: &str) {
    assert_eq!(
        environment.get("BASE_VALUE"),
        Some(&"from-cli-docker".to_owned())
    );
    assert_eq!(
        environment.get("FROM_PROVIDER"),
        Some(&"from-real-provider".to_owned())
    );
    assert_eq!(environment.get("PROVIDER_SHELL"), Some(&shell.to_owned()));
    assert_eq!(environment.get("LEAK_CHECK"), Some(&"absent".to_owned()));
    assert_eq!(
        environment.get("SPECIAL"),
        Some(&"literal;$(not-a-command)".to_owned())
    );
    assert_eq!(
        environment.get("PATH"),
        Some(&"/opt/provider/bin:/usr/bin:/bin:/opt/base/bin:/usr/local/bin".to_owned())
    );
    assert!(!environment.contains_key("DEV_ENV_HOST_LEAK"));
}

#[test]
fn docker_cli_materializes_one_environment_across_real_entrypoints() {
    assert_eq!(std::env::consts::OS, "linux");
    let fixture = Fixture::new();

    let printed = json_environment(&fixture.run(&["print", "--format", "json"]));
    assert_materialized_projection(&printed, "bash");

    let executed = nul_environment(&fixture.run(&["exec", "--", "/usr/bin/env", "-0"]));
    assert_materialized_projection(&executed, "bash");

    let shell = fixture.run(&[
        "shell",
        "--shell",
        "sh",
        "--",
        "-c",
        "printf '%s\\n' \"$BASE_VALUE\" \"$FROM_PROVIDER\" \"$PROVIDER_SHELL\" \"$LEAK_CHECK\" \"$SPECIAL\"",
    ]);
    assert_success(&shell);
    assert_eq!(
        String::from_utf8(shell.stdout).unwrap(),
        "from-cli-docker\nfrom-real-provider\nsh\nabsent\nliteral;$(not-a-command)\n"
    );

    let login = fixture.run_login_shell(
        "printf '%s\\n' \"$BASE_VALUE\" \"$FROM_PROVIDER\" \"$PROVIDER_SHELL\" \"$LEAK_CHECK\"",
    );
    assert_success(&login);
    assert_eq!(
        String::from_utf8(login.stdout).unwrap(),
        "from-cli-docker\nfrom-real-provider\nbash\nabsent\n"
    );

    let shim = fixture.run(&[
        "shim",
        "--shell",
        "bash",
        "--real",
        "/bin/bash",
        "--",
        "-c",
        "printf '%s' \"$SPECIAL\"",
    ]);
    assert_success(&shim);
    assert_eq!(
        String::from_utf8(shim.stdout).unwrap(),
        "literal;$(not-a-command)"
    );

    let shell_output = fixture.run(&["print", "--format", "shell"]);
    assert_success(&shell_output);
    let rendered = String::from_utf8(shell_output.stdout).unwrap();
    assert!(rendered.contains("export SPECIAL='literal;$(not-a-command)'\n"));
    assert!(rendered
        .contains("export PATH='/opt/provider/bin:/usr/bin:/bin:/opt/base/bin:/usr/local/bin'\n"));

    let argument_boundary = fixture.run(&[
        "exec",
        "--",
        "/bin/sh",
        "-c",
        "printf '%s' \"$1\"",
        "dev-env",
        "literal;$(touch should-not-exist)",
    ]);
    assert_success(&argument_boundary);
    assert_eq!(
        String::from_utf8(argument_boundary.stdout).unwrap(),
        "literal;$(touch should-not-exist)"
    );

    assert_eq!(
        fs::read_to_string(fixture.root.join("provider.log")).unwrap(),
        format!(
            "4\nprepare\nbash\n{}\nliteral;$(not-a-command)\n",
            fixture.workspace.display()
        )
    );
}

#[test]
fn docker_cli_rejects_provider_output_without_executing_shell_code() {
    let fixture = Fixture::new();
    let marker = fixture.root.join("must-not-exist");
    let marker_assignment = format!("environment.variables.REJECT_MARKER={}", marker.display());

    let output = fixture
        .command()
        .args(["--set", "environment.variables.PROVIDER_MODE=reject"])
        .arg("--set")
        .arg(marker_assignment)
        .args(["print", "--format", "json"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(70));
    assert!(String::from_utf8_lossy(&output.stderr).contains("DEVENV-E-PROVIDER"));
    assert!(!marker.exists());
}

#[test]
fn docker_cli_explain_doctor_and_trust_are_real_process_commands() {
    let fixture = Fixture::new();

    let explained = fixture.run(&["explain", "--json", "environment.variables.BASE_VALUE"]);
    let explained: Value = {
        assert_success(&explained);
        serde_json::from_slice(&explained.stdout).unwrap()
    };
    assert_eq!(explained["profile"], "docker");
    assert_eq!(explained["value"], "from-cli-docker");
    assert_eq!(explained["merge_policy"], "strict");

    let doctor = fixture.run(&["doctor", "--json"]);
    let doctor: Value = {
        assert_success(&doctor);
        serde_json::from_slice(&doctor.stdout).unwrap()
    };
    assert_eq!(doctor["ok"], true);
    assert!(doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check["name"] == "shell.bash"));
    assert!(doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check["name"] == "provider.fake" && check["status"] == "warn"));
    assert!(!fixture.root.join("provider.log").exists());

    let trusted = fixture.run(&[
        "trust",
        fixture.profiles.join("00-docker.toml").to_str().unwrap(),
    ]);
    assert_success(&trusted);
    let hash = String::from_utf8(trusted.stdout)
        .unwrap()
        .strip_prefix("trusted: ")
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(hash.len(), 64);
    assert_eq!(
        fs::read_to_string(&fixture.trust_file).unwrap().trim(),
        hash
    );
    assert!(!fs::read_to_string(&fixture.trust_file)
        .unwrap()
        .contains("schema = 1"));
}
