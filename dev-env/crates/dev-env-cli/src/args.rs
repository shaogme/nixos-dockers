use dev_env_loader::CliPatch;
use dev_env_model::{OverrideOperation, OverrideSpec, ValueTree};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Cli {
    pub options: CliOptions,
    pub command: CliCommand,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CliOptions {
    pub profile: Option<String>,
    pub profiles_dir: Option<PathBuf>,
    pub admin_profiles_dir: Option<PathBuf>,
    pub default_profile_file: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub user_id: Option<u32>,
    pub patches: Vec<CliPatch>,
    pub show_secrets: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliCommand {
    Exec {
        command: Vec<OsString>,
    },
    Shell {
        shell: Option<String>,
        login: bool,
        args: Vec<OsString>,
    },
    LoginShell {
        args: Vec<OsString>,
    },
    Shim {
        shell: String,
        real: PathBuf,
        args: Vec<OsString>,
    },
    Print {
        format: OutputFormat,
        shell: Option<String>,
    },
    Explain {
        path: Option<String>,
        json: bool,
    },
    Doctor {
        json: bool,
    },
    Trust {
        target: PathBuf,
    },
    Help,
    Version,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    Dotenv,
    Json,
    Shell,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    MissingCommand,
    UnknownCommand { command: String },
    UnknownOption { option: String },
    MissingValue { option: String },
    EmptyValue { option: String },
    InvalidUtf8 { index: usize },
    InvalidAssignment { option: String, value: String },
    InvalidInteger { option: String, value: String },
    InvalidFormat { value: String },
    MissingRequiredOption { command: String, option: String },
    UnexpectedArgument { command: String, argument: String },
    MultipleArguments { command: String },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCommand => write!(formatter, "missing command\n\n{}", usage("")),
            Self::UnknownCommand { command } => {
                write!(formatter, "unknown command {command:?}\n\n{}", usage(""))
            }
            Self::UnknownOption { option } => {
                write!(formatter, "unknown option {option:?}\n\n{}", usage(""))
            }
            Self::MissingValue { option } => write!(formatter, "option {option} requires a value"),
            Self::EmptyValue { option } => {
                write!(formatter, "option {option} requires a non-empty value")
            }
            Self::InvalidUtf8 { index } => {
                write!(formatter, "argument {index} must be valid UTF-8")
            }
            Self::InvalidAssignment { option, value } => write!(
                formatter,
                "option {option} value {value:?} must have the form PATH=VALUE"
            ),
            Self::InvalidInteger { option, value } => {
                write!(
                    formatter,
                    "option {option} value {value:?} is not a valid integer"
                )
            }
            Self::InvalidFormat { value } => write!(
                formatter,
                "unknown environment format {value:?}; expected dotenv, json, or shell"
            ),
            Self::MissingRequiredOption { command, option } => {
                write!(formatter, "command {command} requires {option}")
            }
            Self::UnexpectedArgument { command, argument } => write!(
                formatter,
                "unexpected argument {argument:?} for command {command:?}"
            ),
            Self::MultipleArguments { command } => {
                write!(formatter, "command {command:?} accepts only one argument")
            }
        }
    }
}

impl std::error::Error for ParseError {}

impl Cli {
    pub fn parse<I>(arguments: I) -> Result<Self, ParseError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let arguments = arguments.into_iter().collect::<Vec<_>>();
        if arguments.is_empty() {
            return Err(ParseError::MissingCommand);
        }
        parse_os_arguments(&arguments)
    }

    /// A UTF-8 convenience API for unit tests and callers that construct argv
    /// without platform-specific values.
    pub fn parse_strings<I, S>(arguments: I) -> Result<Self, ParseError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let arguments = arguments
            .into_iter()
            .map(|argument| OsString::from(argument.into()))
            .collect::<Vec<_>>();
        if arguments.is_empty() {
            return Err(ParseError::MissingCommand);
        }
        parse_os_arguments(&arguments)
    }
}

fn parse_os_arguments(arguments: &[OsString]) -> Result<Cli, ParseError> {
    let mut options = CliOptions::default();
    match std::path::Path::new(&arguments[0])
        .file_name()
        .and_then(|name| name.to_str())
    {
        Some("dev-env-login-shell") => {
            return Ok(Cli {
                options,
                command: CliCommand::LoginShell {
                    args: arguments[1..].to_vec(),
                },
            });
        }
        Some("bash") => {
            return Ok(Cli {
                options,
                command: CliCommand::Shim {
                    shell: "bash".to_owned(),
                    real: std::env::var_os("DEVENV_REAL_SHELL")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from(DEFAULT_REAL_SHELL)),
                    args: arguments[1..].to_vec(),
                },
            });
        }
        _ => {}
    }
    let mut index = 1;

    while index < arguments.len() {
        let argument = text_argument(arguments, index)?;
        if is_help(argument) {
            return Ok(Cli {
                options,
                command: CliCommand::Help,
            });
        }
        if argument == "--version" {
            return Ok(Cli {
                options,
                command: CliCommand::Version,
            });
        }
        if argument == "--" {
            return Err(ParseError::MissingCommand);
        }
        if argument.starts_with('-') {
            parse_common_option(arguments, &mut index, &mut options)?;
            continue;
        }
        break;
    }

    if index == arguments.len() {
        return Err(ParseError::MissingCommand);
    }

    let command = text_argument(arguments, index)?.to_owned();
    index += 1;
    let command = match command.as_str() {
        "exec" => parse_exec(arguments, index, &mut options)?,
        "shell" => parse_shell(arguments, index, &mut options)?,
        "login-shell" => parse_login_shell(arguments, index, &mut options)?,
        "shim" => parse_shim(arguments, index, &mut options)?,
        "print" => parse_print(arguments, index, &mut options)?,
        "explain" => parse_explain(arguments, index, &mut options)?,
        "doctor" => parse_doctor(arguments, index, &mut options)?,
        "trust" => parse_trust(arguments, index)?,
        "version" => {
            if index != arguments.len() {
                return Err(ParseError::UnexpectedArgument {
                    command,
                    argument: text_argument(arguments, index)?.to_owned(),
                });
            }
            CliCommand::Version
        }
        _ => return Err(ParseError::UnknownCommand { command }),
    };
    Ok(Cli { options, command })
}

const DEFAULT_REAL_SHELL: &str = "/usr/local/libexec/dev-env/real/bash";

fn parse_exec(
    arguments: &[OsString],
    mut index: usize,
    options: &mut CliOptions,
) -> Result<CliCommand, ParseError> {
    while index < arguments.len() && is_common_option(text_argument(arguments, index)?) {
        parse_common_option(arguments, &mut index, options)?;
    }
    if index < arguments.len() && text_argument(arguments, index)? == "--" {
        index += 1;
    } else if index < arguments.len() && text_argument(arguments, index)?.starts_with('-') {
        return Err(ParseError::UnknownOption {
            option: text_argument(arguments, index)?.to_owned(),
        });
    }
    if index == arguments.len() {
        return Err(ParseError::MissingValue {
            option: "exec command".to_owned(),
        });
    }
    Ok(CliCommand::Exec {
        command: arguments[index..].to_vec(),
    })
}

fn parse_shell(
    arguments: &[OsString],
    mut index: usize,
    options: &mut CliOptions,
) -> Result<CliCommand, ParseError> {
    let mut shell = None;
    let mut login = false;
    while index < arguments.len() {
        let argument = text_argument(arguments, index)?;
        if is_help(argument) {
            return Ok(CliCommand::Help);
        }
        if argument == "--" {
            index += 1;
            break;
        }
        if argument == "--login" {
            login = true;
            index += 1;
        } else if argument == "--show-secrets" {
            options.show_secrets = true;
            index += 1;
        } else if argument == "--shell" || argument.starts_with("--shell=") {
            let value = option_value(arguments, &mut index, "--shell")?;
            shell = Some(value);
        } else if is_common_option(argument) {
            parse_common_option(arguments, &mut index, options)?;
        } else if argument.starts_with('-') {
            return Err(ParseError::UnknownOption {
                option: argument.to_owned(),
            });
        } else {
            break;
        }
    }
    Ok(CliCommand::Shell {
        shell,
        login,
        args: arguments[index..].to_vec(),
    })
}

fn parse_login_shell(
    arguments: &[OsString],
    mut index: usize,
    options: &mut CliOptions,
) -> Result<CliCommand, ParseError> {
    while index < arguments.len() {
        let argument = text_argument(arguments, index)?;
        if is_help(argument) {
            return Ok(CliCommand::Help);
        }
        if argument == "--" {
            index += 1;
            break;
        }
        if is_common_option(argument) {
            parse_common_option(arguments, &mut index, options)?;
        } else if argument.starts_with('-') {
            // SSH commonly passes -c and -l here.  They are shell argv, not
            // dev-env options, so preserve the complete remainder.
            break;
        } else {
            break;
        }
    }
    Ok(CliCommand::LoginShell {
        args: arguments[index..].to_vec(),
    })
}

fn parse_shim(
    arguments: &[OsString],
    mut index: usize,
    options: &mut CliOptions,
) -> Result<CliCommand, ParseError> {
    let mut shell = None;
    let mut real = None;
    while index < arguments.len() {
        let argument = text_argument(arguments, index)?;
        if is_help(argument) {
            return Ok(CliCommand::Help);
        }
        if argument == "--" {
            index += 1;
            break;
        }
        if argument == "--shell" || argument.starts_with("--shell=") {
            shell = Some(option_value(arguments, &mut index, "--shell")?);
        } else if argument == "--real" || argument.starts_with("--real=") {
            real = Some(PathBuf::from(option_value(
                arguments, &mut index, "--real",
            )?));
        } else if is_common_option(argument) {
            parse_common_option(arguments, &mut index, options)?;
        } else if argument.starts_with('-') {
            return Err(ParseError::UnknownOption {
                option: argument.to_owned(),
            });
        } else {
            break;
        }
    }
    let shell = shell.ok_or_else(|| ParseError::MissingRequiredOption {
        command: "shim".to_owned(),
        option: "--shell".to_owned(),
    })?;
    let real = real.ok_or_else(|| ParseError::MissingRequiredOption {
        command: "shim".to_owned(),
        option: "--real".to_owned(),
    })?;
    Ok(CliCommand::Shim {
        shell,
        real,
        args: arguments[index..].to_vec(),
    })
}

fn parse_print(
    arguments: &[OsString],
    mut index: usize,
    options: &mut CliOptions,
) -> Result<CliCommand, ParseError> {
    let mut format = OutputFormat::Json;
    let mut shell = None;
    while index < arguments.len() {
        let argument = text_argument(arguments, index)?;
        if is_help(argument) {
            return Ok(CliCommand::Help);
        }
        if argument == "--json" {
            format = OutputFormat::Json;
            index += 1;
        } else if argument == "--format" || argument.starts_with("--format=") {
            format = parse_format(&option_value(arguments, &mut index, "--format")?)?;
        } else if argument == "--shell" || argument.starts_with("--shell=") {
            shell = Some(option_value(arguments, &mut index, "--shell")?);
        } else if argument == "--show-secrets" {
            options.show_secrets = true;
            index += 1;
        } else if is_common_option(argument) {
            parse_common_option(arguments, &mut index, options)?;
        } else if is_help(argument) {
            return Ok(CliCommand::Help);
        } else {
            return Err(ParseError::UnexpectedArgument {
                command: "print".to_owned(),
                argument: argument.to_owned(),
            });
        }
    }
    Ok(CliCommand::Print { format, shell })
}

fn parse_explain(
    arguments: &[OsString],
    mut index: usize,
    options: &mut CliOptions,
) -> Result<CliCommand, ParseError> {
    let mut path = None;
    let mut json = false;
    while index < arguments.len() {
        let argument = text_argument(arguments, index)?;
        if is_help(argument) {
            return Ok(CliCommand::Help);
        }
        if argument == "--json" {
            json = true;
            index += 1;
        } else if is_common_option(argument) {
            parse_common_option(arguments, &mut index, options)?;
        } else if path.is_none() && !argument.starts_with('-') {
            path = Some(argument.to_owned());
            index += 1;
        } else {
            return Err(ParseError::UnexpectedArgument {
                command: "explain".to_owned(),
                argument: argument.to_owned(),
            });
        }
    }
    Ok(CliCommand::Explain { path, json })
}

fn parse_doctor(
    arguments: &[OsString],
    mut index: usize,
    options: &mut CliOptions,
) -> Result<CliCommand, ParseError> {
    let mut json = false;
    while index < arguments.len() {
        let argument = text_argument(arguments, index)?;
        if is_help(argument) {
            return Ok(CliCommand::Help);
        }
        if argument == "--json" {
            json = true;
            index += 1;
        } else if is_common_option(argument) {
            parse_common_option(arguments, &mut index, options)?;
        } else {
            return Err(ParseError::UnexpectedArgument {
                command: "doctor".to_owned(),
                argument: argument.to_owned(),
            });
        }
    }
    Ok(CliCommand::Doctor { json })
}

fn parse_trust(arguments: &[OsString], index: usize) -> Result<CliCommand, ParseError> {
    if index == arguments.len() {
        return Err(ParseError::MissingValue {
            option: "trust target".to_owned(),
        });
    }
    if index + 1 != arguments.len() {
        return Err(ParseError::MultipleArguments {
            command: "trust".to_owned(),
        });
    }
    Ok(CliCommand::Trust {
        target: PathBuf::from(text_argument(arguments, index)?),
    })
}

fn parse_common_option(
    arguments: &[OsString],
    index: &mut usize,
    options: &mut CliOptions,
) -> Result<(), ParseError> {
    let argument = text_argument(arguments, *index)?;
    let (name, inline) = argument
        .split_once('=')
        .map_or((argument, None), |(name, value)| (name, Some(value)));
    let value = |index: &mut usize| option_value_with_inline(arguments, index, name, inline);
    match name {
        "--profile" => options.profile = Some(value(index)?),
        "--profiles-dir" | "--profile-dir" => {
            options.profiles_dir = Some(PathBuf::from(value(index)?))
        }
        "--admin-profiles-dir" => options.admin_profiles_dir = Some(PathBuf::from(value(index)?)),
        "--default-profile" | "--default-profile-file" => {
            options.default_profile_file = Some(PathBuf::from(value(index)?))
        }
        "--workspace" => options.workspace = Some(PathBuf::from(value(index)?)),
        "--cwd" => options.cwd = Some(PathBuf::from(value(index)?)),
        "--config" => options.config = Some(PathBuf::from(value(index)?)),
        "--user-id" => {
            let raw = value(index)?;
            options.user_id = Some(raw.parse().map_err(|_| ParseError::InvalidInteger {
                option: name.to_owned(),
                value: raw,
            })?);
        }
        "--set" => {
            let raw = value(index)?;
            let (path, value) =
                raw.split_once('=')
                    .ok_or_else(|| ParseError::InvalidAssignment {
                        option: name.to_owned(),
                        value: raw.clone(),
                    })?;
            if path.is_empty() {
                return Err(ParseError::InvalidAssignment {
                    option: name.to_owned(),
                    value: raw,
                });
            }
            options.patches.push(CliPatch::new(
                path,
                OverrideSpec {
                    op: OverrideOperation::Set,
                    value: Some(ValueTree::String(value.to_owned())),
                    values: Vec::new(),
                    reason: "command line --set".to_owned(),
                },
            ));
        }
        "--unset" => {
            let path = value(index)?;
            options.patches.push(CliPatch::new(
                path,
                OverrideSpec {
                    op: OverrideOperation::Unset,
                    value: None,
                    values: Vec::new(),
                    reason: "command line --unset".to_owned(),
                },
            ));
        }
        _ => {
            return Err(ParseError::UnknownOption {
                option: name.to_owned(),
            })
        }
    }
    Ok(())
}

fn is_common_option(argument: &str) -> bool {
    [
        "--profile",
        "--profiles-dir",
        "--profile-dir",
        "--admin-profiles-dir",
        "--default-profile",
        "--default-profile-file",
        "--workspace",
        "--cwd",
        "--config",
        "--user-id",
        "--set",
        "--unset",
    ]
    .iter()
    .any(|name| argument == *name || argument.starts_with(&format!("{name}=")))
}

fn option_value(
    arguments: &[OsString],
    index: &mut usize,
    option: &str,
) -> Result<String, ParseError> {
    let argument = text_argument(arguments, *index)?;
    let inline = argument
        .strip_prefix(option)
        .and_then(|rest| rest.strip_prefix('='));
    option_value_with_inline(arguments, index, option, inline)
}

fn option_value_with_inline(
    arguments: &[OsString],
    index: &mut usize,
    option: &str,
    inline: Option<&str>,
) -> Result<String, ParseError> {
    let value = if let Some(value) = inline {
        *index += 1;
        value.to_owned()
    } else {
        arguments
            .get(*index + 1)
            .ok_or_else(|| ParseError::MissingValue {
                option: option.to_owned(),
            })?;
        *index += 2;
        text_argument(arguments, *index - 1)?.to_owned()
    };
    if value.is_empty() {
        return Err(ParseError::EmptyValue {
            option: option.to_owned(),
        });
    }
    Ok(value)
}

fn parse_format(value: &str) -> Result<OutputFormat, ParseError> {
    match value {
        "dotenv" => Ok(OutputFormat::Dotenv),
        "json" => Ok(OutputFormat::Json),
        "shell" => Ok(OutputFormat::Shell),
        _ => Err(ParseError::InvalidFormat {
            value: value.to_owned(),
        }),
    }
}

fn text_argument(arguments: &[OsString], index: usize) -> Result<&str, ParseError> {
    arguments[index]
        .to_str()
        .ok_or(ParseError::InvalidUtf8 { index })
}

fn is_help(argument: &str) -> bool {
    matches!(argument, "-h" | "--help")
}

pub(crate) fn usage(reason: &str) -> String {
    let prefix = if reason.is_empty() {
        String::new()
    } else {
        format!("{reason}\n\n")
    };
    format!(
        "{prefix}usage: dev-env [OPTIONS] <COMMAND> [ARGS...]\n\n\
options:\n  \
    --profile ID                 select the profile id\n  \
    --profiles-dir PATH          load image profiles from PATH\n  \
    --admin-profiles-dir PATH    load trusted admin profiles from PATH\n  \
    --default-profile-file PATH read the default profile id from PATH\n  \
    --workspace PATH             use PATH as the workspace root\n  \
    --cwd PATH                   run the session from PATH\n  \
    --config PATH                add an explicit workspace overlay\n  \
    --set PATH=VALUE             set an allowed configuration value\n  \
    --unset PATH                 unset an allowed configuration value\n  \
    --user-id UID                set the runtime user id for provider locks\n  \
    -h, --help                   show this help\n\n\
commands:\n  \
    exec [--] COMMAND [ARGS...]  execute a command with the materialized environment\n  \
    shell [OPTIONS]              enter the configured shell\n  \
    login-shell [--] [ARGS...]   SSH-compatible login shell entry point\n  \
    shim --shell ID --real PATH  execute a real shell through the materializer\n  \
    print [OPTIONS]              print the materialized environment (--json is an alias)\n  \
    explain [PATH]               show configuration and provenance\n  \
    doctor [--json]              check configuration and runtime paths\n  \
    trust PATH_OR_HASH           record or inspect a workspace config hash\n  \
    version                      print the dev-env version"
    )
}
