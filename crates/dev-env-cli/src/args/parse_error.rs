use std::{
    error::Error,
    fmt::{Display, Formatter, Result as FmtResult},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    MissingCommand,
    UnknownCommand { command: String },
    UnknownOption { option: String },
    MissingValue { option: String },
    EmptyValue { option: String },
    InvalidUtf8 { index: usize },
    InvalidAssignment { option: String, value: String },
    InvalidFormat { value: String },
    InvalidStartupTimeout { value: String },
    MissingRequiredOption { command: String, option: String },
    UnexpectedArgument { command: String, argument: String },
    MultipleArguments { command: String },
}

impl Display for ParseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::MissingCommand => write!(formatter, "missing command\n\n{}", Self::usage("")),
            Self::UnknownCommand { command } => {
                write!(
                    formatter,
                    "unknown command {command:?}\n\n{}",
                    Self::usage("")
                )
            }
            Self::UnknownOption { option } => {
                write!(
                    formatter,
                    "unknown option {option:?}\n\n{}",
                    Self::usage("")
                )
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
            Self::InvalidFormat { value } => write!(
                formatter,
                "unknown environment format {value:?}; expected dotenv, json, or shell"
            ),
            Self::InvalidStartupTimeout { value } => write!(
                formatter,
                "startup timeout {value:?} must be a positive number of milliseconds"
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

impl Error for ParseError {}

impl ParseError {
    pub(super) fn usage(reason: &str) -> String {
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
    --offline                    use the file loader for diagnostics/tests\n  \
    --startup-timeout MS         wait up to MS milliseconds for backend startup\n  \
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
    backend run [OPTIONS]       start the single-instance backend\n  \
    backend status [--json]     query backend state\n  \
    backend wait-ready          wait until the backend control plane is serving\n  \
    backend reload [--wait]     reload the backend snapshot\n  \
    backend stop                 stop the backend\n  \
    version                      print the dev-env version"
        )
    }
}
