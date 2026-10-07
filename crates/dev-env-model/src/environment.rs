use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::validation::{validate_concrete_path, validate_env_name};
use crate::Condition;
use crate::{ModelError, ModelErrorReason};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfiguredValuePrecedence {
    #[default]
    Locked,
    Ambient,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathMode {
    #[default]
    Merge,
    Replace,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentPath {
    #[serde(default)]
    pub prepend: Vec<String>,
    #[serde(default)]
    pub append: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

impl EnvironmentPath {
    pub fn validate(&self) -> Result<(), ModelError> {
        for (kind, entries) in [
            ("prepend", &self.prepend),
            ("append", &self.append),
            ("remove", &self.remove),
        ] {
            for (index, entry) in entries.iter().enumerate() {
                validate_concrete_path(&format!("environment.path.{kind}[{index}]"), entry)?;
            }
        }
        Ok(())
    }

    /// Apply structured PATH operations while preserving first occurrence.
    /// `prepend` entries have priority over inherited and appended entries.
    pub fn resolve(&self, inherited: &[String]) -> Result<Vec<String>, ModelError> {
        self.validate()?;
        let removed = self.remove.iter().collect::<BTreeSet<_>>();
        let mut result = Vec::new();
        for entry in self
            .prepend
            .iter()
            .chain(inherited.iter())
            .chain(self.append.iter())
        {
            if removed.contains(entry) || result.iter().any(|existing| existing == entry) {
                continue;
            }
            result.push(entry.clone());
        }
        Ok(result)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionalEnvironmentVariable {
    pub value: String,
    pub when: String,
}

impl ConditionalEnvironmentVariable {
    pub fn validate(&self, name: &str) -> Result<(), ModelError> {
        let location = format!("environment.conditional_variables.{name}");
        validate_env_name(&location, name)?;
        if self.value.contains('\0') {
            return Err(ModelError::InvalidEnvironmentValue {
                location: format!("{location}.value"),
                reason: ModelErrorReason::Nul,
            });
        }
        Condition::parse(&self.when).map_err(|reason| ModelError::InvalidCondition {
            location: format!("{location}.when"),
            reason,
        })?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentConfig {
    #[serde(default = "default_true")]
    pub inherit_process: bool,
    #[serde(default)]
    pub configured_value_precedence: ConfiguredValuePrecedence,
    #[serde(default)]
    pub variables: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub conditional_variables: std::collections::BTreeMap<String, ConditionalEnvironmentVariable>,
    #[serde(default)]
    pub path: EnvironmentPath,
}

fn default_true() -> bool {
    true
}

impl Default for EnvironmentConfig {
    fn default() -> Self {
        Self {
            inherit_process: true,
            configured_value_precedence: ConfiguredValuePrecedence::Locked,
            variables: Default::default(),
            conditional_variables: Default::default(),
            path: Default::default(),
        }
    }
}

impl EnvironmentConfig {
    pub fn validate(&self) -> Result<(), ModelError> {
        for (name, value) in &self.variables {
            validate_env_name(&format!("environment.variables.{name}"), name)?;
            if value.contains('\0') {
                return Err(ModelError::InvalidEnvironmentValue {
                    location: format!("environment.variables.{name}"),
                    reason: ModelErrorReason::Nul,
                });
            }
        }
        for (name, variable) in &self.conditional_variables {
            variable.validate(name)?;
            if self.variables.contains_key(name) {
                return Err(ModelError::InvalidValue {
                    location: format!("environment.conditional_variables.{name}"),
                    reason: ModelErrorReason::Duplicate,
                });
            }
        }
        self.path.validate()
    }
}
