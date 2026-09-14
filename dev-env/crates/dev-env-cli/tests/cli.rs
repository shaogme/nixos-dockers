use dev_env_cli::{Cli, CliCommand, CliError, ParseError};
use dev_env_loader::LoaderError;
use dev_env_model::ModelError;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct Fixture {
    root: PathBuf,
    profiles: PathBuf,
    default_profile: PathBuf,
    trust_file: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("dev-env-cli-test-{}-{suffix}", std::process::id()));
        let profiles = root.join("profiles");
        fs::create_dir_all(&profiles).unwrap();
        let default_profile = root.join("default-profile");
        fs::write(&default_profile, "base\n").unwrap();
        let trust_file = root.join("trusted-hashes");
        let profile = format!(
            r#"schema = 1
id = "base"

[policy]
workspace_can_override = ["environment.variables.*"]
cli_can_override = ["environment.variables.*"]

[config.workspace]
root = "{}"
search = "fixed"

[config.shell]
default = "sh"

[config.shells.sh]
command = "/bin/sh"
kind = "posix"
command_arg = "-c"

[config.environment]
inherit_process = false

[config.environment.variables]
BASE = "base"
"#,
            root.display()
        );
        fs::write(profiles.join("00-base.toml"), profile).unwrap();
        Self {
            root,
            profiles,
            default_profile,
            trust_file,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dev-env"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.join("home"))
            .env("DEVENV_PROFILES_DIR", &self.profiles)
            .env("DEVENV_DEFAULT_PROFILE_FILE", &self.default_profile)
            .env("DEVENV_TRUST_FILE", &self.trust_file)
            .current_dir(&self.root);
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run(fixture: &Fixture, arguments: &[&str]) -> Output {
    fixture.command().args(arguments).output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn parser_keeps_command_argv_and_supports_login_launcher() {
    let cli = Cli::parse_strings([
        "dev-env",
        "--profile",
        "base",
        "exec",
        "--",
        "printf",
        "%s",
        "argument with spaces",
    ])
    .unwrap();
    assert_eq!(cli.options.profile.as_deref(), Some("base"));
    assert!(matches!(
        cli.command,
        CliCommand::Exec { ref command }
            if command == &[
                OsString::from("printf"),
                OsString::from("%s"),
                OsString::from("argument with spaces")
            ]
    ));

    let login = Cli::parse_strings(["/usr/bin/dev-env-login-shell", "-c", "printf ok"]).unwrap();
    assert!(matches!(login.command, CliCommand::LoginShell { ref args } if args.len() == 2));

    let bash = Cli::parse_strings(["/bin/bash", "-c", "printf ok"]).unwrap();
    assert!(matches!(
        bash.command,
        CliCommand::Shim { ref shell, ref real, ref args }
            if shell == "bash"
                && real == std::path::Path::new("/usr/local/libexec/dev-env/real/bash")
                && args.len() == 2
    ));
}

#[test]
fn parser_rejects_malformed_cli_values_with_typed_errors() {
    assert!(matches!(
        Cli::parse_strings(["dev-env", "print", "--format", "xml"]),
        Err(ParseError::InvalidFormat { value }) if value == "xml"
    ));
    assert!(matches!(
        Cli::parse_strings(["dev-env", "--set", "not-an-assignment", "print"]),
        Err(ParseError::InvalidAssignment { option, .. }) if option == "--set"
    ));
    assert!(matches!(
        Cli::parse_strings(["dev-env", "shim", "--shell", "sh"]),
        Err(ParseError::MissingRequiredOption { option, .. }) if option == "--real"
    ));
}

#[test]
fn print_exec_shell_login_and_shim_share_the_same_environment() {
    let fixture = Fixture::new();

    let printed = run(&fixture, &["print", "--json"]);
    assert!(
        printed.status.success(),
        "{}",
        String::from_utf8_lossy(&printed.stderr)
    );
    assert_eq!(stdout(&printed), "{\"BASE\":\"base\"}\n");

    let executed = run(&fixture, &["exec", "--", "/usr/bin/env"]);
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );
    assert_eq!(stdout(&executed), "BASE=base\n");

    let shell = run(
        &fixture,
        &[
            "shell",
            "--shell",
            "sh",
            "--",
            "-c",
            "printf '%s' \"$BASE\"",
        ],
    );
    assert!(
        shell.status.success(),
        "{}",
        String::from_utf8_lossy(&shell.stderr)
    );
    assert_eq!(stdout(&shell), "base");

    let login = run(
        &fixture,
        &["login-shell", "--", "-c", "printf '%s' \"$BASE\""],
    );
    assert!(
        login.status.success(),
        "{}",
        String::from_utf8_lossy(&login.stderr)
    );
    assert_eq!(stdout(&login), "base");

    let shim = run(
        &fixture,
        &[
            "shim",
            "--shell",
            "sh",
            "--real",
            "/bin/bash",
            "--",
            "-c",
            "printf '%s' \"$BASE\"",
        ],
    );
    assert!(
        shim.status.success(),
        "{}",
        String::from_utf8_lossy(&shim.stderr)
    );
    assert_eq!(stdout(&shim), "base");
}

#[cfg(unix)]
#[test]
fn cli_runs_a_declared_provider_and_includes_its_shellenv_delta() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let provider = fixture.root.join("fake-provider");
    fs::write(
        &provider,
        "#!/bin/sh\nprintf '%s' '{\"PROVIDED\":\"from-provider\"}'\n",
    )
    .unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();
    let profile_path = fixture.profiles.join("00-base.toml");
    let mut profile = fs::read_to_string(&profile_path).unwrap();
    profile.push_str(&format!(
        r#"
[config.providers.fake]
executable = "{}"
shellenv = {{ argv = ["env"], format = "json", failure = "error" }}
"#,
        provider.display()
    ));
    fs::write(profile_path, profile).unwrap();

    let output = run(&fixture, &["print", "--format", "json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout(&output),
        "{\"BASE\":\"base\",\"PROVIDED\":\"from-provider\"}\n"
    );
}

#[test]
fn declared_runtime_inputs_are_typed_before_the_environment_is_materialized() {
    let fixture = Fixture::new();
    let profile_path = fixture.profiles.join("00-base.toml");
    let mut profile = fs::read_to_string(&profile_path).unwrap();
    profile.push_str(
        r#"
[config.features.devbox]
auto_init = "never"

[inputs.DEVBOX_AUTO_INIT]
target = "features.devbox.auto_init"
type = "enum"
values = ["never", "if-missing", "ask"]
aliases = { "1" = "if-missing" }
runtime = true
"#,
    );
    fs::write(profile_path, profile).unwrap();
    let mut command = fixture.command();
    command.env("DEVBOX_AUTO_INIT", "1");
    command.args(["explain", "--json", "features.devbox.auto_init"]);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["value"], "if-missing");
    assert_eq!(
        value["provenance"]["features.devbox.auto_init"]["origins"][1]["source"]["kind"],
        "environment"
    );
}

#[test]
fn cli_and_workspace_overlays_are_applied_after_the_profile() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join(".dev-env.toml"),
        r#"schema = 1
id = "workspace"

[override."environment.variables.BASE"]
op = "set"
value = "workspace"
reason = "workspace policy"
"#,
    )
    .unwrap();
    let workspace = run(&fixture, &["print", "--format", "json"]);
    assert!(
        workspace.status.success(),
        "{}",
        String::from_utf8_lossy(&workspace.stderr)
    );
    assert_eq!(stdout(&workspace), "{\"BASE\":\"workspace\"}\n");

    let cli = run(
        &fixture,
        &[
            "print",
            "--format",
            "json",
            "--set",
            "environment.variables.BASE=cli",
        ],
    );
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert_eq!(stdout(&cli), "{\"BASE\":\"cli\"}\n");
}

#[test]
fn bare_workspace_overlay_headers_are_optional_and_ambiguous_files_need_config() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join(".dev-env.local.toml"),
        r#"[override."environment.variables.BASE"]
op = "set"
value = "bare-overlay"
reason = "local workspace choice"
"#,
    )
    .unwrap();
    let local = run(&fixture, &["print", "--format", "json"]);
    assert!(
        local.status.success(),
        "{}",
        String::from_utf8_lossy(&local.stderr)
    );
    assert_eq!(stdout(&local), "{\"BASE\":\"bare-overlay\"}\n");
    fs::remove_file(fixture.root.join(".dev-env.local.toml")).unwrap();

    fs::create_dir_all(fixture.root.join(".dev-env")).unwrap();
    fs::write(
        fixture.root.join(".dev-env.toml"),
        r#"[override."environment.variables.BASE"]
op = "set"
value = "conventional"
reason = "conventional workspace choice"
"#,
    )
    .unwrap();
    fs::write(
        fixture.root.join(".dev-env/config.toml"),
        r#"[override."environment.variables.BASE"]
op = "set"
value = "directory"
reason = "directory workspace choice"
"#,
    )
    .unwrap();
    let ambiguous = run(&fixture, &["print", "--format", "json"]);
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("use --config"));

    let selected = run(
        &fixture,
        &[
            "print",
            "--format",
            "json",
            "--config",
            ".dev-env/config.toml",
        ],
    );
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    assert_eq!(stdout(&selected), "{\"BASE\":\"directory\"}\n");
}

#[test]
fn explain_and_doctor_are_auditable_without_running_a_provider() {
    let fixture = Fixture::new();
    let explained = run(
        &fixture,
        &["explain", "--json", "environment.variables.BASE"],
    );
    assert!(
        explained.status.success(),
        "{}",
        String::from_utf8_lossy(&explained.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&explained.stdout).unwrap();
    assert_eq!(value["value"], "base");
    assert_eq!(value["merge_policy"], "strict");
    assert_eq!(
        value["provenance"]["environment.variables.BASE"]["sensitivity"],
        "public"
    );

    let doctor = run(&fixture, &["doctor", "--json"]);
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(report["ok"], true);
    assert!(report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|check| check["name"] == "shell.sh"));
}

#[test]
fn trust_records_a_hash_without_storing_the_file_contents() {
    let fixture = Fixture::new();
    let profile = fixture.profiles.join("00-base.toml");
    let trusted = run(&fixture, &["trust", profile.to_str().unwrap()]);
    assert!(
        trusted.status.success(),
        "{}",
        String::from_utf8_lossy(&trusted.stderr)
    );
    let trusted_text = stdout(&trusted);
    let hash = trusted_text.strip_prefix("trusted: ").unwrap().trim();
    assert_eq!(hash.len(), 64);
    let store = fs::read_to_string(&fixture.trust_file).unwrap();
    assert_eq!(store.trim(), hash);
    assert!(!store.contains("schema = 1"));
}

#[test]
fn child_exit_status_is_not_wrapped_by_the_cli() {
    let fixture = Fixture::new();
    let output = run(&fixture, &["exec", "--", "/bin/sh", "-c", "exit 23"]);
    assert_eq!(output.status.code(), Some(23));
}

#[test]
fn cli_error_keeps_a_structured_loader_error_as_its_source() {
    let error = CliError::Loader(LoaderError::Model {
        location: Some("fixture.toml".to_owned()),
        source: ModelError::MissingDefaultShell {
            shell: "missing".to_owned(),
        },
    });
    let source = std::error::Error::source(&error).unwrap();
    assert!(source.downcast_ref::<LoaderError>().is_some());
    assert!(source
        .source()
        .unwrap()
        .downcast_ref::<ModelError>()
        .is_some());
}

#[allow(dead_code)]
fn _path(_: &Path) {}
