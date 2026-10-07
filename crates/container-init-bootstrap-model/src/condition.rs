use crate::path::PathTemplate;
use crate::validation::{is_config_path, is_env_name, is_reference};
use crate::ModelError;
use condition_expr::{Expr as SharedExpr, ParseError as SharedParseError};

/// A restricted condition AST. It intentionally has no shell/eval escape
/// hatch; the loader can keep the original string for provenance and use this
/// parser for validation before an executor evaluates it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Condition {
    Always,
    Boolean(bool),
    ContextPathExistsOrCreate,
    Exists(PathTemplate),
    Writable(PathTemplate),
    InputSet(String),
    Feature(String),
    Equal(ConditionValue, ConditionValue),
    NotEqual(ConditionValue, ConditionValue),
    And(Vec<Condition>),
    Or(Vec<Condition>),
    Not(Box<Condition>),
}

impl Condition {
    pub fn parse(input: &str) -> Result<Self, ModelError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(invalid("condition may not be empty"));
        }
        from_shared(condition_expr::parse(input).map_err(map_shared_error)?)
    }
}

fn from_shared(expression: SharedExpr) -> Result<Condition, ModelError> {
    match expression {
        SharedExpr::Atom(input) => parse_atom(&input),
        SharedExpr::Equal(left, right) => Ok(Condition::Equal(
            ConditionValue::parse(&left)?,
            ConditionValue::parse(&right)?,
        )),
        SharedExpr::NotEqual(left, right) => Ok(Condition::NotEqual(
            ConditionValue::parse(&left)?,
            ConditionValue::parse(&right)?,
        )),
        SharedExpr::And(children) => Ok(Condition::And(
            children
                .into_iter()
                .map(from_shared)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        SharedExpr::Or(children) => Ok(Condition::Or(
            children
                .into_iter()
                .map(from_shared)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        SharedExpr::Not(child) => Ok(Condition::Not(Box::new(from_shared(*child)?))),
    }
}

fn parse_atom(input: &str) -> Result<Condition, ModelError> {
    if input == "always" {
        return Ok(Condition::Always);
    }
    if input == "context.path_exists_or_create" {
        return Ok(Condition::ContextPathExistsOrCreate);
    }
    if input == "true" {
        return Ok(Condition::Boolean(true));
    }
    if input == "false" {
        return Ok(Condition::Boolean(false));
    }
    if let Some(argument) = function_argument(input, "exists")? {
        return Ok(Condition::Exists(PathTemplate::new(argument)?));
    }
    if let Some(argument) = function_argument(input, "writable")? {
        return Ok(Condition::Writable(PathTemplate::new(argument)?));
    }
    if let Some(argument) = function_argument(input, "input_set")? {
        if !is_env_name(argument) {
            return Err(invalid(format!("invalid input_set name {argument:?}")));
        }
        return Ok(Condition::InputSet(argument.to_owned()));
    }
    if let Some(feature) = input.strip_prefix("feature:") {
        if feature.is_empty() || !is_config_path(feature) {
            return Err(invalid(format!("invalid feature path {feature:?}")));
        }
        return Ok(Condition::Feature(feature.to_owned()));
    }
    Err(invalid(format!(
        "unsupported bootstrap condition {input:?}"
    )))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConditionValue {
    Reference(String),
    Literal(String),
    Boolean(bool),
}

impl ConditionValue {
    fn parse(input: &str) -> Result<Self, ModelError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(invalid("comparison value may not be empty"));
        }
        if input == "true" {
            return Ok(Self::Boolean(true));
        }
        if input == "false" {
            return Ok(Self::Boolean(false));
        }
        if input.starts_with('"') || input.starts_with('\'') {
            if input.len() < 2 || input.chars().last() != input.chars().next() {
                return Err(invalid(format!("unterminated comparison string {input:?}")));
            }
            return Ok(Self::Literal(input[1..input.len() - 1].to_owned()));
        }
        if input.chars().all(|character| character.is_ascii_digit()) {
            return Ok(Self::Literal(input.to_owned()));
        }
        if is_reference(input) {
            return Ok(Self::Reference(input.to_owned()));
        }
        Err(invalid(format!("invalid comparison value {input:?}")))
    }
}

fn map_shared_error(error: SharedParseError) -> ModelError {
    let message = match error {
        SharedParseError::Empty => "condition may not be empty".to_owned(),
        SharedParseError::EmptyOperand { operator } => {
            format!("operator {operator:?} has an empty operand")
        }
        SharedParseError::InvalidComparison(_) => "comparison requires two operands".to_owned(),
        SharedParseError::UnterminatedString => "unterminated quote in condition".to_owned(),
        SharedParseError::UnbalancedParentheses => {
            "unmatched or unbalanced parentheses in condition".to_owned()
        }
    };
    invalid(message)
}

fn function_argument<'a>(input: &'a str, name: &str) -> Result<Option<&'a str>, ModelError> {
    let prefix = format!("{name}(");
    if !input.starts_with(&prefix) {
        return Ok(None);
    }
    if !input.ends_with(')') {
        return Err(invalid(format!("unterminated {name}() condition")));
    }
    let argument = &input[prefix.len()..input.len() - 1];
    if argument.is_empty() || argument.contains('(') || argument.contains(')') {
        return Err(invalid(format!("invalid {name}() argument")));
    }
    Ok(Some(argument.trim_matches([' ', '\'', '"'])))
}

fn invalid(message: impl Into<String>) -> ModelError {
    ModelError::Invalid {
        location: "bootstrap.condition".to_owned(),
        message: message.into(),
    }
}
