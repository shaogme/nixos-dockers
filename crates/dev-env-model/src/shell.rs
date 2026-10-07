use serde::{Deserialize, Serialize};
use std::fmt;

use crate::validation::{validate_command_arg, validate_concrete_path, validate_id};
use crate::ModelError;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellKind {
    Posix,
    Argv,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ShellArgError {
    InvalidId,
    EmptyCommand,
    InvalidCommand,
    EmptyArgument,
}

impl fmt::Display for ShellArgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId => formatter.write_str("shell id is invalid"),
            Self::EmptyCommand => formatter.write_str("shell command may not be empty"),
            Self::InvalidCommand => {
                formatter.write_str("shell command must be an absolute executable path")
            }
            Self::EmptyArgument => formatter.write_str("shell arguments may not be empty"),
        }
    }
}

impl std::error::Error for ShellArgError {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShellConfig {
    pub command: String,
    pub kind: ShellKind,
    #[serde(default)]
    pub login_args: Vec<String>,
    #[serde(default)]
    pub interactive_args: Vec<String>,
    pub command_arg: Option<String>,
}

impl ShellConfig {
    pub fn validate(&self, id: &str) -> Result<(), ModelError> {
        validate_id(&format!("shells.{id}"), id).map_err(|_| ModelError::InvalidShell {
            shell: id.to_owned(),
            reason: ShellArgError::InvalidId,
        })?;
        if self.command.is_empty() {
            return Err(ModelError::InvalidShell {
                shell: id.to_owned(),
                reason: ShellArgError::EmptyCommand,
            });
        }
        validate_concrete_path(&format!("shells.{id}.command"), &self.command).map_err(|_| {
            ModelError::InvalidShell {
                shell: id.to_owned(),
                reason: ShellArgError::InvalidCommand,
            }
        })?;
        for argument in self
            .login_args
            .iter()
            .chain(self.interactive_args.iter())
            .chain(self.command_arg.iter())
        {
            if argument.is_empty() {
                return Err(ModelError::InvalidShell {
                    shell: id.to_owned(),
                    reason: ShellArgError::EmptyArgument,
                });
            }
            validate_command_arg(&format!("shells.{id}.args"), argument).map_err(|_| {
                ModelError::InvalidShell {
                    shell: id.to_owned(),
                    reason: ShellArgError::InvalidCommand,
                }
            })?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ShellEnvEntry {
    Set { name: String, value: String },
    Unset { name: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ShellEnvParseError {
    EmptyAssignment,
    InvalidName { name: String },
    InvalidStatement,
    Nul,
    Newline,
    ShellSyntax,
    UnterminatedQuote,
    InvalidEscape,
}

impl fmt::Display for ShellEnvParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyAssignment => formatter.write_str("environment assignment is empty"),
            Self::InvalidName { name } => write!(formatter, "invalid environment name {name:?}"),
            Self::InvalidStatement => formatter.write_str("unsupported shellenv statement"),
            Self::Nul => formatter.write_str("shellenv output may not contain NUL"),
            Self::Newline => formatter.write_str("shellenv line may not contain a carriage return"),
            Self::ShellSyntax => {
                formatter.write_str("shell syntax is not allowed in shellenv output")
            }
            Self::UnterminatedQuote => formatter.write_str("unterminated shellenv quote"),
            Self::InvalidEscape => formatter.write_str("invalid shellenv escape"),
        }
    }
}

impl std::error::Error for ShellEnvParseError {}

impl ShellEnvEntry {
    pub fn parse_line(line: &str) -> Result<Option<Self>, ShellEnvParseError> {
        if line.is_empty() {
            return Ok(None);
        }
        if line.contains('\0') {
            return Err(ShellEnvParseError::Nul);
        }
        if line.contains('\r') {
            return Err(ShellEnvParseError::Newline);
        }
        let statement = line.trim();
        if statement.is_empty() {
            return Ok(None);
        }
        // Devbox emits this harmless cache refresh after its assignments. It
        // changes shell state but does not contribute an environment delta.
        if statement == "hash -r" {
            return Ok(None);
        }
        if let Some(name) = statement.strip_prefix("unset ") {
            let name = name.strip_suffix(';').unwrap_or(name);
            if name.is_empty()
                || name.chars().any(char::is_whitespace)
                || !crate::validation::is_posix_env_name(name)
            {
                return Err(ShellEnvParseError::InvalidName {
                    name: name.to_owned(),
                });
            }
            return Ok(Some(Self::Unset {
                name: name.to_owned(),
            }));
        }
        let assignment = statement.strip_prefix("export ").unwrap_or(statement);
        let Some((name, raw_value)) = assignment.split_once('=') else {
            return Err(ShellEnvParseError::InvalidStatement);
        };
        if name.is_empty() {
            return Err(ShellEnvParseError::EmptyAssignment);
        }
        if !crate::validation::is_posix_env_name(name) {
            return Err(ShellEnvParseError::InvalidName {
                name: name.to_owned(),
            });
        }
        Ok(Some(Self::Set {
            name: name.to_owned(),
            value: parse_value(raw_value)?,
        }))
    }

    pub fn parse(output: &str) -> Result<Vec<Self>, ShellEnvParseError> {
        let mut entries = Vec::new();
        let mut statement = String::new();
        let mut quote = None;

        for line in output.split('\n') {
            if !statement.is_empty() {
                statement.push('\n');
            }
            statement.push_str(line);
            update_quote_state(line, &mut quote);
            if quote.is_some() {
                continue;
            }
            if let Some(entry) = Self::parse_line(&statement)? {
                entries.push(entry);
            }
            statement.clear();
        }

        if !statement.is_empty() {
            if let Some(entry) = Self::parse_line(&statement)? {
                entries.push(entry);
            }
        }
        Ok(entries)
    }
}

fn update_quote_state(text: &str, quote: &mut Option<char>) {
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        match *quote {
            Some('\'') => {
                if character == '\'' {
                    *quote = None;
                }
            }
            Some('"') => {
                if character == '\\' {
                    characters.next();
                } else if character == '"' {
                    *quote = None;
                }
            }
            None => match character {
                '\'' | '"' => *quote = Some(character),
                '\\' => {
                    characters.next();
                }
                _ => {}
            },
            Some(_) => unreachable!("only single and double quotes can be opened"),
        }
    }
}

fn parse_value(raw: &str) -> Result<String, ShellEnvParseError> {
    let mut output = String::new();
    let mut chars = raw.chars().peekable();
    let mut quote = None;
    while let Some(character) = chars.next() {
        match quote {
            Some('\'') => {
                if character == '\'' {
                    quote = None;
                } else {
                    output.push(character);
                }
            }
            Some('"') => {
                if character == '"' {
                    quote = None;
                } else if character == '\\' {
                    let escaped = chars.next().ok_or(ShellEnvParseError::InvalidEscape)?;
                    if !matches!(escaped, '\\' | '"' | '$' | '`' | 'n' | 'r' | 't') {
                        return Err(ShellEnvParseError::InvalidEscape);
                    }
                    output.push(match escaped {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                } else if character == '$' && chars.peek() == Some(&'(') {
                    return Err(ShellEnvParseError::ShellSyntax);
                } else {
                    output.push(character);
                }
            }
            Some(_) => unreachable!("only single and double quotes can be opened"),
            None => match character {
                '\'' | '"' => quote = Some(character),
                '\\' => {
                    let escaped = chars.next().ok_or(ShellEnvParseError::InvalidEscape)?;
                    if escaped == '\n' {
                        return Err(ShellEnvParseError::InvalidEscape);
                    }
                    output.push(escaped);
                }
                '$' if chars.peek() == Some(&'(') => return Err(ShellEnvParseError::ShellSyntax),
                ';' if chars.peek().is_none() => break,
                '`' | ';' | '|' | '&' | '>' | '<' => return Err(ShellEnvParseError::ShellSyntax),
                character if character.is_whitespace() => {
                    return Err(ShellEnvParseError::ShellSyntax)
                }
                other => output.push(other),
            },
        }
    }
    if quote.is_some() {
        return Err(ShellEnvParseError::UnterminatedQuote);
    }
    Ok(output)
}
