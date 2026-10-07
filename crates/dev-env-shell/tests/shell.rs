use dev_env_model::{EnvValue, MaterializedEnv, ModelError, Sensitivity, ShellConfig, ShellKind};
use dev_env_shell::{
    build_invocation, build_shim, format_dotenv, format_environment, format_json, format_shell,
    validate_shim, CommandLine, ConfiguredShellAdapter, EnvironmentFormat, EnvironmentFormatError,
    RenderOptions, ShellAdapter, ShellBuildError, ShellInvocation, Shim, ShimError, ShimPathError,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::path::Path;

fn shell_config() -> ShellConfig {
    ShellConfig {
        command: "/bin/bash".to_owned(),
        kind: ShellKind::Posix,
        login_args: vec!["-l".to_owned()],
        interactive_args: vec!["-i".to_owned()],
        command_arg: Some("-c".to_owned()),
    }
}

fn environment() -> MaterializedEnv {
    MaterializedEnv::new(
        BTreeMap::from([
            (
                "PUBLIC".to_owned(),
                EnvValue {
                    value: "a'b\n$(not-a-command)".to_owned(),
                    origin: None,
                    sensitivity: Sensitivity::Public,
                    provider: None,
                },
            ),
            (
                "SECRET".to_owned(),
                EnvValue {
                    value: "token".to_owned(),
                    origin: None,
                    sensitivity: Sensitivity::Secret,
                    provider: Some("fixture".to_owned()),
                },
            ),
        ]),
        [4; 32],
    )
}

fn strings(args: &[OsString]) -> Vec<&str> {
    args.iter().map(|arg| arg.to_str().unwrap()).collect()
}

#[test]
fn adapter_uses_profile_arguments_and_keeps_extra_arguments_as_argv() {
    let adapter = ConfiguredShellAdapter::new("bash");
    let config = shell_config();
    let extra = vec![OsString::from("-x"), OsString::from("argument with spaces")];

    let interactive = adapter.build_interactive(&config, &extra).unwrap();
    assert_eq!(interactive.program(), Path::new("/bin/bash"));
    assert_eq!(
        strings(interactive.args()),
        vec!["-i", "-x", "argument with spaces"]
    );

    let login = adapter.build_login(&config, &extra).unwrap();
    assert_eq!(
        strings(login.args()),
        vec!["-l", "-x", "argument with spaces"]
    );
    let default_login = adapter.build_login(&config, &[]).unwrap();
    assert_eq!(strings(default_login.args()), vec!["-l", "-i"]);

    let script = OsStr::new("printf '%s' '$HOME; not shell syntax'");
    let command = adapter.build_command(&config, script).unwrap();
    assert_eq!(
        strings(command.args()),
        vec!["-c", "printf '%s' '$HOME; not shell syntax'"]
    );
}

#[test]
fn command_line_injects_only_the_materialized_values() {
    let line = CommandLine::new("/bin/sh", [OsString::from("-c"), OsString::from("env")]);
    let command = line.command_with_environment(&environment()).unwrap();
    let values = command
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect::<BTreeMap<_, _>>();

    assert_eq!(values["PUBLIC"].as_deref(), Some("a'b\n$(not-a-command)"));
    assert_eq!(values["SECRET"].as_deref(), Some("token"));
}

#[cfg(unix)]
#[test]
fn command_line_executes_the_script_with_injected_values() {
    let line = CommandLine::new(
        "/bin/sh",
        [
            OsString::from("-c"),
            OsString::from("printf '%s' \"$PUBLIC\""),
        ],
    );
    let output = line
        .command_with_environment(&environment())
        .unwrap()
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(output.stdout, b"a'b\n$(not-a-command)");
}

#[test]
fn shell_and_dotenv_output_quote_values_without_evaluating_them() {
    let environment = environment();
    let shell = format_shell(&environment, RenderOptions::show_secrets()).unwrap();
    assert_eq!(
        shell,
        "export PUBLIC='a'\\''b\n$(not-a-command)'\nexport SECRET='token'\n"
    );

    let dotenv = format_dotenv(&environment, RenderOptions::show_secrets()).unwrap();
    assert_eq!(
        dotenv,
        "PUBLIC='a'\\''b\n$(not-a-command)'\nSECRET='token'\n"
    );
}

#[test]
fn shell_quoting_handles_quotes_at_value_boundaries() {
    let environment = MaterializedEnv::new(
        BTreeMap::from([("QUOTED".to_owned(), EnvValue::public("'middle'"))]),
        [0; 32],
    );
    let output = format_shell(&environment, RenderOptions::show_secrets()).unwrap();
    assert_eq!(output, "export QUOTED=''\\''middle'\\'''\n");
}

#[test]
fn output_redacts_sensitive_values_by_default_and_json_is_stable() {
    let environment = environment();
    let json = format_json(&environment, RenderOptions::default()).unwrap();
    assert_eq!(
        json,
        r#"{"PUBLIC":"a'b\n$(not-a-command)","SECRET":"<redacted>"}"#
    );

    let all = format_environment(
        EnvironmentFormat::Json,
        &environment,
        RenderOptions::show_secrets(),
    )
    .unwrap();
    assert_eq!(
        all,
        r#"{"PUBLIC":"a'b\n$(not-a-command)","SECRET":"token"}"#
    );

    let adapter = ConfiguredShellAdapter::new("bash");
    assert!(adapter
        .format_env(&environment)
        .unwrap()
        .contains("<redacted>"));
}

#[test]
fn formatting_keeps_model_errors_structured() {
    let mut environment = environment();
    environment
        .values
        .get_mut("PUBLIC")
        .unwrap()
        .value
        .push('\0');

    let error = format_shell(&environment, RenderOptions::default()).unwrap_err();
    assert!(matches!(
        error,
        EnvironmentFormatError::Model {
            source: ModelError::InvalidEnvironmentValue { .. }
        }
    ));
    assert!(error.source().is_some());
}

#[test]
fn invocation_selects_the_same_adapter_for_interactive_login_and_command() {
    let adapter = ConfiguredShellAdapter::new("bash");
    let config = shell_config();

    let command =
        build_invocation(&adapter, &config, &ShellInvocation::command("echo $PATH")).unwrap();
    assert_eq!(strings(command.args()), vec!["-c", "echo $PATH"]);

    let login = build_invocation(&adapter, &config, &ShellInvocation::login([])).unwrap();
    assert_eq!(strings(login.args()), vec!["-l", "-i"]);

    let empty = build_invocation(
        &adapter,
        &config,
        &ShellInvocation::command(OsString::new()),
    )
    .unwrap();
    assert_eq!(strings(empty.args()), vec!["-c", ""]);
}

#[test]
fn missing_command_arg_is_a_typed_build_error() {
    let adapter = ConfiguredShellAdapter::new("bash");
    let mut config = shell_config();
    config.command_arg = None;

    let error = adapter
        .build_command(&config, OsStr::new("echo test"))
        .unwrap_err();
    assert!(matches!(
        error,
        ShellBuildError::MissingCommandArgument { shell } if shell == "bash"
    ));
}

#[test]
fn shim_forwards_original_arguments_and_rejects_recursion() {
    let args = vec![
        OsString::from("-l"),
        OsString::from("-c"),
        OsString::from("echo $HOME; printf '%s' \"$1\""),
        OsString::from("--value with spaces"),
    ];
    let shim = Shim::new("/usr/bin/bash.real").with_configured_command("/bin/bash");
    let command = shim.build(&args).unwrap();
    assert_eq!(command.program(), Path::new("/usr/bin/bash.real"));
    assert_eq!(command.args(), args.as_slice());

    let recursive = validate_shim(Path::new("/bin/bash"), Path::new("/bin/bash"));
    assert!(matches!(recursive, Err(ShimError::Recursive { .. })));
}

#[test]
fn shim_requires_an_absolute_real_executable_and_preserves_nul_errors() {
    let relative = build_shim("bash.real", &[]);
    assert!(matches!(
        relative,
        Err(ShimError::InvalidRealPath {
            reason: ShimPathError::Relative,
            ..
        })
    ));

    let nul = nul_os_string();
    let error = CommandLine::try_new("/bin/sh", [nul]).unwrap_err();
    assert!(matches!(
        error,
        dev_env_shell::CommandLineError::NulArgument { index: 0 }
    ));
}

fn nul_os_string() -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![b'a', 0, b'b'])
    }
    #[cfg(not(unix))]
    {
        OsString::from("a\0b")
    }
}
