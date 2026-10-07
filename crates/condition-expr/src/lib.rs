//! Shared grammar for the boolean structure of DSL conditions.
//!
//! Product models interpret leaf expressions and comparison values so they
//! can retain their own reference namespaces and evaluation rules.

use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Expr {
    Atom(String),
    Equal(String, String),
    NotEqual(String, String),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    Empty,
    EmptyOperand { operator: String },
    InvalidComparison(String),
    UnterminatedString,
    UnbalancedParentheses,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("condition may not be empty"),
            Self::EmptyOperand { operator } => {
                write!(formatter, "operator {operator:?} has an empty operand")
            }
            Self::InvalidComparison(expression) => {
                write!(
                    formatter,
                    "comparison requires two operands in {expression:?}"
                )
            }
            Self::UnterminatedString => formatter.write_str("unterminated condition string"),
            Self::UnbalancedParentheses => formatter.write_str("unbalanced condition parentheses"),
        }
    }
}

impl Error for ParseError {}

pub fn parse(input: &str) -> Result<Expr, ParseError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(ParseError::Empty);
    }
    let input = strip_outer_parentheses(input)?;
    let or_parts = split_top_level(input, "||")?;
    if or_parts.len() > 1 {
        return Ok(Expr::Or(
            or_parts
                .into_iter()
                .map(parse)
                .collect::<Result<Vec<_>, _>>()?,
        ));
    }
    let and_parts = split_top_level(input, "&&")?;
    if and_parts.len() > 1 {
        return Ok(Expr::And(
            and_parts
                .into_iter()
                .map(parse)
                .collect::<Result<Vec<_>, _>>()?,
        ));
    }
    if let Some(rest) = input.strip_prefix('!') {
        return Ok(Expr::Not(Box::new(parse(rest)?)));
    }
    if let Some((left, operator, right)) = split_comparison(input)? {
        return Ok(match operator {
            "==" => Expr::Equal(left.to_owned(), right.to_owned()),
            "!=" => Expr::NotEqual(left.to_owned(), right.to_owned()),
            _ => unreachable!("only supported comparison operators are returned"),
        });
    }
    Ok(Expr::Atom(input.to_owned()))
}

fn strip_outer_parentheses(input: &str) -> Result<&str, ParseError> {
    if !input.starts_with('(') {
        return Ok(input);
    }
    let mut depth = 0usize;
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
                .ok_or(ParseError::UnbalancedParentheses)?;
            if depth == 0 {
                if input[index + character.len_utf8()..].trim().is_empty() {
                    return strip_outer_parentheses(&input[1..index]);
                }
                return Ok(input);
            }
        }
    }
    if quote.is_some() {
        Err(ParseError::UnterminatedString)
    } else {
        Err(ParseError::UnbalancedParentheses)
    }
}

fn split_top_level<'a>(input: &'a str, operator: &str) -> Result<Vec<&'a str>, ParseError> {
    let mut parts = Vec::new();
    let mut start = 0;
    scan_top_level(input, operator, |index| {
        parts.push(input[start..index].trim());
        start = index + operator.len();
    })?;
    if parts.is_empty() {
        return Ok(vec![input]);
    }
    parts.push(input[start..].trim());
    if parts.iter().any(|part| part.is_empty()) {
        return Err(ParseError::EmptyOperand {
            operator: operator.to_owned(),
        });
    }
    Ok(parts)
}

fn split_comparison(input: &str) -> Result<Option<(&str, &str, &str)>, ParseError> {
    // Keep the product parsers' historical operator preference: `!=` is
    // searched before `==`, even if both appear in the same leaf.
    for operator in ["!=", "=="] {
        let mut found = None;
        scan_top_level(input, operator, |index| {
            if found.is_none() {
                found = Some(index);
            }
        })?;
        if let Some(index) = found {
            let left = input[..index].trim();
            let right = input[index + operator.len()..].trim();
            if left.is_empty() || right.is_empty() {
                return Err(ParseError::InvalidComparison(input.to_owned()));
            }
            return Ok(Some((left, operator, right)));
        }
    }
    Ok(None)
}

fn scan_top_level(
    input: &str,
    needle: &str,
    mut found: impl FnMut(usize),
) -> Result<(), ParseError> {
    let mut depth = 0usize;
    let mut quote = None;
    let mut cursor = 0;
    while cursor < input.len() {
        let character = input[cursor..].chars().next().expect("cursor is in bounds");
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
                .ok_or(ParseError::UnbalancedParentheses)?;
        } else if depth == 0 && input[cursor..].starts_with(needle) {
            found(cursor);
            cursor += needle.len();
            continue;
        }
        cursor += character.len_utf8();
    }
    if quote.is_some() {
        return Err(ParseError::UnterminatedString);
    }
    if depth != 0 {
        return Err(ParseError::UnbalancedParentheses);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse, Expr, ParseError};

    #[test]
    fn parses_boolean_precedence_parentheses_and_comparisons() {
        assert_eq!(
            parse("a || b && !(c == 'x||y')").unwrap(),
            Expr::Or(vec![
                Expr::Atom("a".to_owned()),
                Expr::And(vec![
                    Expr::Atom("b".to_owned()),
                    Expr::Not(Box::new(Expr::Equal("c".to_owned(), "'x||y'".to_owned(),))),
                ]),
            ])
        );
        assert_eq!(parse("((a))").unwrap(), Expr::Atom("a".to_owned()));
    }

    #[test]
    fn reports_empty_operands_and_malformed_grouping() {
        assert!(matches!(
            parse("a &&"),
            Err(ParseError::EmptyOperand { operator }) if operator == "&&"
        ));
        assert_eq!(
            parse("a == ").unwrap_err(),
            ParseError::InvalidComparison("a ==".to_owned())
        );
        assert_eq!(
            parse("(a || b").unwrap_err(),
            ParseError::UnbalancedParentheses
        );
        assert_eq!(
            parse("a == \"unfinished").unwrap_err(),
            ParseError::UnterminatedString
        );
    }
}
