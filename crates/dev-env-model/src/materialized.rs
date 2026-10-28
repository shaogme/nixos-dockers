use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{
    provenance::Origin, validation::StringValidation, ModelError, ModelErrorReason, Sensitivity,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnvValue {
    value: String,
    origin: Option<Origin>,
    #[serde(default)]
    sensitivity: Sensitivity,
    #[serde(default)]
    provider: Option<String>,
}

impl EnvValue {
    pub fn new(
        value: impl Into<String>,
        origin: Option<Origin>,
        sensitivity: Sensitivity,
        provider: Option<String>,
    ) -> Self {
        Self {
            value: value.into(),
            origin,
            sensitivity,
            provider,
        }
    }

    pub fn public(value: impl Into<String>) -> Self {
        Self::new(value, None, Sensitivity::Public, None)
    }

    pub fn validate(&self, name: &str) -> Result<(), ModelError> {
        if !name.is_posix_env_name() {
            return Err(ModelError::InvalidEnvironmentName {
                location: format!("environment.variables.{name}"),
                name: name.to_owned(),
            });
        }
        if self.value.contains('\0') {
            return Err(ModelError::InvalidEnvironmentValue {
                location: format!("environment.variables.{name}"),
                reason: ModelErrorReason::Nul,
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
    values: BTreeMap<String, EnvValue>,
    config_fingerprint: [u8; 32],
    #[serde(default)]
    provider_receipts: Vec<ProviderReceipt>,
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

    pub fn insert(&mut self, name: String, value: EnvValue) {
        let _ = self.values.insert(name, value);
    }

    pub fn remove(&mut self, name: &str) {
        let _ = self.values.remove(name);
    }

    pub fn push_provider_receipt(&mut self, receipt: ProviderReceipt) {
        self.provider_receipts.push(receipt);
    }

    pub fn set_config_fingerprint(&mut self, fingerprint: [u8; 32]) {
        self.config_fingerprint = fingerprint;
    }
}

impl EnvValue {
    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn origin(&self) -> Option<&Origin> {
        self.origin.as_ref()
    }

    pub fn sensitivity(&self) -> Sensitivity {
        self.sensitivity
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }
}

impl MaterializedEnv {
    pub fn values(&self) -> &BTreeMap<String, EnvValue> {
        &self.values
    }

    pub fn config_fingerprint(&self) -> [u8; 32] {
        self.config_fingerprint
    }

    pub fn provider_receipts(&self) -> &[ProviderReceipt] {
        &self.provider_receipts
    }
}
