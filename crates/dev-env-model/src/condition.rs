use serde::Serialize;
use std::fmt;

use crate::validation::{is_config_path, is_env_name};
use condition_expr::{Expr as SharedExpr, ParseError as SharedParseError};

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
        let expression = condition_expr::parse(input).map_err(map_shared_error)?;
        from_shared(expression)
    }
}

fn from_shared(expression: SharedExpr) -> Result<Condition, ConditionError> {
    Ok(match expression {
        SharedExpr::Atom(input) => {
            if input == "always" {
                Condition::Always
            } else if input == "true" {
                Condition::Boolean(true)
            } else if input == "false" {
                Condition::Boolean(false)
            } else {
                validate_reference(&input)?;
                Condition::Reference(input)
            }
        }
        SharedExpr::Equal(left, right) => Condition::Equal(
            ConditionValue::parse(&left)?,
            ConditionValue::parse(&right)?,
        ),
        SharedExpr::NotEqual(left, right) => Condition::NotEqual(
            ConditionValue::parse(&left)?,
            ConditionValue::parse(&right)?,
        ),
        SharedExpr::And(children) => Condition::And(
            children
                .into_iter()
                .map(from_shared)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        SharedExpr::Or(children) => Condition::Or(
            children
                .into_iter()
                .map(from_shared)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        SharedExpr::Not(child) => Condition::Not(Box::new(from_shared(*child)?)),
    })
}

fn map_shared_error(error: SharedParseError) -> ConditionError {
    match error {
        SharedParseError::Empty => ConditionError::Empty,
        SharedParseError::EmptyOperand { operator } => ConditionError::EmptyOperand { operator },
        SharedParseError::InvalidComparison(value) => ConditionError::InvalidValue { value },
        SharedParseError::UnterminatedString => ConditionError::UnterminatedString,
        SharedParseError::UnbalancedParentheses => ConditionError::UnbalancedParentheses,
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
