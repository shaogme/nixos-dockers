use dev_env_model::{ResolvedConfig, ValueTree};
use serde_json::{to_value, Error as JsonError, Value};
use std::{collections::BTreeMap, error::Error, fmt};

#[derive(Debug)]
pub enum ConfigTreeError {
    Serialize { source: JsonError },
    UnsupportedNumber,
}

impl ConfigTreeError {
    pub fn is_unsupported_number(&self) -> bool {
        matches!(self, Self::UnsupportedNumber)
    }

    pub(crate) fn to_value_tree(config: &ResolvedConfig) -> Result<ValueTree, Self> {
        let value = to_value(config).map_err(|source| Self::Serialize { source })?;
        convert(value)
    }
}

impl fmt::Display for ConfigTreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialize { source } => write!(
                formatter,
                "could not encode provider configuration: {source}"
            ),
            Self::UnsupportedNumber => {
                formatter.write_str("provider configuration contains an unsupported number")
            }
        }
    }
}

impl Error for ConfigTreeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Serialize { source } => Some(source),
            Self::UnsupportedNumber => None,
        }
    }
}

fn convert(value: Value) -> Result<ValueTree, ConfigTreeError> {
    Ok(match value {
        Value::Null => ValueTree::Null,
        Value::Bool(value) => ValueTree::Bool(value),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                ValueTree::Integer(value)
            } else if let Some(value) = value.as_u64() {
                match i64::try_from(value) {
                    Ok(value) => ValueTree::Integer(value),
                    Err(_) => ValueTree::Float(value as f64),
                }
            } else if let Some(value) = value.as_f64() {
                ValueTree::Float(value)
            } else {
                return Err(ConfigTreeError::UnsupportedNumber);
            }
        }
        Value::String(value) => ValueTree::String(value),
        Value::Array(values) => ValueTree::Array(
            values
                .into_iter()
                .map(convert)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Value::Object(values) => ValueTree::Map(
            values
                .into_iter()
                .map(|(key, value)| Ok((key, convert(value)?)))
                .collect::<Result<BTreeMap<_, _>, ConfigTreeError>>()?,
        ),
    })
}
