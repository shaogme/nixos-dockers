use dev_env_model::{
    FailurePolicy, MaterializedEnv, MissingProviderPolicy, PathMode, PrepareStep, ProviderConfig,
    Sensitivity, ShellEnvConfig, ShellEnvFormat, ValueTree,
};
use dev_env_provider::{
    LockManager, ProviderContext, ProviderRunner, ProviderRuntimeError, ProviderRuntimeErrorKind,
};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "dev-env-provider-docker-{}-{}",
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

fn provider_config(executable: &Path, shellenv: ShellEnvConfig) -> ProviderConfig {
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
        shellenv: Some(shellenv),
        sensitivity: Sensitivity::Public,
    }
}

fn shellenv(format: ShellEnvFormat) -> ShellEnvConfig {
    ShellEnvConfig {
        argv: vec!["env".to_owned(), "{shell}".to_owned()],
        format,
        failure: FailurePolicy::Error,
        path_mode: PathMode::Merge,
        timeout_ms: Some(1_000),
    }
}

fn context(workspace: &Path, log: &Path, mode: &str) -> ProviderContext {
    ProviderContext::new(
        workspace,
        workspace,
        "bash",
        BTreeMap::from([
            (
                "PATH".to_owned(),
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned(),
            ),
            ("PROVIDER_LOG".to_owned(), log.display().to_string()),
            ("PROVIDER_MODE".to_owned(), mode.to_owned()),
        ]),
    )
    .with_config(ValueTree::Map(BTreeMap::from([(
        "features".to_owned(),
        ValueTree::Map(BTreeMap::from([(
            "tool".to_owned(),
            ValueTree::Map(BTreeMap::from([(
                "mode".to_owned(),
                ValueTree::String("enabled".to_owned()),
            )])),
        )])),
    )])))
    .with_workspace_writable(true)
    .with_user_id(0)
    .with_config_fingerprint([9; 32])
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
    if [ "$PROVIDER_MODE" = reject ]; then
      printf 'export BAD=$(touch %s)\n' "$REJECT_MARKER"
    else
      printf '{"FROM_PROVIDER":"real-provider","PROVIDER_PATH":"/opt/provider/bin","LEAK_CHECK":"%s"}\n' "$leak"
    fi
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

#[test]
fn runs_a_real_provider_process_and_materializes_its_environment() {
    assert_eq!(std::env::consts::OS, "linux");

    let temp = TempWorkspace::new();
    let workspace = temp.path.join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("project.config"), "enabled\n").unwrap();

    let executable = temp.path.join("provider");
    let log = temp.path.join("provider.log");
    write_provider(&executable);

    let runner =
        ProviderRunner::default().with_lock_manager(LockManager::new(temp.path.join("locks")));
    let result = runner
        .run(
            "tool",
            &provider_config(&executable, shellenv(ShellEnvFormat::Json)),
            &context(&workspace, &log, "real"),
        )
        .unwrap();

    assert!(result.detection.applicable);
    assert_eq!(
        result.detection.matched_files,
        vec![workspace.join("project.config")]
    );
    assert_eq!(result.prepared_steps, 1);
    assert_eq!(result.environment.set["FROM_PROVIDER"], "real-provider");
    assert_eq!(result.environment.set["PROVIDER_PATH"], "/opt/provider/bin");
    assert_eq!(result.environment.set["LEAK_CHECK"], "absent");
    assert!(result.receipt.is_some());

    assert_eq!(
        fs::read_to_string(log).unwrap(),
        format!(
            "4\nbash\n{}\nliteral;$(not-a-command)\n",
            workspace.display()
        )
    );

    let mut environment = MaterializedEnv::new(
        BTreeMap::from([("BASE".to_owned(), dev_env_model::EnvValue::public("value"))]),
        [9; 32],
    );
    result.apply_to(&mut environment);
    assert_eq!(
        environment.values["FROM_PROVIDER"].provider.as_deref(),
        Some("tool")
    );
    assert_eq!(environment.provider_receipts.len(), 1);
}

#[test]
fn rejects_unsafe_output_from_a_real_provider_without_executing_it() {
    let temp = TempWorkspace::new();
    let workspace = temp.path.join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("project.config"), "enabled\n").unwrap();

    let executable = temp.path.join("provider");
    let log = temp.path.join("provider.log");
    let marker = temp.path.join("must-not-exist");
    write_provider(&executable);

    let mut provider_context = context(&workspace, &log, "reject");
    provider_context
        .environment
        .insert("REJECT_MARKER".to_owned(), marker.display().to_string());
    let error = ProviderRunner::default()
        .without_locks()
        .run(
            "tool",
            &provider_config(&executable, shellenv(ShellEnvFormat::Shell)),
            &provider_context,
        )
        .unwrap_err();

    assert_eq!(error.kind(), ProviderRuntimeErrorKind::OutputRejected);
    assert!(matches!(error, ProviderRuntimeError::OutputRejected { .. }));
    assert!(!marker.exists());
}
