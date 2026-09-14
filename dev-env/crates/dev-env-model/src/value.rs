use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The intentionally small dynamic value tree used by `features` and
/// override values.  Keeping this type in the model avoids coupling the
/// schema to a particular document parser.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ValueTree {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Array(Vec<ValueTree>),
    Map(BTreeMap<String, ValueTree>),
}

impl Default for ValueTree {
    fn default() -> Self {
        Self::Map(BTreeMap::new())
    }
}

impl ValueTree {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn is_scalar(&self) -> bool {
        matches!(
            self,
            Self::Null | Self::Bool(_) | Self::Integer(_) | Self::Float(_) | Self::String(_)
        )
    }

    pub(crate) fn validate(&self, location: &str) -> Result<(), crate::ModelError> {
        match self {
            Self::String(value) if value.contains('\0') => Err(crate::ModelError::InvalidValue {
                location: location.to_owned(),
                reason: crate::ModelErrorReason::Nul,
            }),
            Self::String(_) | Self::Null | Self::Bool(_) | Self::Integer(_) | Self::Float(_) => {
                Ok(())
            }
            Self::Array(values) => values
                .iter()
                .enumerate()
                .try_for_each(|(index, value)| value.validate(&format!("{location}[{index}]"))),
            Self::Map(values) => values.iter().try_for_each(|(key, value)| {
                if key.is_empty() || key.contains('\0') {
                    return Err(crate::ModelError::InvalidValue {
                        location: location.to_owned(),
                        reason: crate::ModelErrorReason::InvalidMapKey,
                    });
                }
                value.validate(&format!("{location}.{key}"))
            }),
        }
    }
}
