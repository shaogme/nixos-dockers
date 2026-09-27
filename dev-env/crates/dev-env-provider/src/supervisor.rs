use crate::{ProviderContext, ProviderRunResult, ProviderRunner, ProviderRuntimeError};
use dev_env_model::{EffectiveIdentity, Generation, ProviderConfig};

/// Owned input for one provider operation. A backend creates a job once and
/// submits it to its supervisor; the supervisor becomes the sole execution
/// boundary for provider commands.
#[derive(Clone, Debug)]
pub struct ProviderJob {
    pub job_id: String,
    pub request_id: String,
    pub generation: Generation,
    pub provider_id: String,
    pub config: ProviderConfig,
    pub context: ProviderContext,
    pub identity: EffectiveIdentity,
}

impl ProviderJob {
    pub fn new(
        job_id: impl Into<String>,
        request_id: impl Into<String>,
        generation: Generation,
        provider_id: impl Into<String>,
        config: ProviderConfig,
        context: ProviderContext,
        identity: EffectiveIdentity,
    ) -> Self {
        Self {
            job_id: job_id.into(),
            request_id: request_id.into(),
            generation,
            provider_id: provider_id.into(),
            config,
            context,
            identity,
        }
    }

    /// Build a job for the compatibility materializer path. Backend callers
    /// should use `new` so request identity and snapshot generation are real.
    pub fn legacy(
        provider_id: impl Into<String>,
        config: ProviderConfig,
        context: ProviderContext,
    ) -> Self {
        Self::new(
            "legacy-materialize",
            "legacy-materialize",
            Generation::new(0),
            provider_id,
            config,
            context,
            EffectiveIdentity::root(),
        )
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

impl ProviderSupervisor for ProviderRunner {
    fn prepare(&self, job: &ProviderJob) -> Result<ProviderRunResult, ProviderRuntimeError> {
        self.run_with_identity(
            &job.provider_id,
            &job.config,
            &job.context,
            Some(&job.identity),
        )
    }
}
