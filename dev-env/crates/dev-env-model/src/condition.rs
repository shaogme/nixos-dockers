use serde::Serialize;
use std::fmt;

use crate::validation::{is_config_path, is_env_name};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ConditionError {
    Empty,
    EmptyOperand { operator: String },
    UnterminatedString,
    InvalidReference { reference: String },
    InvalidValue { value: String },
    UnsupportedExpression { expression: String },
    UnbalancedParentheses,
    ShellSyntax,
}

impl fmt::Display for ConditionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("condition may not be empty"),
            Self::EmptyOperand { operator } => {
                write!(formatter, "operator {operator:?} has an empty operand")
            }
            Self::UnterminatedString => formatter.write_str("unterminated condition string"),
            Self::InvalidReference { reference } => {
                write!(formatter, "invalid condition reference {reference:?}")
            }
            Self::InvalidValue { value } => write!(formatter, "invalid condition value {value:?}"),
            Self::UnsupportedExpression { expression } => {
                write!(formatter, "unsupported condition {expression:?}")
            }
            Self::UnbalancedParentheses => formatter.write_str("unbalanced condition parentheses"),
            Self::ShellSyntax => formatter.write_str("shell expressions are not allowed"),
        }
    }
}

impl std::error::Error for ConditionError {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum Condition {
    Always,
    Boolean(bool),
    Reference(String),
    Equal(ConditionValue, ConditionValue),
    NotEqual(ConditionValue, ConditionValue),
    And(Vec<Condition>),
    Or(Vec<Condition>),
    Not(Box<Condition>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ConditionValue {
    Reference(String),
    Literal(String),
    Boolean(bool),
}

impl Condition {
    pub fn parse(input: &str) -> Result<Self, ConditionError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ConditionError::Empty);
        }
        if input.contains("$(")
            || input.contains('`')
            || input.contains(';')
            || input.contains('|') && !input.contains("||")
            || input.contains('>')
            || input.contains('<')
        {
            return Err(ConditionError::ShellSyntax);
        }
        let input = strip_outer_parentheses(input)?;
        if input == "always" || input == "true" {
            return Ok(if input == "always" {
                Self::Always
            } else {
                Self::Boolean(true)
            });
        }
        if input == "false" {
            return Ok(Self::Boolean(false));
        }
        let or_parts = split_top_level(input, "||")?;
        if or_parts.len() > 1 {
            return Ok(Self::Or(
                or_parts
                    .into_iter()
                    .map(Self::parse)
                    .collect::<Result<Vec<_>, _>>()?,
            ));
        }
        let and_parts = split_top_level(input, "&&")?;
        if and_parts.len() > 1 {
            return Ok(Self::And(
                and_parts
                    .into_iter()
                    .map(Self::parse)
                    .collect::<Result<Vec<_>, _>>()?,
            ));
        }
        if let Some(rest) = input.strip_prefix('!') {
            return Ok(Self::Not(Box::new(Self::parse(rest)?)));
        }
        if let Some((left, operator, right)) = split_comparison(input)? {
            let left = ConditionValue::parse(left)?;
            let right = ConditionValue::parse(right)?;
            return Ok(match operator {
                Comparison::Equal => Self::Equal(left, right),
                Comparison::NotEqual => Self::NotEqual(left, right),
            });
        }
        validate_reference(input)?;
        Ok(Self::Reference(input.to_owned()))
    }
}

impl ConditionValue {
    fn parse(input: &str) -> Result<Self, ConditionError> {
        let input = input.trim();
        if input == "true" {
            return Ok(Self::Boolean(true));
        }
        if input == "false" {
            return Ok(Self::Boolean(false));
        }
        if (input.starts_with('"') && input.ends_with('"'))
            || (input.starts_with('\'') && input.ends_with('\''))
        {
            if input.len() < 2 {
                return Err(ConditionError::UnterminatedString);
            }
            return Ok(Self::Literal(input[1..input.len() - 1].to_owned()));
        }
        if input.chars().all(|character| character.is_ascii_digit()) && !input.is_empty() {
            return Ok(Self::Literal(input.to_owned()));
        }
        validate_reference(input)?;
        Ok(Self::Reference(input.to_owned()))
    }
}

#[derive(Clone, Copy)]
enum Comparison {
    Equal,
    NotEqual,
}

fn validate_reference(input: &str) -> Result<(), ConditionError> {
    let valid = match input.split_once('.') {
        Some((namespace, rest)) => match namespace {
            "provider" => rest == "enabled" || is_config_path(rest),
            "workspace" => matches!(rest, "config-present" | "writable" | "root"),
            "features" | "config" => is_config_path(rest),
            "input" | "env" => is_env_name(rest),
            "context" => matches!(rest, "cwd" | "os" | "arch"),
            _ => false,
        },
        None => false,
    };
    if valid {
        Ok(())
    } else {
        Err(ConditionError::InvalidReference {
            reference: input.to_owned(),
        })
    }
}

fn strip_outer_parentheses(input: &str) -> Result<&str, ConditionError> {
    if !input.starts_with('(') {
        return Ok(input);
    }
    let mut depth: usize = 0;
    let mut quote = None;
    for (index, character) in input.char_indices() {
        if let Some(expected) = quote {
            if character == expected {
                quote = None;
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
        } else if character == '(' {
            depth += 1;
        } else if character == ')' {
            depth = depth
                .checked_sub(1)
                .ok_or(ConditionError::UnbalancedParentheses)?;
            if depth == 0 {
                if input[index + character.len_utf8()..].trim().is_empty() {
                    return strip_outer_parentheses(&input[1..index]);
                }
                return Ok(input);
            }
        }
    }
    Err(ConditionError::UnbalancedParentheses)
}

fn split_top_level<'a>(input: &'a str, operator: &str) -> Result<Vec<&'a str>, ConditionError> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut cursor = 0;
    while cursor < input.len() {
        if input[cursor..].starts_with(operator) && top_level_at(input, cursor)? {
            parts.push(input[start..cursor].trim());
            cursor += operator.len();
            start = cursor;
        } else {
            cursor += input[cursor..]
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(1);
        }
    }
    parts.push(input[start..].trim());
    if parts.iter().any(|part| part.is_empty()) {
        return Err(ConditionError::EmptyOperand {
            operator: operator.to_owned(),
        });
    }
    Ok(parts)
}

fn split_comparison(input: &str) -> Result<Option<(&str, Comparison, &str)>, ConditionError> {
    for (operator, comparison) in [("!=", Comparison::NotEqual), ("==", Comparison::Equal)] {
        if let Some(index) = find_top_level(input, operator)? {
            let left = input[..index].trim();
            let right = input[index + operator.len()..].trim();
            if left.is_empty() || right.is_empty() {
                return Err(ConditionError::InvalidValue {
                    value: input.to_owned(),
                });
            }
            return Ok(Some((left, comparison, right)));
        }
    }
    Ok(None)
}

fn find_top_level(input: &str, needle: &str) -> Result<Option<usize>, ConditionError> {
    let mut cursor = 0;
    while cursor < input.len() {
        if input[cursor..].starts_with(needle) && top_level_at(input, cursor)? {
            return Ok(Some(cursor));
        }
        cursor += input[cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
    }
    Ok(None)
}

fn top_level_at(input: &str, position: usize) -> Result<bool, ConditionError> {
    let mut depth = 0usize;
    let mut quote = None;
    for character in input[..position].chars() {
        if let Some(expected) = quote {
            if character == expected {
                quote = None;
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
        } else if character == '(' {
            depth += 1;
        } else if character == ')' {
            depth = depth
                .checked_sub(1)
                .ok_or(ConditionError::UnbalancedParentheses)?;
        }
    }
    Ok(depth == 0 && quote.is_none())
}
