use crate::context::ProviderContext;
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TemplateError {
    EmptyPlaceholder,
    UnterminatedPlaceholder { argument: String },
    UnknownPlaceholder { argument: String, name: String },
    Nul,
}

impl fmt::Display for TemplateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPlaceholder => formatter.write_str("argv placeholder may not be empty"),
            Self::UnterminatedPlaceholder { argument } => {
                write!(
                    formatter,
                    "argv template {argument:?} has an unterminated placeholder"
                )
            }
            Self::UnknownPlaceholder { argument, name } => write!(
                formatter,
                "argv template {argument:?} uses unknown placeholder {name:?}"
            ),
            Self::Nul => formatter.write_str("argv template may not contain NUL"),
        }
    }
}

impl std::error::Error for TemplateError {}

/// Expand only known data placeholders.  Values are returned as individual
/// argv elements and are never interpreted as shell source.
pub fn expand_argv(
    arguments: &[String],
    provider: &str,
    context: &ProviderContext,
) -> Result<Vec<String>, TemplateError> {
    arguments
        .iter()
        .map(|argument| expand_argument(argument, provider, context))
        .collect()
}

fn expand_argument(
    argument: &str,
    provider: &str,
    context: &ProviderContext,
) -> Result<String, TemplateError> {
    if argument.contains('\0') {
        return Err(TemplateError::Nul);
    }
    let mut output = String::with_capacity(argument.len());
    let mut rest = argument;
    while let Some(start) = rest.find('{') {
        output.push_str(&rest[..start]);
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('}') else {
            return Err(TemplateError::UnterminatedPlaceholder {
                argument: argument.to_owned(),
            });
        };
        let name = &after_start[..end];
        if name.is_empty() {
            return Err(TemplateError::EmptyPlaceholder);
        }
        let value = if name == "provider" {
            provider.to_owned()
        } else {
            context
                .template_value(name)
                .ok_or_else(|| TemplateError::UnknownPlaceholder {
                    argument: argument.to_owned(),
                    name: name.to_owned(),
                })?
        };
        output.push_str(&value);
        rest = &after_start[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}
