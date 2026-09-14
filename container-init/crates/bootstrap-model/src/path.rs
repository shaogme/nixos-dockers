use std::collections::BTreeMap;
use std::str::FromStr;

use crate::error::ModelError;
use crate::validation::{validate_path_template, validate_rendered_path};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathTemplate(String);

impl PathTemplate {
    pub fn new(input: &str) -> Result<Self, ModelError> {
        validate_path_template("path", input)?;
        Ok(Self(input.to_owned()))
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
        validate_rendered_path(&output)?;
        Ok(output)
    }
}

impl FromStr for PathTemplate {
    type Err = ModelError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::new(input)
    }
}
