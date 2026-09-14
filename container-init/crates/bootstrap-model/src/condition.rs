use crate::path::PathTemplate;
use crate::validation::{is_config_path, is_env_name, is_reference};
use crate::ModelError;

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
        let input = strip_outer_parentheses(input)?;

        if input == "always" {
            return Ok(Self::Always);
        }
        if input == "context.path_exists_or_create" {
            return Ok(Self::ContextPathExistsOrCreate);
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
        if input == "true" {
            return Ok(Self::Boolean(true));
        }
        if input == "false" {
            return Ok(Self::Boolean(false));
        }
        if let Some(argument) = function_argument(input, "exists")? {
            return Ok(Self::Exists(PathTemplate::new(argument)?));
        }
        if let Some(argument) = function_argument(input, "writable")? {
            return Ok(Self::Writable(PathTemplate::new(argument)?));
        }
        if let Some(argument) = function_argument(input, "input_set")? {
            if !is_env_name(argument) {
                return Err(invalid(format!("invalid input_set name {argument:?}")));
            }
            return Ok(Self::InputSet(argument.to_owned()));
        }
        if let Some(feature) = input.strip_prefix("feature:") {
            if feature.is_empty() || !is_config_path(feature) {
                return Err(invalid(format!("invalid feature path {feature:?}")));
            }
            return Ok(Self::Feature(feature.to_owned()));
        }
        if let Some((left, operator, right)) = split_comparison(input)? {
            let left = ConditionValue::parse(left)?;
            let right = ConditionValue::parse(right)?;
            return Ok(match operator {
                Comparison::Equal => Self::Equal(left, right),
                Comparison::NotEqual => Self::NotEqual(left, right),
            });
        }
        Err(invalid(format!(
            "unsupported bootstrap condition {input:?}"
        )))
    }
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

#[derive(Clone, Copy)]
enum Comparison {
    Equal,
    NotEqual,
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

fn split_comparison(input: &str) -> Result<Option<(&str, Comparison, &str)>, ModelError> {
    for (operator, comparison) in [("!=", Comparison::NotEqual), ("==", Comparison::Equal)] {
        if let Some(index) = find_top_level(input, operator)? {
            let left = input[..index].trim();
            let right = input[index + operator.len()..].trim();
            if left.is_empty() || right.is_empty() {
                return Err(invalid("comparison requires two operands"));
            }
            return Ok(Some((left, comparison, right)));
        }
    }
    Ok(None)
}

fn split_top_level<'a>(input: &'a str, operator: &str) -> Result<Vec<&'a str>, ModelError> {
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
        return Err(invalid(format!(
            "operator {operator:?} has an empty operand"
        )));
    }
    Ok(parts)
}

fn find_top_level(input: &str, needle: &str) -> Result<Option<usize>, ModelError> {
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

fn top_level_at(input: &str, position: usize) -> Result<bool, ModelError> {
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
                .ok_or_else(|| invalid("unmatched ')' in condition"))?;
        }
    }
    if quote.is_some() {
        return Err(invalid("unterminated quote in condition"));
    }
    Ok(depth == 0)
}

fn strip_outer_parentheses(input: &str) -> Result<&str, ModelError> {
    let mut output = input;
    loop {
        if !output.starts_with('(') || !output.ends_with(')') {
            return Ok(output);
        }
        let mut depth = 0usize;
        let mut quote = None;
        let mut closes_at = None;
        for (index, character) in output.char_indices() {
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
                    .ok_or_else(|| invalid("unmatched ')' in condition"))?;
                if depth == 0 {
                    closes_at = Some(index);
                    break;
                }
            }
        }
        if closes_at != Some(output.len() - 1) {
            return Ok(output);
        }
        output = output[1..output.len() - 1].trim();
    }
}

fn invalid(message: impl Into<String>) -> ModelError {
    ModelError::Invalid {
        location: "bootstrap.condition".to_owned(),
        message: message.into(),
    }
}
