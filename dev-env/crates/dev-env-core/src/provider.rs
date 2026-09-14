use dev_env_model::ProviderConfig;
use dev_env_provider::{ProviderRunResult, ProviderRunner, ProviderRuntimeError};

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
