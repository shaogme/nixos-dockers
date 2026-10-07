use dev_env_model::{EnvValue, MaterializedEnv, ShellConfig, ShellKind};
use dev_env_shell::{
    build_invocation, build_shim, CommandLine, ConfiguredShellAdapter, ShellInvocation,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

fn environment() -> MaterializedEnv {
    MaterializedEnv::new(
        BTreeMap::from([
            ("HOME".to_owned(), EnvValue::public("/tmp/dev-env-shell")),
            (
                "PATH".to_owned(),
                EnvValue::public("/usr/local/bin:/usr/bin:/bin"),
            ),
            (
                "PUBLIC".to_owned(),
                EnvValue::public("a'b;$(not-a-command)\nsecond-line"),
            ),
            ("SECRET".to_owned(), EnvValue::public("secret value")),
        ]),
        [9; 32],
    )
}

fn bash_config() -> ShellConfig {
    ShellConfig {
        command: "/bin/bash".to_owned(),
        kind: ShellKind::Posix,
        login_args: vec!["--login".to_owned()],
        interactive_args: vec!["--interactive".to_owned()],
        command_arg: Some("-c".to_owned()),
    }
}

fn assert_success(output: std::process::Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn executes_a_real_posix_shell_with_only_the_materialized_environment() {
    assert_eq!(std::env::consts::OS, "linux");

    let line = CommandLine::new(
        "/bin/sh",
        [
            OsString::from("-c"),
            OsString::from(
                "printf '%s\\n' \"$PUBLIC\" \"$SECRET\" \"${DEV_ENV_HOST_LEAK-absent}\"",
            ),
        ],
    );
    let output = line
        .command_with_environment(&environment())
        .unwrap()
        .output()
        .unwrap();

    assert_eq!(
        assert_success(output),
        b"a'b;$(not-a-command)\nsecond-line\nsecret value\nabsent\n"
    );
}

#[test]
fn runs_a_profile_login_invocation_in_a_real_bash() {
    let adapter = ConfiguredShellAdapter::new("bash");
    let command = build_invocation(
        &adapter,
        &bash_config(),
        &ShellInvocation::login([
            OsString::from("-c"),
            OsString::from("printf '%s' \"$PUBLIC\""),
        ]),
    )
    .unwrap();

    assert_eq!(command.program(), Path::new("/bin/bash"));
    let output = command
        .command_with_environment(&environment())
        .unwrap()
        .output()
        .unwrap();
    assert_eq!(assert_success(output), b"a'b;$(not-a-command)\nsecond-line");
}

#[test]
fn executes_a_real_shell_shim_without_reparsing_forwarded_arguments() {
    let marker = std::env::temp_dir().join(format!(
        "dev-env-shell-shim-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let argument = format!("literal;$(touch {})", marker.display());
    let args = vec![
        OsString::from("-c"),
        OsString::from("printf '%s' \"$1\""),
        OsString::from("dev-env-shim"),
        OsString::from(argument.clone()),
    ];
    let command = build_shim("/bin/sh", &args).unwrap();
    let output = command
        .command_with_environment(&environment())
        .unwrap()
        .output()
        .unwrap();

    assert_eq!(assert_success(output), argument.as_bytes());
    assert!(!marker.exists());
}
