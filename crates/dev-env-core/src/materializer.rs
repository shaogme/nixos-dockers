use crate::{
    config_tree::ConfigTreeError,
    context::{ContextError, RuntimeContext},
    environment::Environment,
    error::{CoreError, FingerprintError},
};
use dev_env_model::{
    Condition, EffectiveIdentity, EnvValue, Generation, MaterializedEnv, ModelError,
    ProviderConfig, ResolvedConfig, ValueTree,
};
use dev_env_provider::{
    evaluate_condition, ProcessExecutor, ProviderContext, ProviderDiagnostic, ProviderJob,
    ProviderJobParts, ProviderRunResult, ProviderRunner, ProviderSupervisor,
};
use serde::Serialize;
use std::collections::BTreeMap;

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

/// The result of one environment materialization, including non-fatal
/// provider diagnostics that callers may expose through `doctor` or logs.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Materialization {
    environment: MaterializedEnv,
    diagnostics: Vec<MaterializationDiagnostic>,
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

/// Converts one validated [`ResolvedConfig`] into a process environment.
pub struct Materializer {
    config: ResolvedConfig,
    provider_supervisor: Box<dyn ProviderSupervisor>,
}

/// Request metadata owned by the backend and attached to every provider job.
/// Keeping it out of [`RuntimeContext`] prevents callers from smuggling an
/// identity or generation through the environment DSL.
#[derive(Clone, Debug)]
pub struct MaterializationMetadata {
    request_id: String,
    generation: Generation,
    identity: EffectiveIdentity,
}

impl MaterializationMetadata {
    pub fn new(
        request_id: impl Into<String>,
        generation: Generation,
        identity: EffectiveIdentity,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            generation,
            identity,
        }
    }

    fn legacy() -> Self {
        Self::new(
            "legacy-materialize",
            Generation::new(0),
            EffectiveIdentity::root(),
        )
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn generation(&self) -> Generation {
        self.generation
    }

    pub fn identity(&self) -> &EffectiveIdentity {
        &self.identity
    }
}

struct ConditionalVariables<'a> {
    config: &'a ResolvedConfig,
    context: &'a RuntimeContext,
    shell: &'a str,
    config_value: ValueTree,
    fingerprint: [u8; 32],
    environment: &'a mut MaterializedEnv,
}

struct ProviderContextInput<'a> {
    context: &'a RuntimeContext,
    shell: &'a str,
    environment: &'a MaterializedEnv,
    config: ValueTree,
    fingerprint: [u8; 32],
    provider_enabled: bool,
}

impl Materializer {
    pub fn try_new_with_process_executor(
        config: ResolvedConfig,
        executor: ProcessExecutor,
    ) -> Result<Self, CoreError> {
        config.validate().map_err(CoreError::Model)?;
        Ok(Self::with_provider_runner(
            config,
            ProviderRunner::with_process_executor(executor),
        ))
    }

    pub fn new(config: ResolvedConfig) -> Self {
        Self::with_provider_runner(config, ProviderRunner::default())
    }

    pub fn try_new(config: ResolvedConfig) -> Result<Self, CoreError> {
        config.validate().map_err(CoreError::Model)?;
        Ok(Self::new(config))
    }

    pub fn with_provider_supervisor<S>(config: ResolvedConfig, provider_supervisor: S) -> Self
    where
        S: ProviderSupervisor + 'static,
    {
        Self {
            config,
            provider_supervisor: Box::new(provider_supervisor),
        }
    }

    pub fn with_provider_runner(config: ResolvedConfig, provider_runner: ProviderRunner) -> Self {
        Self::with_provider_supervisor(config, provider_runner)
    }

    pub fn config(&self) -> &ResolvedConfig {
        &self.config
    }

    pub fn materialize(&self, context: &RuntimeContext) -> Result<Materialization, CoreError> {
        self.materialize_with_metadata(context, &MaterializationMetadata::legacy())
    }

    pub fn materialize_with_metadata(
        &self,
        context: &RuntimeContext,
        metadata: &MaterializationMetadata,
    ) -> Result<Materialization, CoreError> {
        self.config.validate().map_err(CoreError::Model)?;
        context.validate().map_err(CoreError::Context)?;
        let shell = select_shell(&self.config, context)?;
        let config_value = ConfigTreeError::to_value_tree(&self.config)?;
        let fingerprint = FingerprintError::config_fingerprint(&self.config)?;
        let mut environment = Environment::initial(&self.config, context)?;
        environment.set_config_fingerprint(fingerprint);
        apply_conditional_variables(ConditionalVariables {
            config: &self.config,
            context,
            shell: &shell,
            config_value: config_value.clone(),
            fingerprint,
            environment: &mut environment,
        })?;
        let mut diagnostics = Vec::new();

        for provider_id in self.config.provider_order().map_err(CoreError::Model)? {
            let provider_config = self
                .config
                .providers()
                .get(&provider_id)
                .expect("provider_order only returns configured providers");
            if !provider_is_enabled(self.config.features(), &provider_id) {
                diagnostics.push(MaterializationDiagnostic::ProviderDisabled {
                    provider: provider_id,
                });
                continue;
            }

            let provider_context = provider_context(ProviderContextInput {
                context,
                shell: &shell,
                environment: &environment,
                config: config_value.clone(),
                fingerprint,
                provider_enabled: provider_is_enabled(self.config.features(), &provider_id),
            });
            let job = ProviderJob::new(ProviderJobParts {
                job_id: format!("{}:{provider_id}", metadata.request_id),
                request_id: metadata.request_id.clone(),
                generation: metadata.generation,
                provider_id: provider_id.clone(),
                config: provider_config.clone(),
                context: provider_context,
                identity: metadata.identity.clone(),
            });
            let result =
                self.provider_supervisor
                    .prepare(&job)
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

fn apply_conditional_variables(input: ConditionalVariables<'_>) -> Result<(), CoreError> {
    for (name, variable) in input.config.environment().conditional_variables() {
        let condition = Condition::parse(variable.when()).map_err(|reason| {
            CoreError::Model(ModelError::InvalidCondition {
                location: format!("environment.conditional_variables.{name}.when"),
                reason,
            })
        })?;
        let enabled = {
            let condition_context = provider_context(ProviderContextInput {
                context: input.context,
                shell: input.shell,
                environment: input.environment,
                config: input.config_value.clone(),
                fingerprint: input.fingerprint,
                provider_enabled: true,
            });
            evaluate_condition(&condition, &condition_context)
        };
        if enabled {
            let path = format!("environment.conditional_variables.{name}");
            let (origin, sensitivity) = Environment::configured_provenance(input.config, &path);
            input.environment.insert(
                name.clone(),
                EnvValue::new(variable.value().to_owned(), origin, sensitivity, None),
            );
        } else {
            input.environment.remove(name);
        }
    }
    Ok(())
}

fn select_shell(config: &ResolvedConfig, context: &RuntimeContext) -> Result<String, CoreError> {
    let shell = context.shell().unwrap_or(config.shell().default());
    if config.shells().contains_key(shell) {
        Ok(shell.to_owned())
    } else {
        Err(CoreError::Context(ContextError::UnknownShell {
            shell: shell.to_owned(),
        }))
    }
}

fn provider_context(input: ProviderContextInput<'_>) -> ProviderContext {
    ProviderContext::new(
        input.context.workspace().to_path_buf(),
        input.context.cwd().to_path_buf(),
        input.shell,
        environment_map(input.environment),
    )
    .with_config(input.config)
    .with_workspace_writable(input.context.writable())
    .with_workspace_config_present(input.context.workspace_config_present())
    .with_user_id(input.context.user_id())
    .with_config_fingerprint(input.fingerprint)
    .with_provider_enabled(input.provider_enabled)
}

fn environment_map(environment: &MaterializedEnv) -> BTreeMap<String, String> {
    environment
        .values()
        .iter()
        .map(|(name, value)| (name.clone(), value.value().to_owned()))
        .collect()
}

fn apply_result(
    environment: &mut MaterializedEnv,
    diagnostics: &mut Vec<MaterializationDiagnostic>,
    provider_id: &str,
    provider_config: &ProviderConfig,
    result: ProviderRunResult,
) -> Result<(), CoreError> {
    let path_mode = provider_config
        .shellenv()
        .as_ref()
        .map(|shellenv| shellenv.path_mode())
        .unwrap_or_default();
    Environment::apply_provider_delta(
        environment,
        result.environment(),
        provider_id,
        result.sensitivity(),
        path_mode,
    )?;
    for diagnostic in result.diagnostics() {
        diagnostics.push(MaterializationDiagnostic::Provider {
            provider: provider_id.to_owned(),
            diagnostic: diagnostic.clone(),
        });
    }
    if let Some(receipt) = result.receipt() {
        environment.push_provider_receipt(receipt.clone());
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
