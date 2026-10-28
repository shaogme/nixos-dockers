use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::{validation::StringValidation, Condition, ModelError, ModelErrorReason};

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
    prepend: Vec<String>,
    #[serde(default)]
    append: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

impl EnvironmentPath {
    pub fn new(prepend: Vec<String>, append: Vec<String>, remove: Vec<String>) -> Self {
        Self {
            prepend,
            append,
            remove,
        }
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        for (kind, entries) in [
            ("prepend", &self.prepend),
            ("append", &self.append),
            ("remove", &self.remove),
        ] {
            for (index, entry) in entries.iter().enumerate() {
                entry.validate_concrete_path(&format!("environment.path.{kind}[{index}]"))?;
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
    value: String,
    when: String,
}

impl ConditionalEnvironmentVariable {
    pub fn new(value: impl Into<String>, when: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            when: when.into(),
        }
    }

    pub fn validate(&self, name: &str) -> Result<(), ModelError> {
        let location = format!("environment.conditional_variables.{name}");
        name.validate_env_name(&location)?;
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
    inherit_process: bool,
    #[serde(default)]
    configured_value_precedence: ConfiguredValuePrecedence,
    #[serde(default)]
    variables: BTreeMap<String, String>,
    #[serde(default)]
    conditional_variables: BTreeMap<String, ConditionalEnvironmentVariable>,
    #[serde(default)]
    path: EnvironmentPath,
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
    pub fn new(
        inherit_process: bool,
        configured_value_precedence: ConfiguredValuePrecedence,
        variables: BTreeMap<String, String>,
        conditional_variables: BTreeMap<String, ConditionalEnvironmentVariable>,
        path: EnvironmentPath,
    ) -> Self {
        Self {
            inherit_process,
            configured_value_precedence,
            variables,
            conditional_variables,
            path,
        }
    }

    pub fn with_variables(mut self, variables: BTreeMap<String, String>) -> Self {
        self.variables = variables;
        self
    }

    pub fn with_conditional_variables(
        mut self,
        conditional_variables: BTreeMap<String, ConditionalEnvironmentVariable>,
    ) -> Self {
        self.conditional_variables = conditional_variables;
        self
    }

    pub fn with_configured_value_precedence(
        mut self,
        precedence: ConfiguredValuePrecedence,
    ) -> Self {
        self.configured_value_precedence = precedence;
        self
    }

    pub fn with_conditional_variable(
        mut self,
        name: impl Into<String>,
        variable: ConditionalEnvironmentVariable,
    ) -> Self {
        self.conditional_variables.insert(name.into(), variable);
        self
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        for (name, value) in &self.variables {
            name.validate_env_name(&format!("environment.variables.{name}"))?;
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

impl EnvironmentPath {
    pub fn prepend(&self) -> &[String] {
        &self.prepend
    }

    pub fn append(&self) -> &[String] {
        &self.append
    }

    pub fn remove(&self) -> &[String] {
        &self.remove
    }
}

impl ConditionalEnvironmentVariable {
    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn when(&self) -> &str {
        &self.when
    }
}

impl EnvironmentConfig {
    pub fn inherit_process(&self) -> bool {
        self.inherit_process
    }

    pub fn configured_value_precedence(&self) -> ConfiguredValuePrecedence {
        self.configured_value_precedence
    }

    pub fn variables(&self) -> &BTreeMap<String, String> {
        &self.variables
    }

    pub fn conditional_variables(&self) -> &BTreeMap<String, ConditionalEnvironmentVariable> {
        &self.conditional_variables
    }

    pub fn path(&self) -> &EnvironmentPath {
        &self.path
    }
}
