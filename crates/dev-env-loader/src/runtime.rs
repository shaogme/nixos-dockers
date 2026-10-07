use crate::error::{LoaderError, TrustViolationReason};
use crate::merge::{parsed_input_value, MergedConfig};
use crate::source::SourceKind;
use dev_env_model::{ModelError, Origin, Sensitivity, ValueTree};
use std::collections::BTreeMap;

/// Apply only inputs explicitly declared as runtime inputs. Ambient variables
/// remain untouched here; the materializer decides which of them to inherit.
pub(crate) fn apply(
    environment: &BTreeMap<String, String>,
    merged: &mut MergedConfig,
) -> Result<(), LoaderError> {
    let config = merged.config_snapshot()?;
    let policy = config.policy.clone();

    for (name, input) in &config.inputs {
        if !input.runtime {
            continue;
        }
        let mut supplied: Option<(String, String)> = None;
        for candidate in [Some(name.as_str()), input.export_as.as_deref()] {
            let Some(candidate) = candidate else {
                continue;
            };
            if let Some(value) = environment.get(candidate) {
                if let Some((previous_name, previous_value)) = &supplied {
                    if previous_value != value {
                        return Err(LoaderError::RuntimeInputConflict {
                            input: name.clone(),
                            first_name: previous_name.clone(),
                            second_name: candidate.to_owned(),
                        });
                    }
                } else {
                    supplied = Some((candidate.to_owned(), value.clone()));
                }
            }
        }
        let Some((environment_name, raw_value)) = supplied else {
            continue;
        };
        let parsed = input.parse_runtime_value_named(&environment_name, &raw_value)?;
        merged.set_runtime_value(
            &input.target,
            parsed_input_value(parsed),
            Origin::environment(environment_name),
            input.sensitivity,
        )?;
    }

    for (name, raw_value) in environment {
        let Some(path) = name.strip_prefix("DEVENV_OVERRIDE__") else {
            continue;
        };
        let path = namespace_path(path)?;
        if !policy.allows_workspace_override(&path) {
            if policy.unknown_input == dev_env_model::UnknownInputPolicy::Ignore {
                continue;
            }
            return Err(LoaderError::TrustViolation {
                profile: "<runtime>".to_owned(),
                source: SourceKind::Runtime,
                path,
                reason: TrustViolationReason::WorkspaceOverrideNotAllowed,
            });
        }
        let value = match config.inputs.values().find(|input| input.target == path) {
            Some(input) => parsed_input_value(input.parse_runtime_value_named(name, raw_value)?),
            None => {
                if merged.value_at(&path).is_none() {
                    if policy.unknown_input == dev_env_model::UnknownInputPolicy::Ignore {
                        continue;
                    }
                    return Err(ModelError::UnknownInput { name: name.clone() }.into());
                }
                ValueTree::String(raw_value.clone())
            }
        };
        merged.set_runtime_value(
            &path,
            value,
            Origin::environment(name.clone()),
            Sensitivity::Public,
        )?;
    }
    Ok(())
}

fn namespace_path(raw: &str) -> Result<String, LoaderError> {
    let parts = raw.split("__").collect::<Vec<_>>();
    if parts.is_empty()
        || parts.iter().any(|part| {
            part.is_empty() || !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    {
        return Err(LoaderError::Model {
            location: Some(format!("DEVENV_OVERRIDE__{raw}")),
            source: ModelError::InvalidValue {
                location: "runtime override".to_owned(),
                reason: dev_env_model::ModelErrorReason::InvalidIdentifier,
            },
        });
    }
    Ok(parts
        .into_iter()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("."))
}
