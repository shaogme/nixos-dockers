use dev_env_core::{CoreError, Materializer, RuntimeContext};
use dev_env_model::{
    ConfiguredValuePrecedence, EnvironmentConfig, EnvironmentPath, FailurePolicy,
    MissingProviderPolicy, PathMode, PolicyConfig, PrepareStep, ProviderConfig, ResolvedConfig,
    Sensitivity, ShellConfig, ShellEnvConfig, ShellEnvFormat, ShellKind, ShellSelection, ValueTree,
    WorkspaceConfig, WorkspaceSearch,
};
use dev_env_provider::{
    LockManager, ProviderRunner, ProviderRuntimeError, ProviderRuntimeErrorKind,
};
use dev_env_shell::CommandLine;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "dev-env-core-docker-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write_provider(path: &Path) {
    fs::write(
        path,
        r#"#!/bin/sh
set -eu
case "$1" in
  prepare)
    printf '%s\n' "$#" "$2" "$3" "$4" > "$PROVIDER_LOG"
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
    if [ "${PROVIDER_MODE:-ok}" = fail ]; then
      printf '%s\n' 'real provider shellenv failed' >&2
      exit 23
    fi
    printf '{"FROM_PROVIDER":"real-provider","LEAK_CHECK":"%s","PATH":"/opt/provider/bin:/usr/bin:/bin"}\n' "$leak"
    ;;
  *)
    echo "unexpected provider operation: $1" >&2
    exit 64
    ;;
esac
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn provider_config(executable: &Path) -> ProviderConfig {
    ProviderConfig {
        executable: executable.display().to_string(),
        detect_files: vec!["project.config".to_owned()],
        missing: MissingProviderPolicy::Error,
        depends_on: Vec::new(),
        prepare: vec![PrepareStep {
            argv: vec![
                "prepare".to_owned(),
                "{shell}".to_owned(),
                "{workspace}".to_owned(),
                "literal;$(not-a-command)".to_owned(),
            ],
            when: Some("workspace.config-present && features.tool.mode == 'enabled'".to_owned()),
            failure: FailurePolicy::Error,
            timeout_ms: Some(1_000),
            sensitivity: Sensitivity::Public,
        }],
        shellenv: Some(ShellEnvConfig {
            argv: vec!["env".to_owned(), "{shell}".to_owned()],
            format: ShellEnvFormat::Json,
            failure: FailurePolicy::Error,
            path_mode: PathMode::Merge,
            timeout_ms: Some(1_000),
        }),
        sensitivity: Sensitivity::Public,
    }
}

fn config(workspace: &Path, executable: &Path, provider_mode: &str) -> ResolvedConfig {
    let mut shells = BTreeMap::new();
    shells.insert(
        "sh".to_owned(),
        ShellConfig {
            command: "/bin/sh".to_owned(),
            kind: ShellKind::Posix,
            login_args: vec!["-l".to_owned()],
            interactive_args: vec!["-i".to_owned()],
            command_arg: Some("-c".to_owned()),
        },
    );
    let mut providers = BTreeMap::new();
    providers.insert("tool".to_owned(), provider_config(executable));
    ResolvedConfig {
        workspace: WorkspaceConfig {
            root: workspace.display().to_string(),
            search: WorkspaceSearch::Upward,
        },
        shell: ShellSelection {
            default: "sh".to_owned(),
        },
        shells,
        environment: EnvironmentConfig {
            // The Docker image deliberately contains DEV_ENV_HOST_LEAK.  The
            // session below supplies it as ambient input, but locked config
            // inheritance is disabled so it cannot reach the provider or shell.
            inherit_process: false,
            configured_value_precedence: ConfiguredValuePrecedence::Locked,
            variables: BTreeMap::from([
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                ("BASE_VALUE".to_owned(), "from-core".to_owned()),
                (
                    "PROVIDER_LOG".to_owned(),
                    workspace.join("provider.log").display().to_string(),
                ),
                ("PROVIDER_MODE".to_owned(), provider_mode.to_owned()),
            ]),
            conditional_variables: BTreeMap::new(),
            path: EnvironmentPath {
                prepend: vec!["/opt/base/bin".to_owned()],
                append: vec!["/usr/local/bin".to_owned()],
                remove: vec![],
            },
        },
        features: ValueTree::Map(BTreeMap::from([(
            "tool".to_owned(),
            ValueTree::Map(BTreeMap::from([(
                "mode".to_owned(),
                ValueTree::String("enabled".to_owned()),
            )])),
        )])),
        providers,
        inputs: BTreeMap::new(),
        policy: PolicyConfig::default(),
        provenance: Default::default(),
    }
}

fn context(workspace: &Path) -> RuntimeContext {
    RuntimeContext::new(
        workspace,
        workspace,
        "sh",
        BTreeMap::from([
            (
                "PATH".to_owned(),
                "/host/should-not-be-visible:/usr/bin:/bin".to_owned(),
            ),
            (
                "DEV_ENV_HOST_LEAK".to_owned(),
                std::env::var("DEV_ENV_HOST_LEAK").unwrap_or_else(|_| "missing".to_owned()),
            ),
        ]),
    )
    .with_user_id(0)
    .with_workspace_writable(true)
}

fn runner(temp: &TempWorkspace) -> ProviderRunner {
    ProviderRunner::default()
        .with_lock_manager(LockManager::new(temp.path.join("locks")))
        .with_lock_timeout(Duration::from_secs(2))
}

fn run_shell(environment: &dev_env_model::MaterializedEnv, workspace: &Path) -> String {
    let command = CommandLine::new(
        "/bin/sh",
        [
            "-c",
            "printf '%s\\n' \"$BASE_VALUE\" \"$FROM_PROVIDER\" \"$LEAK_CHECK\" \"${DEV_ENV_HOST_LEAK-absent}\"",
        ],
    );
    let output = command
        .command_with_environment(environment)
        .unwrap()
        .current_dir(workspace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "shell failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn docker_materializes_a_real_provider_and_real_shell_without_host_leaks() {
    assert_eq!(std::env::consts::OS, "linux");

    let temp = TempWorkspace::new();
    let workspace = temp.path.join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("project.config"), "enabled\n").unwrap();
    let executable = temp.path.join("provider");
    write_provider(&executable);

    let materialization =
        Materializer::with_provider_runner(config(&workspace, &executable, "ok"), runner(&temp))
            .materialize(&context(&workspace))
            .unwrap();

    assert_eq!(
        materialization.environment.values["BASE_VALUE"].value,
        "from-core"
    );
    assert_eq!(
        materialization.environment.values["FROM_PROVIDER"].value,
        "real-provider"
    );
    assert_eq!(
        materialization.environment.values["LEAK_CHECK"].value,
        "absent"
    );
    assert_eq!(
        materialization.environment.values["PATH"].value,
        "/opt/provider/bin:/usr/bin:/bin:/opt/base/bin:/usr/local/bin"
    );
    assert_eq!(materialization.environment.provider_receipts.len(), 1);
    assert_ne!(materialization.environment.config_fingerprint, [0; 32]);
    assert_eq!(materialization.diagnostics.len(), 0);

    assert_eq!(
        fs::read_to_string(workspace.join("provider.log")).unwrap(),
        format!("4\nsh\n{}\nliteral;$(not-a-command)\n", workspace.display())
    );
    assert_eq!(
        run_shell(&materialization.environment, &workspace),
        "from-core\nreal-provider\nabsent\nabsent\n"
    );
}

#[test]
fn docker_preserves_a_real_provider_failure_as_a_structured_error() {
    let temp = TempWorkspace::new();
    let workspace = temp.path.join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("project.config"), "enabled\n").unwrap();
    let executable = temp.path.join("provider");
    write_provider(&executable);

    let error =
        Materializer::with_provider_runner(config(&workspace, &executable, "fail"), runner(&temp))
            .materialize(&context(&workspace))
            .unwrap_err();

    assert_eq!(error.code(), "DEVENV-E-PROVIDER");
    assert!(matches!(
        &error,
        CoreError::Provider { provider, source }
            if provider == "tool"
                && source.kind() == ProviderRuntimeErrorKind::CommandFailed
    ));
    assert!(matches!(
        std::error::Error::source(&error),
        Some(source) if source.downcast_ref::<ProviderRuntimeError>().is_some()
    ));
    if let CoreError::Provider { source, .. } = error {
        assert!(matches!(
            source.as_ref(),
            ProviderRuntimeError::CommandFailed { output, .. }
                if output.status == Some(23) && output.stderr == b"real provider shellenv failed\n"
        ));
    }
}

#[test]
fn docker_rejects_shellenv_code_before_a_real_shell_can_execute_it() {
    let temp = TempWorkspace::new();
    let workspace = temp.path.join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("project.config"), "enabled\n").unwrap();
    let marker = temp.path.join("must-not-exist");
    let executable = temp.path.join("provider");
    write_provider(&executable);

    let mut reject_config = config(&workspace, &executable, "reject");
    reject_config
        .providers
        .get_mut("tool")
        .unwrap()
        .shellenv
        .as_mut()
        .unwrap()
        .format = ShellEnvFormat::Shell;
    reject_config
        .environment
        .variables
        .insert("REJECT_MARKER".to_owned(), marker.display().to_string());
    let error = Materializer::with_provider_runner(reject_config, runner(&temp))
        .materialize(&context(&workspace))
        .unwrap_err();

    assert!(matches!(
        error,
        CoreError::Provider {
            source,
            ..
        } if matches!(source.as_ref(), ProviderRuntimeError::OutputRejected { .. })
    ));
    assert!(!marker.exists());
}
