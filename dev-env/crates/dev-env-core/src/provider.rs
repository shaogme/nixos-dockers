use dev_env_model::ProviderConfig;
use dev_env_provider::{ProviderJob, ProviderRunResult, ProviderRunner, ProviderRuntimeError};

/// Provider lifecycle boundary used by [`crate::Materializer`].
///
/// The default implementation uses the generic command provider runtime. A
/// test or a future external-provider adapter can implement this trait without
/// changing environment construction or error handling in the core crate.
pub trait ProviderRuntime: Send + Sync {
    fn run(
        &self,
        provider_id: &str,
        config: &ProviderConfig,
        context: &dev_env_provider::ProviderContext,
    ) -> Result<ProviderRunResult, ProviderRuntimeError>;
}

impl ProviderRuntime for ProviderRunner {
    fn run(
        &self,
        provider_id: &str,
        config: &ProviderConfig,
        context: &dev_env_provider::ProviderContext,
    ) -> Result<ProviderRunResult, ProviderRuntimeError> {
        ProviderRunner::run(self, provider_id, config, context)
    }
}

/// Adapter retained for callers that still provide the pre-backend runtime
/// callback. New backend code should implement dev_env_provider's
/// ProviderSupervisor directly.
pub struct LegacyProviderSupervisor<R> {
    runtime: R,
}

impl<R> LegacyProviderSupervisor<R> {
    pub fn new(runtime: R) -> Self {
        Self { runtime }
    }
}

impl<R> dev_env_provider::ProviderSupervisor for LegacyProviderSupervisor<R>
where
    R: ProviderRuntime + 'static,
{
    fn prepare(&self, job: &ProviderJob) -> Result<ProviderRunResult, ProviderRuntimeError> {
        self.runtime
            .run(&job.provider_id, &job.config, &job.context)
    }
}
