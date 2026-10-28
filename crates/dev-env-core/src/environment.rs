use crate::{context::RuntimeContext, error::CoreError};
use dev_env_model::{
    ConfiguredValuePrecedence, EnvValue, MaterializedEnv, ModelError, ModelErrorReason, Origin,
    PathMode, ResolvedConfig, Sensitivity,
};
use dev_env_provider::EnvironmentDelta;
use std::{
    collections::BTreeMap,
    env::{join_paths, split_paths},
    ffi::OsStr,
};

pub(crate) struct Environment;

impl Environment {
    pub(crate) fn initial(
        config: &ResolvedConfig,
        context: &RuntimeContext,
    ) -> Result<MaterializedEnv, CoreError> {
        let mut values = BTreeMap::new();
        if config.environment().inherit_process() {
            for (name, value) in context.process_environment() {
                values.insert(
                    name.clone(),
                    Self::checked_value(
                        name,
                        value.clone(),
                        Some(Origin::environment(name.clone())),
                        Sensitivity::Public,
                        None,
                    )?,
                );
            }
        }

        for (name, value) in config.environment().variables() {
            let use_configured = config.environment().configured_value_precedence()
                == ConfiguredValuePrecedence::Locked
                || !values.contains_key(name);
            if use_configured {
                let (origin, sensitivity) =
                    Self::provenance(config, &format!("environment.variables.{name}"));
                values.insert(
                    name.clone(),
                    Self::checked_value(name, value.clone(), origin, sensitivity, None)?,
                );
            }
        }

        Self::apply_configured_path(config, &mut values)?;
        let environment = MaterializedEnv::new(values, [0; 32]);
        environment.validate().map_err(CoreError::Model)?;
        Ok(environment)
    }

    pub(crate) fn configured_provenance(
        config: &ResolvedConfig,
        path: &str,
    ) -> (Option<Origin>, Sensitivity) {
        Self::provenance(config, path)
    }

    pub(crate) fn apply_provider_delta(
        environment: &mut MaterializedEnv,
        delta: &EnvironmentDelta,
        provider: &str,
        sensitivity: Sensitivity,
        path_mode: PathMode,
    ) -> Result<(), CoreError> {
        for name in delta.unset() {
            environment.remove(name);
        }
        for (name, value) in delta.set() {
            if name == "PATH" {
                let value = match path_mode {
                    PathMode::Replace => value.clone(),
                    PathMode::Merge => Self::merge_provider_path(
                        environment.get("PATH").map(|item| item.value()),
                        value,
                    )?,
                };
                environment.insert(
                    name.clone(),
                    EnvValue::new(value, None, sensitivity, Some(provider.to_owned())),
                );
            } else {
                environment.insert(
                    name.clone(),
                    EnvValue::new(value.clone(), None, sensitivity, Some(provider.to_owned())),
                );
            }
        }
        environment.validate().map_err(CoreError::Model)
    }

    fn apply_configured_path(
        config: &ResolvedConfig,
        values: &mut BTreeMap<String, EnvValue>,
    ) -> Result<(), CoreError> {
        let path = config.environment().path();
        if path.prepend().is_empty() && path.append().is_empty() && path.remove().is_empty() {
            return Ok(());
        }
        let inherited = values
            .get("PATH")
            .map(|value| Self::split_path(value.value()))
            .unwrap_or_default();
        let resolved = path.resolve(&inherited).map_err(CoreError::Model)?;
        let path_value = Self::join_path(&resolved)?;
        let (origin, sensitivity) = Self::provenance(config, "environment.path.prepend");
        values.insert(
            "PATH".to_owned(),
            EnvValue::new(path_value, origin, sensitivity, None),
        );
        Ok(())
    }

    fn provenance(config: &ResolvedConfig, path: &str) -> (Option<Origin>, Sensitivity) {
        if let Some(entry) = config.provenance().get(path) {
            return (entry.origins.last().cloned(), entry.sensitivity);
        }
        if path == "environment.path.prepend" {
            for fallback in [
                "environment.path.append",
                "environment.path.remove",
                "environment.variables.PATH",
            ] {
                if let Some(entry) = config.provenance().get(fallback) {
                    return (entry.origins.last().cloned(), entry.sensitivity);
                }
            }
        }
        (None, Sensitivity::Public)
    }

    fn checked_value(
        name: &str,
        value: String,
        origin: Option<Origin>,
        sensitivity: Sensitivity,
        provider: Option<String>,
    ) -> Result<EnvValue, CoreError> {
        let result = EnvValue::new(value, origin, sensitivity, provider);
        result.validate(name).map_err(CoreError::Model)?;
        Ok(result)
    }

    fn split_path(value: &str) -> Vec<String> {
        if value.is_empty() {
            return Vec::new();
        }
        split_paths(OsStr::new(value))
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    fn join_path(values: &[String]) -> Result<String, CoreError> {
        let joined = join_paths(values.iter().map(OsStr::new)).map_err(CoreError::PathJoin)?;
        joined.to_str().map(str::to_owned).ok_or_else(|| {
            CoreError::Model(ModelError::InvalidEnvironmentValue {
                location: "environment.variables.PATH".to_owned(),
                reason: ModelErrorReason::InvalidPath,
            })
        })
    }

    fn merge_provider_path(
        current: Option<&str>,
        provider_path: &str,
    ) -> Result<String, CoreError> {
        let mut values = Vec::new();
        for path in Self::split_path(provider_path)
            .into_iter()
            .chain(current.map(Self::split_path).unwrap_or_default())
        {
            if !values.iter().any(|existing| existing == &path) {
                values.push(path);
            }
        }
        Self::join_path(&values)
    }
}
