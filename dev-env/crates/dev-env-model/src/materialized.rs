use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::provenance::Origin;
use crate::validation::validate_env_name;
use crate::{ModelError, Sensitivity};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnvValue {
    pub value: String,
    pub origin: Option<Origin>,
    #[serde(default)]
    pub sensitivity: Sensitivity,
    #[serde(default)]
    pub provider: Option<String>,
}

impl EnvValue {
    pub fn public(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            origin: None,
            sensitivity: Sensitivity::Public,
            provider: None,
        }
    }

    pub fn validate(&self, name: &str) -> Result<(), ModelError> {
        validate_env_name(&format!("environment.variables.{name}"), name)?;
        if self.value.contains('\0') {
            return Err(ModelError::InvalidEnvironmentValue {
                location: format!("environment.variables.{name}"),
                reason: crate::ModelErrorReason::Nul,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderReceipt {
    pub provider: String,
    pub config_fingerprint: [u8; 32],
    pub workspace_fingerprint: [u8; 32],
    pub version: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MaterializedEnv {
    pub values: BTreeMap<String, EnvValue>,
    pub config_fingerprint: [u8; 32],
    #[serde(default)]
    pub provider_receipts: Vec<ProviderReceipt>,
}

impl MaterializedEnv {
    pub fn new(values: BTreeMap<String, EnvValue>, config_fingerprint: [u8; 32]) -> Self {
        Self {
            values,
            config_fingerprint,
            provider_receipts: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        for (name, value) in &self.values {
            value.validate(name)?;
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&EnvValue> {
        self.values.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &EnvValue)> {
        self.values
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }
}
