use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cli {
    pub options: CliOptions,
    pub command: CliCommand,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CliOptions {
    pub profile: Option<String>,
    pub profiles_dir: Option<PathBuf>,
    pub admin_profiles_dir: Option<PathBuf>,
    pub default_profile_file: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
    pub receipt_path: Option<PathBuf>,
    pub backend_socket: Option<PathBuf>,
    pub request_timeout: Option<Duration>,
    pub inputs: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliCommand {
    Run { command: Vec<String> },
    Exec { command: Vec<String> },
    Plan { json: bool },
    Doctor { json: bool },
    Status { json: bool },
    Version,
    Help,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    Invalid(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for ParseError {}

impl Cli {
    pub fn parse<I>(arguments: I) -> Result<Self, ParseError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let arguments = arguments
            .into_iter()
            .map(|argument| {
                argument
                    .into_string()
                    .map_err(|_| ParseError::Invalid("arguments must be valid UTF-8".to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::parse_strings(arguments)
    }

    pub fn parse_strings<I, S>(arguments: I) -> Result<Self, ParseError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        if arguments.is_empty() {
            return Err(ParseError::Invalid(usage("missing command")));
        }

        let mut options = CliOptions::default();
        let mut command_name = None;
        let mut run_arguments = Vec::new();
        let mut run_started = false;
        let mut json = false;
        let mut help = false;
        let mut index = 1;

        while index < arguments.len() {
            let argument = &arguments[index];

            if command_name.is_none() {
                if is_help(argument) {
                    help = true;
                    index += 1;
                    continue;
                }
                if argument == "--version" {
                    command_name = Some("version".to_owned());
                    index += 1;
                    continue;
                }
                if argument == "--json" {
                    json = true;
                    index += 1;
                    continue;
                }
                if parse_common_option(&arguments, &mut index, &mut options)? {
                    continue;
                }
                if argument == "--" {
                    return Err(ParseError::Invalid(usage(
                        "the command name must precede --",
                    )));
                }
                if argument.starts_with('-') {
                    return Err(ParseError::Invalid(format!(
                        "unknown option {argument:?}\n{}",
                        usage("")
                    )));
                }
                command_name = Some(argument.clone());
                index += 1;
                continue;
            }

            match command_name.as_deref() {
                Some("run") | Some("exec") if !run_started => {
                    if is_help(argument) {
                        help = true;
                        index += 1;
                    } else if argument == "--" {
                        run_started = true;
                        index += 1;
                    } else if parse_common_option(&arguments, &mut index, &mut options)? {
                        // The option parser advances index past its value.
                    } else if argument == "--json" {
                        return Err(ParseError::Invalid(
                            "--json is only valid with plan, doctor, or status".to_owned(),
                        ));
                    } else if argument.starts_with('-') {
                        return Err(ParseError::Invalid(format!(
                            "unknown {} option {argument:?}; use -- before command arguments",
                            command_name.as_deref().unwrap_or("run")
                        )));
                    } else {
                        run_arguments.push(argument.clone());
                        index += 1;
                        run_arguments.extend(arguments[index..].iter().cloned());
                        break;
                    }
                }
                Some("run") | Some("exec") => {
                    run_arguments.push(argument.clone());
                    index += 1;
                }
                Some("plan") | Some("doctor") | Some("status") => {
                    if is_help(argument) {
                        help = true;
                        index += 1;
                    } else if argument == "--json" {
                        json = true;
                        index += 1;
                    } else if parse_common_option(&arguments, &mut index, &mut options)? {
                        // The option parser advances index past its value.
                    } else {
                        return Err(ParseError::Invalid(format!(
                            "unexpected argument {argument:?}\n{}",
                            usage("")
                        )));
                    }
                }
                Some("version") => {
                    return Err(ParseError::Invalid(format!(
                        "unexpected argument {argument:?}\n{}",
                        usage("")
                    )));
                }
                Some(other) => {
                    return Err(ParseError::Invalid(format!(
                        "unknown command {other:?}\n{}",
                        usage("")
                    )));
                }
                None => unreachable!(),
            }
        }

        let command = if help {
            CliCommand::Help
        } else {
            match command_name.as_deref() {
                Some("run") => {
                    if json {
                        return Err(ParseError::Invalid(
                            "--json is only valid with plan, doctor, or status".to_owned(),
                        ));
                    }
                    CliCommand::Run {
                        command: run_arguments,
                    }
                }
                Some("exec") => {
                    if json {
                        return Err(ParseError::Invalid(
                            "--json is only valid with plan, doctor, or status".to_owned(),
                        ));
                    }
                    CliCommand::Exec {
                        command: run_arguments,
                    }
                }
                Some("plan") => CliCommand::Plan { json },
                Some("doctor") => CliCommand::Doctor { json },
                Some("status") => CliCommand::Status { json },
                Some("version") => {
                    if json {
                        return Err(ParseError::Invalid(
                            "--json is only valid with plan or doctor".to_owned(),
                        ));
                    }
                    CliCommand::Version
                }
                Some(other) => {
                    return Err(ParseError::Invalid(format!(
                        "unknown command {other:?}\n{}",
                        usage("")
                    )))
                }
                None => return Err(ParseError::Invalid(usage("missing command"))),
            }
        };

        validate_command_options(&options, &command)?;
        Ok(Self { options, command })
    }
}

fn parse_common_option(
    arguments: &[String],
    index: &mut usize,
    options: &mut CliOptions,
) -> Result<bool, ParseError> {
    let argument = &arguments[*index];
    let (name, inline_value) = match argument.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value.to_owned())),
        _ => (argument.as_str(), None),
    };
    let expects_value = matches!(
        name,
        "--profile"
            | "--profiles-dir"
            | "--profile-dir"
            | "--admin-profiles-dir"
            | "--default-profile"
            | "--default-profile-file"
            | "--workspace"
            | "--cwd"
            | "--receipt-path"
            | "--input"
            | "--set"
            | "--backend-socket"
            | "--request-timeout-ms"
    );
    if !expects_value {
        return Ok(false);
    }
    let value = if let Some(value) = inline_value {
        value
    } else {
        let next = arguments
            .get(*index + 1)
            .ok_or_else(|| ParseError::Invalid(format!("option {name} requires a value")))?;
        *index += 1;
        next.clone()
    };
    if value.is_empty() {
        return Err(ParseError::Invalid(format!(
            "option {name} requires a non-empty value"
        )));
    }

    match name {
        "--profile" => options.profile = Some(value),
        "--profiles-dir" | "--profile-dir" => options.profiles_dir = Some(PathBuf::from(value)),
        "--admin-profiles-dir" => options.admin_profiles_dir = Some(PathBuf::from(value)),
        "--default-profile" | "--default-profile-file" => {
            options.default_profile_file = Some(PathBuf::from(value))
        }
        "--workspace" | "--cwd" => options.workspace = Some(PathBuf::from(value)),
        "--backend-socket" => options.backend_socket = Some(PathBuf::from(value)),
        "--request-timeout-ms" => {
            let ms = value.parse::<u64>().map_err(|_| {
                ParseError::Invalid(format!(
                    "option --request-timeout-ms must be an integer: {value:?}"
                ))
            })?;
            options.request_timeout = Some(Duration::from_millis(ms));
        }
        "--receipt-path" => options.receipt_path = Some(PathBuf::from(value)),
        "--input" | "--set" => options.inputs.push(parse_input(&value)?),
        _ => unreachable!("the option table is exhaustive"),
    }
    *index += 1;
    Ok(true)
}

fn parse_input(value: &str) -> Result<(String, String), ParseError> {
    let (name, value) = value.split_once('=').ok_or_else(|| {
        ParseError::Invalid(format!("input {value:?} must have the form NAME=VALUE"))
    })?;
    if !is_environment_name(name) {
        return Err(ParseError::Invalid(format!(
            "input name {name:?} is not a valid environment name"
        )));
    }
    Ok((name.to_owned(), value.to_owned()))
}

fn validate_command_options(options: &CliOptions, command: &CliCommand) -> Result<(), ParseError> {
    let has_config_options = options.profile.is_some()
        || options.profiles_dir.is_some()
        || options.admin_profiles_dir.is_some()
        || options.default_profile_file.is_some()
        || options.workspace.is_some()
        || options.receipt_path.is_some();
    let valid = match command {
        CliCommand::Run { .. } | CliCommand::Plan { .. } | CliCommand::Doctor { .. } => true,
        CliCommand::Exec { .. } => !has_config_options,
        CliCommand::Status { .. } => !has_config_options && options.inputs.is_empty(),
        CliCommand::Version | CliCommand::Help => true,
    };
    if valid {
        Ok(())
    } else {
        Err(ParseError::Invalid(match command {
            CliCommand::Exec { .. } => {
                "exec accepts only backend options, runtime inputs, and handoff arguments"
                    .to_owned()
            }
            CliCommand::Status { .. } => "status accepts only backend options".to_owned(),
            _ => "options are not valid for this command".to_owned(),
        }))
    }
}

fn is_environment_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

fn is_help(argument: &str) -> bool {
    matches!(argument, "--help" | "-h")
}

pub(crate) fn usage(reason: &str) -> String {
    let prefix = if reason.is_empty() {
        String::new()
    } else {
        format!("{reason}\n\n")
    };
    format!(
        "{prefix}usage: container-init [OPTIONS] <run|exec|plan|doctor|status|version> [ARGS...]\n\n\
options:\n  \
    --profile ID                 select the profile id\n  \
    --profiles-dir PATH          load image profiles from PATH\n  \
    --admin-profiles-dir PATH    load trusted admin profiles from PATH\n  \
    --default-profile PATH       read the default profile id from PATH\n  \
    --workspace PATH             use PATH as the runtime working directory\n  \
    --input NAME=VALUE           set a declared bootstrap runtime input (--set is an alias)\n  \
    --backend-socket PATH        connect to the backend socket\n  \
    --request-timeout-ms N       bound backend connection time\n  \
    --receipt-path PATH          write an execution receipt to PATH\n  \
    -h, --help                   show this help\n\n\
commands:\n  \
    run [--] [COMMAND...]        start the backend and initial handoff\n  \
    exec [--] [COMMAND...]       request a handoff from the running backend\n  \
    plan [--json]                show the live plan or an offline plan\n  \
    doctor [--json]              inspect the live backend or local profile\n  \
    status [--json]              show the running backend status\n  \
    version                      print the container-init version"
    )
}
