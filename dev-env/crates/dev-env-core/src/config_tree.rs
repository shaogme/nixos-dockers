use dev_env_model::{ResolvedConfig, ValueTree};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum ConfigTreeError {
    Serialize { source: serde_json::Error },
    UnsupportedNumber,
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

pub(crate) fn to_value_tree(config: &ResolvedConfig) -> Result<ValueTree, ConfigTreeError> {
    let value =
        serde_json::to_value(config).map_err(|source| ConfigTreeError::Serialize { source })?;
    convert(value)
}

fn convert(value: serde_json::Value) -> Result<ValueTree, ConfigTreeError> {
    Ok(match value {
        serde_json::Value::Null => ValueTree::Null,
        serde_json::Value::Bool(value) => ValueTree::Bool(value),
        serde_json::Value::Number(value) => {
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
        serde_json::Value::String(value) => ValueTree::String(value),
        serde_json::Value::Array(values) => ValueTree::Array(
            values
                .into_iter()
                .map(convert)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        serde_json::Value::Object(values) => ValueTree::Map(
            values
                .into_iter()
                .map(|(key, value)| Ok((key, convert(value)?)))
                .collect::<Result<BTreeMap<_, _>, ConfigTreeError>>()?,
        ),
    })
}
