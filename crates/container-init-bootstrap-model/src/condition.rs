use std::{collections::BTreeMap, str::FromStr};

use condition_expr::{parse, Expr as SharedExpr, ParseError as SharedParseError};

use crate::{input::BootstrapInput, ModelError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathTemplate(String);

impl PathTemplate {
    pub fn new(input: &str) -> Result<Self, ModelError> {
        Self::validate_template("path", input)?;
        Ok(Self(input.to_owned()))
    }

    pub(crate) fn validate_template(location: &str, value: &str) -> Result<(), ModelError> {
        if value.is_empty() {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "path may not be empty".to_owned(),
            });
        }
        if value.contains('\0') {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "path may not contain NUL".to_owned(),
            });
        }
        if value.contains("$(")
            || value.contains('`')
            || value.contains(';')
            || value.contains('|')
            || value.contains('&')
            || value.contains('>')
            || value.contains('<')
        {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "shell expressions are not allowed in paths".to_owned(),
            });
        }
        if value.contains('\n') || value.contains('\r') {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "path may not contain newlines".to_owned(),
            });
        }
        Self::validate_interpolations(location, value)?;
        if !value.starts_with('/') && !value.starts_with("${") {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "path must be absolute or start with a structured interpolation"
                    .to_owned(),
            });
        }
        if value.split('/').any(|component| component == "..") {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "path traversal with '..' is not allowed".to_owned(),
            });
        }
        Ok(())
    }

    fn validate_interpolations(location: &str, value: &str) -> Result<(), ModelError> {
        let mut cursor = 0;
        while cursor < value.len() {
            let character = value[cursor..]
                .chars()
                .next()
                .expect("cursor is a character boundary");
            if character != '$' && character != '{' && character != '}' {
                cursor += character.len_utf8();
                continue;
            }
            if character != '$' || !value[cursor..].starts_with("${") {
                return Err(ModelError::Invalid {
                    location: location.to_owned(),
                    message: "only ${namespace.reference} interpolation is allowed".to_owned(),
                });
            }
            let after_start = &value[cursor + 2..];
            let end = after_start.find('}').ok_or_else(|| ModelError::Invalid {
                location: location.to_owned(),
                message: "unterminated structured interpolation".to_owned(),
            })?;
            let reference = &after_start[..end];
            if !Condition::is_reference(reference) {
                return Err(ModelError::Invalid {
                    location: location.to_owned(),
                    message: format!("unsupported structured reference {reference:?}"),
                });
            }
            cursor += 2 + end + 1;
        }
        Ok(())
    }

    pub(crate) fn validate_rendered(value: &str) -> Result<(), String> {
        if value.is_empty() || !value.starts_with('/') {
            return Err("rendered path must be absolute".to_owned());
        }
        if value.contains('\0') {
            return Err("rendered path may not contain NUL".to_owned());
        }
        if value.contains('\n') || value.contains('\r') {
            return Err("rendered path may not contain newlines".to_owned());
        }
        if value.contains("$(")
            || value.contains('`')
            || value.contains(';')
            || value.contains('|')
            || value.contains('&')
            || value.contains('>')
            || value.contains('<')
        {
            return Err("rendered path may not contain shell metacharacters".to_owned());
        }
        if value.split('/').any(|component| component == "..") {
            return Err("rendered path may not contain '..'".to_owned());
        }
        Ok(())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Render a template after resolving every structured reference. The
    /// caller supplies values keyed by the exact reference, e.g.
    /// `identity.home` or `input.CONTAINER_HOME`.
    pub fn render(&self, values: &BTreeMap<String, String>) -> Result<String, String> {
        let mut output = String::new();
        let mut rest = self.0.as_str();
        while let Some(start) = rest.find("${") {
            output.push_str(&rest[..start]);
            let after_start = &rest[start + 2..];
            let end = after_start
                .find('}')
                .ok_or_else(|| "unterminated path interpolation".to_owned())?;
            let reference = &after_start[..end];
            let value = values
                .get(reference)
                .ok_or_else(|| format!("missing path interpolation value {reference:?}"))?;
            output.push_str(value);
            rest = &after_start[end + 1..];
        }
        output.push_str(rest);
        Self::validate_rendered(&output)?;
        Ok(output)
    }
}

impl FromStr for PathTemplate {
    type Err = ModelError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::new(input)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConditionValue {
    Reference(String),
    Literal(String),
    Boolean(bool),
}

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
    fn is_reference(value: &str) -> bool {
        let (namespace, name) = value.split_once('.').unwrap_or(("", ""));
        match namespace {
            "bootstrap" => name == "workspace_root",
            "identity" => matches!(name, "uid" | "gid" | "user" | "home" | "target"),
            "context" => matches!(name, "cwd" | "os" | "arch"),
            "input" => BootstrapInput::is_env_name(name),
            "config" => Self::is_config_path(name),
            "env" => BootstrapInput::is_env_name(name),
            _ => false,
        }
    }

    pub(crate) fn is_config_path(value: &str) -> bool {
        !value.is_empty()
            && !value.split('.').any(|part| part.is_empty() || part == "..")
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
            })
    }

    pub fn parse(input: &str) -> Result<Self, ModelError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(invalid("condition may not be empty"));
        }
        from_shared(parse(input).map_err(map_shared_error)?)
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
        if !BootstrapInput::is_env_name(argument) {
            return Err(invalid(format!("invalid input_set name {argument:?}")));
        }
        return Ok(Condition::InputSet(argument.to_owned()));
    }
    if let Some(feature) = input.strip_prefix("feature:") {
        if feature.is_empty() || !Condition::is_config_path(feature) {
            return Err(invalid(format!("invalid feature path {feature:?}")));
        }
        return Ok(Condition::Feature(feature.to_owned()));
    }
    Err(invalid(format!(
        "unsupported bootstrap condition {input:?}"
    )))
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
        if Condition::is_reference(input) {
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
