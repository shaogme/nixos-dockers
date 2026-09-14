use crate::config_tree::to_value_tree;
use crate::context::{ContextError, RuntimeContext};
use crate::environment::{apply_provider_delta, configured_provenance, initial_environment};
use crate::error::CoreError;
use crate::fingerprint::config_fingerprint;
use crate::provider::ProviderRuntime;
use dev_env_model::{Condition, EnvValue, MaterializedEnv, ModelError, ResolvedConfig, ValueTree};
use dev_env_provider::{ProviderContext, ProviderDiagnostic, ProviderRunResult};
use serde::Serialize;
use std::collections::BTreeMap;

/// The result of one environment materialization, including non-fatal
/// provider diagnostics that callers may expose through `doctor` or logs.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Materialization {
    pub environment: MaterializedEnv,
    pub diagnostics: Vec<MaterializationDiagnostic>,
}

impl Materialization {
    pub fn environment(&self) -> &MaterializedEnv {
        &self.environment
    }

    pub fn diagnostics(&self) -> &[MaterializationDiagnostic] {
        &self.diagnostics
    }

    pub fn into_environment(self) -> MaterializedEnv {
        self.environment
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MaterializationDiagnostic {
    Provider {
        provider: String,
        diagnostic: ProviderDiagnostic,
    },
    ProviderDisabled {
        provider: String,
    },
}

/// Converts one validated [`ResolvedConfig`] into a process environment.
pub struct Materializer {
    config: ResolvedConfig,
    provider_runtime: Box<dyn ProviderRuntime>,
}

impl Materializer {
    pub fn new(config: ResolvedConfig) -> Self {
        Self::with_provider_runtime(config, dev_env_provider::ProviderRunner::default())
    }

    pub fn try_new(config: ResolvedConfig) -> Result<Self, CoreError> {
        config.validate().map_err(CoreError::Model)?;
        Ok(Self::new(config))
    }

    pub fn with_provider_runtime<R>(config: ResolvedConfig, provider_runtime: R) -> Self
    where
        R: ProviderRuntime + 'static,
    {
        Self {
            config,
            provider_runtime: Box::new(provider_runtime),
        }
    }

    pub fn with_provider_runner(
        config: ResolvedConfig,
        provider_runner: dev_env_provider::ProviderRunner,
    ) -> Self {
        Self::with_provider_runtime(config, provider_runner)
    }

    pub fn config(&self) -> &ResolvedConfig {
        &self.config
    }

    pub fn materialize(&self, context: &RuntimeContext) -> Result<Materialization, CoreError> {
        self.config.validate().map_err(CoreError::Model)?;
        context.validate().map_err(CoreError::Context)?;
        let shell = select_shell(&self.config, context)?;
        let config_value = to_value_tree(&self.config)?;
        let fingerprint = config_fingerprint(&self.config)?;
        let mut environment = initial_environment(&self.config, context)?;
        environment.config_fingerprint = fingerprint;
        apply_conditional_variables(
            &self.config,
            context,
            &shell,
            config_value.clone(),
            fingerprint,
            &mut environment,
        )?;
        let mut diagnostics = Vec::new();

        for provider_id in self.config.provider_order().map_err(CoreError::Model)? {
            let provider_config = self
                .config
                .providers
                .get(&provider_id)
                .expect("provider_order only returns configured providers");
            if !provider_is_enabled(&self.config.features, &provider_id) {
                diagnostics.push(MaterializationDiagnostic::ProviderDisabled {
                    provider: provider_id,
                });
                continue;
            }

            let provider_context = provider_context(
                context,
                &shell,
                &environment,
                config_value.clone(),
                fingerprint,
                provider_is_enabled(&self.config.features, &provider_id),
            );
            let result = self
                .provider_runtime
                .run(&provider_id, provider_config, &provider_context)
                .map_err(|source| CoreError::Provider {
                    provider: provider_id.clone(),
                    source: Box::new(source),
                })?;
            apply_result(
                &mut environment,
                &mut diagnostics,
                &provider_id,
                provider_config,
                result,
            )?;
        }

        environment.validate().map_err(CoreError::Model)?;
        Ok(Materialization {
            environment,
            diagnostics,
        })
    }

    pub fn materialize_environment(
        &self,
        context: &RuntimeContext,
    ) -> Result<MaterializedEnv, CoreError> {
        self.materialize(context)
            .map(Materialization::into_environment)
    }
}

fn apply_conditional_variables(
    config: &ResolvedConfig,
    context: &RuntimeContext,
    shell: &str,
    config_value: ValueTree,
    fingerprint: [u8; 32],
    environment: &mut MaterializedEnv,
) -> Result<(), CoreError> {
    for (name, variable) in &config.environment.conditional_variables {
        let condition = Condition::parse(&variable.when).map_err(|reason| {
            CoreError::Model(ModelError::InvalidCondition {
                location: format!("environment.conditional_variables.{name}.when"),
                reason,
            })
        })?;
        let enabled = {
            let condition_context = provider_context(
                context,
                shell,
                environment,
                config_value.clone(),
                fingerprint,
                true,
            );
            dev_env_provider::evaluate_condition(&condition, &condition_context)
        };
        if enabled {
            let path = format!("environment.conditional_variables.{name}");
            let (origin, sensitivity) = configured_provenance(config, &path);
            environment.values.insert(
                name.clone(),
                EnvValue {
                    value: variable.value.clone(),
                    origin,
                    sensitivity,
                    provider: None,
                },
            );
        } else {
            environment.values.remove(name);
        }
    }
    Ok(())
}

fn select_shell(config: &ResolvedConfig, context: &RuntimeContext) -> Result<String, CoreError> {
    let shell = context.shell.as_deref().unwrap_or(&config.shell.default);
    if config.shells.contains_key(shell) {
        Ok(shell.to_owned())
    } else {
        Err(CoreError::Context(ContextError::UnknownShell {
            shell: shell.to_owned(),
        }))
    }
}

fn provider_context(
    context: &RuntimeContext,
    shell: &str,
    environment: &MaterializedEnv,
    config: ValueTree,
    fingerprint: [u8; 32],
    provider_enabled: bool,
) -> ProviderContext {
    ProviderContext::new(
        context.workspace.clone(),
        context.cwd.clone(),
        shell,
        environment_map(environment),
    )
    .with_config(config)
    .with_workspace_writable(context.writable())
    .with_workspace_config_present(context.workspace_config_present)
    .with_user_id(context.user_id)
    .with_config_fingerprint(fingerprint)
    .with_provider_enabled(provider_enabled)
}

fn environment_map(environment: &MaterializedEnv) -> BTreeMap<String, String> {
    environment
        .values
        .iter()
        .map(|(name, value)| (name.clone(), value.value.clone()))
        .collect()
}

fn apply_result(
    environment: &mut MaterializedEnv,
    diagnostics: &mut Vec<MaterializationDiagnostic>,
    provider_id: &str,
    provider_config: &dev_env_model::ProviderConfig,
    result: ProviderRunResult,
) -> Result<(), CoreError> {
    let path_mode = provider_config
        .shellenv
        .as_ref()
        .map(|shellenv| shellenv.path_mode)
        .unwrap_or_default();
    apply_provider_delta(
        environment,
        &result.environment,
        provider_id,
        result.sensitivity,
        path_mode,
    )?;
    for diagnostic in result.diagnostics {
        diagnostics.push(MaterializationDiagnostic::Provider {
            provider: provider_id.to_owned(),
            diagnostic,
        });
    }
    if let Some(receipt) = result.receipt {
        environment.provider_receipts.push(receipt);
    }
    Ok(())
}

fn provider_is_enabled(features: &ValueTree, provider: &str) -> bool {
    let ValueTree::Map(features) = features else {
        return true;
    };
    let Some(ValueTree::Map(provider_features)) = features.get(provider) else {
        return true;
    };
    match provider_features.get("enabled") {
        Some(ValueTree::Bool(enabled)) => *enabled,
        _ => true,
    }
}
