use crate::context::RuntimeContext;
use crate::error::CoreError;
use dev_env_model::{EnvValue, MaterializedEnv, Origin, ResolvedConfig, Sensitivity};
use std::collections::BTreeMap;
use std::ffi::OsStr;

pub(crate) fn initial_environment(
    config: &ResolvedConfig,
    context: &RuntimeContext,
) -> Result<MaterializedEnv, CoreError> {
    let mut values = BTreeMap::new();
    if config.environment.inherit_process {
        for (name, value) in &context.process_environment {
            values.insert(
                name.clone(),
                checked_value(
                    name,
                    value.clone(),
                    Some(Origin::environment(name.clone())),
                    Sensitivity::Public,
                    None,
                )?,
            );
        }
    }

    for (name, value) in &config.environment.variables {
        let use_configured = config.environment.configured_value_precedence
            == dev_env_model::ConfiguredValuePrecedence::Locked
            || !values.contains_key(name);
        if use_configured {
            let (origin, sensitivity) =
                provenance(config, &format!("environment.variables.{name}"));
            values.insert(
                name.clone(),
                checked_value(name, value.clone(), origin, sensitivity, None)?,
            );
        }
    }

    apply_configured_path(config, &mut values)?;
    let environment = MaterializedEnv::new(values, [0; 32]);
    environment.validate().map_err(CoreError::Model)?;
    Ok(environment)
}

pub(crate) fn configured_provenance(
    config: &ResolvedConfig,
    path: &str,
) -> (Option<dev_env_model::Origin>, Sensitivity) {
    provenance(config, path)
}

pub(crate) fn apply_provider_delta(
    environment: &mut MaterializedEnv,
    delta: &dev_env_provider::EnvironmentDelta,
    provider: &str,
    sensitivity: Sensitivity,
    path_mode: dev_env_model::PathMode,
) -> Result<(), CoreError> {
    for name in &delta.unset {
        environment.values.remove(name);
    }
    for (name, value) in &delta.set {
        if name == "PATH" {
            let value = match path_mode {
                dev_env_model::PathMode::Replace => value.clone(),
                dev_env_model::PathMode::Merge => merge_provider_path(
                    environment.get("PATH").map(|item| item.value.as_str()),
                    value,
                )?,
            };
            environment.values.insert(
                name.clone(),
                EnvValue {
                    value,
                    origin: None,
                    sensitivity,
                    provider: Some(provider.to_owned()),
                },
            );
        } else {
            environment.values.insert(
                name.clone(),
                EnvValue {
                    value: value.clone(),
                    origin: None,
                    sensitivity,
                    provider: Some(provider.to_owned()),
                },
            );
        }
    }
    environment.validate().map_err(CoreError::Model)
}

fn apply_configured_path(
    config: &ResolvedConfig,
    values: &mut BTreeMap<String, EnvValue>,
) -> Result<(), CoreError> {
    let path = &config.environment.path;
    if path.prepend.is_empty() && path.append.is_empty() && path.remove.is_empty() {
        return Ok(());
    }
    let inherited = values
        .get("PATH")
        .map(|value| split_path(&value.value))
        .unwrap_or_default();
    let resolved = path.resolve(&inherited).map_err(CoreError::Model)?;
    let path_value = join_path(&resolved)?;
    let (origin, sensitivity) = provenance(config, "environment.path.prepend");
    values.insert(
        "PATH".to_owned(),
        EnvValue {
            value: path_value,
            origin,
            sensitivity,
            provider: None,
        },
    );
    Ok(())
}

fn provenance(config: &ResolvedConfig, path: &str) -> (Option<Origin>, Sensitivity) {
    if let Some(entry) = config.provenance.get(path) {
        return (entry.origins.last().cloned(), entry.sensitivity);
    }
    if path == "environment.path.prepend" {
        for fallback in [
            "environment.path.append",
            "environment.path.remove",
            "environment.variables.PATH",
        ] {
            if let Some(entry) = config.provenance.get(fallback) {
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
    let result = EnvValue {
        value,
        origin,
        sensitivity,
        provider,
    };
    result.validate(name).map_err(CoreError::Model)?;
    Ok(result)
}

fn split_path(value: &str) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    std::env::split_paths(OsStr::new(value))
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

fn join_path(values: &[String]) -> Result<String, CoreError> {
    let joined =
        std::env::join_paths(values.iter().map(OsStr::new)).map_err(CoreError::PathJoin)?;
    joined.to_str().map(str::to_owned).ok_or_else(|| {
        CoreError::Model(dev_env_model::ModelError::InvalidEnvironmentValue {
            location: "environment.variables.PATH".to_owned(),
            reason: dev_env_model::ModelErrorReason::InvalidPath,
        })
    })
}

fn merge_provider_path(current: Option<&str>, provider_path: &str) -> Result<String, CoreError> {
    let mut values = Vec::new();
    for path in split_path(provider_path)
        .into_iter()
        .chain(current.map(split_path).unwrap_or_default())
    {
        if !values.iter().any(|existing| existing == &path) {
            values.push(path);
        }
    }
    join_path(&values)
}
