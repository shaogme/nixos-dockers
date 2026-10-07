use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use crate::validation::validate_path;
use crate::ModelError;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum PathRenderError {
    UnterminatedInterpolation,
    MissingValue { reference: String },
    InvalidRenderedPath,
}

impl fmt::Display for PathRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnterminatedInterpolation => {
                formatter.write_str("unterminated structured interpolation")
            }
            Self::MissingValue { reference } => {
                write!(formatter, "missing interpolation value {reference:?}")
            }
            Self::InvalidRenderedPath => {
                formatter.write_str("rendered value is not a safe absolute path")
            }
        }
    }
}

impl std::error::Error for PathRenderError {}

/// A concrete absolute path with optional structured references.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PathTemplate(String);

impl<'de> Deserialize<'de> for PathTemplate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).map_err(serde::de::Error::custom)
    }
}

impl PathTemplate {
    pub fn new(input: &str) -> Result<Self, ModelError> {
        validate_path("path", input)?;
        validate_interpolations(input)?;
        Ok(Self(input.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Render only the structured references supplied by the caller.  No
    /// shell expansion or environment lookup is performed here.
    pub fn render(&self, values: &BTreeMap<String, String>) -> Result<String, PathRenderError> {
        let mut output = String::new();
        let mut rest = self.0.as_str();
        while let Some(start) = rest.find("${") {
            output.push_str(&rest[..start]);
            let after_start = &rest[start + 2..];
            let end = after_start
                .find('}')
                .ok_or(PathRenderError::UnterminatedInterpolation)?;
            let reference = &after_start[..end];
            let value = values
                .get(reference)
                .ok_or_else(|| PathRenderError::MissingValue {
                    reference: reference.to_owned(),
                })?;
            output.push_str(value);
            rest = &after_start[end + 1..];
        }
        output.push_str(rest);
        if output.is_empty()
            || !output.starts_with('/')
            || output.contains('\0')
            || output.contains('\n')
            || output.contains('\r')
            || output.contains('$')
            || output.contains('{')
            || output.contains('}')
            || output.split('/').any(|component| component == "..")
            || output.contains("$(")
            || output.contains('`')
            || output.contains(';')
            || output.contains('|')
            || output.contains('&')
            || output.contains('>')
            || output.contains('<')
        {
            return Err(PathRenderError::InvalidRenderedPath);
        }
        Ok(output)
    }
}

fn validate_interpolations(input: &str) -> Result<(), ModelError> {
    let mut cursor = 0;
    while cursor < input.len() {
        let character = input[cursor..]
            .chars()
            .next()
            .expect("cursor always points at a character boundary");
        if character == '$' {
            if !input[cursor..].starts_with("${") {
                return Err(ModelError::InvalidPath {
                    location: "path".to_owned(),
                    reason: crate::ModelErrorReason::InvalidPath,
                });
            }
            let after_start = &input[cursor + 2..];
            let end = after_start
                .find('}')
                .ok_or_else(|| ModelError::InvalidPath {
                    location: "path".to_owned(),
                    reason: crate::ModelErrorReason::InvalidPath,
                })?;
            let reference = &after_start[..end];
            if !crate::validation::is_reference(reference) {
                return Err(ModelError::InvalidPath {
                    location: "path".to_owned(),
                    reason: crate::ModelErrorReason::Unsupported,
                });
            }
            cursor += end + 3;
        } else if character == '}' {
            return Err(ModelError::InvalidPath {
                location: "path".to_owned(),
                reason: crate::ModelErrorReason::InvalidPath,
            });
        } else {
            cursor += character.len_utf8();
        }
    }
    Ok(())
}

impl FromStr for PathTemplate {
    type Err = ModelError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::new(input)
    }
}
