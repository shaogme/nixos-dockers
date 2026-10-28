use crate::{ProviderContext, ProviderRunResult, ProviderRuntimeError};
use dev_env_model::{EffectiveIdentity, Generation, ProviderConfig};

/// Owned input for one provider operation. A backend creates a job once and
/// submits it to its supervisor; the supervisor becomes the sole execution
/// boundary for provider commands.
#[derive(Clone, Debug)]
pub struct ProviderJob {
    job_id: String,
    request_id: String,
    generation: Generation,
    provider_id: String,
    config: ProviderConfig,
    context: ProviderContext,
    identity: EffectiveIdentity,
}

#[derive(Clone, Debug)]
pub struct ProviderJobParts {
    pub job_id: String,
    pub request_id: String,
    pub generation: Generation,
    pub provider_id: String,
    pub config: ProviderConfig,
    pub context: ProviderContext,
    pub identity: EffectiveIdentity,
}

impl ProviderJob {
    pub fn new(parts: ProviderJobParts) -> Self {
        Self {
            job_id: parts.job_id,
            request_id: parts.request_id,
            generation: parts.generation,
            provider_id: parts.provider_id,
            config: parts.config,
            context: parts.context,
            identity: parts.identity,
        }
    }

    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn generation(&self) -> &Generation {
        &self.generation
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }

    pub fn context(&self) -> &ProviderContext {
        &self.context
    }

    pub fn identity(&self) -> &EffectiveIdentity {
        &self.identity
    }

    /// Build a job for the compatibility materializer path. Backend callers
    /// should use `new` so request identity and snapshot generation are real.
    pub fn legacy(
        provider_id: impl Into<String>,
        config: ProviderConfig,
        context: ProviderContext,
    ) -> Self {
        Self::new(ProviderJobParts {
            job_id: "legacy-materialize".to_owned(),
            request_id: "legacy-materialize".to_owned(),
            generation: Generation::new(0),
            provider_id: provider_id.into(),
            config,
            context,
            identity: EffectiveIdentity::root(),
        })
    }
}

/// Provider lifecycle boundary owned by the backend.
///
/// Implementations must own the complete provider process tree. In particular,
/// callers must not spawn a provider command and wait for it outside this
/// trait. The default runner uses the process-group/subreaper boundary in
/// [`crate::ProcessExecutor`] and keeps the wait/reap operation inside the
/// provider runtime.
pub trait ProviderSupervisor: Send + Sync {
    fn prepare(&self, job: &ProviderJob) -> Result<ProviderRunResult, ProviderRuntimeError>;

    fn run(&self, job: &ProviderJob) -> Result<ProviderRunResult, ProviderRuntimeError> {
        self.prepare(job)
    }
}
