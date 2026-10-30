use crate::{
    CoreError, Materialization, MaterializationDiagnostic, MaterializationMetadata, Materializer,
    RuntimeContext,
};
use dev_env_model::{MaterializationKey, MaterializedEnv, Sensitivity};
use std::{
    collections::HashMap,
    error::Error,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

/// The result returned by the backend-owned materialization service.
#[derive(Clone, Debug)]
pub struct MaterializationServiceResult {
    materialization: Arc<Materialization>,
    cache_hit: bool,
}

impl MaterializationServiceResult {
    pub fn materialization(&self) -> &Materialization {
        &self.materialization
    }

    pub fn environment(&self) -> &MaterializedEnv {
        self.materialization.environment()
    }

    pub fn diagnostics(&self) -> &[MaterializationDiagnostic] {
        self.materialization.diagnostics()
    }

    pub fn cache_hit(&self) -> bool {
        self.cache_hit
    }
}

/// Errors returned by the service.  The owner of a single-flight operation
/// receives the original structured [`CoreError`]. Waiters receive the same
/// failure message after the operation has completed; failed results are never
/// inserted into the cache.
#[derive(Debug)]
pub enum MaterializationServiceError {
    Core(CoreError),
    SharedFailure { code: String, message: String },
}

impl fmt::Display for MaterializationServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core(source) => source.fmt(formatter),
            Self::SharedFailure { message, .. } => formatter.write_str(message),
        }
    }
}

impl Error for MaterializationServiceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Core(source) => Some(source),
            Self::SharedFailure { .. } => None,
        }
    }
}

impl MaterializationServiceError {
    pub fn code(&self) -> &str {
        match self {
            Self::Core(source) => source.code(),
            Self::SharedFailure { code, .. } => code,
        }
    }
}

struct CacheEntry {
    value: Arc<Materialization>,
    last_used: Instant,
    sensitive_bytes: usize,
}

struct ServiceState {
    cache: HashMap<MaterializationKey, CacheEntry>,
    sensitive_bytes: usize,
}

/// Owns a materializer for one immutable backend snapshot. The materializer
/// is constructed once, successful values are retained in memory, and
/// concurrent requests for the same key share one provider execution.
pub struct MaterializationService {
    materializer: Arc<Materializer>,
    state: ServiceState,
    max_entries: usize,
    max_sensitive_bytes: usize,
    ttl: Option<Duration>,
}

impl MaterializationService {
    pub fn new(materializer: Materializer) -> Self {
        Self {
            materializer: Arc::new(materializer),
            state: ServiceState {
                cache: HashMap::new(),
                sensitive_bytes: 0,
            },
            max_entries: 128,
            max_sensitive_bytes: 8 * 1024 * 1024,
            ttl: Some(Duration::from_secs(900)),
        }
    }

    pub fn with_limits(mut self, max_entries: usize, ttl: Option<Duration>) -> Self {
        self.max_entries = max_entries.max(1);
        self.ttl = ttl;
        self
    }

    pub fn with_sensitive_byte_limit(mut self, max_sensitive_bytes: usize) -> Self {
        self.max_sensitive_bytes = max_sensitive_bytes;
        self
    }

    pub fn materializer(&self) -> &Materializer {
        &self.materializer
    }

    pub fn shared_materializer(&self) -> Arc<Materializer> {
        Arc::clone(&self.materializer)
    }

    pub fn clear(&mut self) {
        self.state.cache.clear();
        self.state.sensitive_bytes = 0;
    }

    pub fn len(&self) -> usize {
        self.state.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.state.cache.is_empty()
    }

    pub fn cached(&mut self, key: &MaterializationKey) -> Option<MaterializationServiceResult> {
        self.remove_expired();
        self.state.cache.get_mut(key).map(|entry| {
            entry.last_used = Instant::now();
            MaterializationServiceResult {
                materialization: Arc::clone(&entry.value),
                cache_hit: true,
            }
        })
    }

    pub fn cache_success(
        &mut self,
        key: MaterializationKey,
        materialization: Arc<Materialization>,
    ) {
        let sensitive_bytes = sensitive_bytes(&materialization);
        if sensitive_bytes > self.max_sensitive_bytes {
            return;
        }
        if let Some(previous) = self.state.cache.insert(
            key,
            CacheEntry {
                value: materialization,
                last_used: Instant::now(),
                sensitive_bytes,
            },
        ) {
            self.state.sensitive_bytes = self
                .state
                .sensitive_bytes
                .saturating_sub(previous.sensitive_bytes);
        }
        self.state.sensitive_bytes = self.state.sensitive_bytes.saturating_add(sensitive_bytes);
        self.evict_to_limits();
    }

    pub fn prepare(
        &mut self,
        key: MaterializationKey,
        context: &RuntimeContext,
        metadata: &MaterializationMetadata,
    ) -> Result<MaterializationServiceResult, MaterializationServiceError> {
        key.validate()
            .map_err(|error| MaterializationServiceError::SharedFailure {
                code: "DEVENV-E-MODEL".to_owned(),
                message: error.to_string(),
            })?;
        if let Some(result) = self.cached(&key) {
            return Ok(result);
        }
        let materialization = Arc::new(
            self.materializer
                .materialize_with_metadata(context, metadata)
                .map_err(MaterializationServiceError::Core)?,
        );
        self.cache_success(key, Arc::clone(&materialization));
        Ok(MaterializationServiceResult {
            materialization,
            cache_hit: false,
        })
    }

    fn remove_expired(&mut self) {
        let Some(ttl) = self.ttl else {
            return;
        };
        let now = Instant::now();
        self.state
            .cache
            .retain(|_, entry| now.duration_since(entry.last_used) <= ttl);
        self.state.sensitive_bytes = self
            .state
            .cache
            .values()
            .map(|entry| entry.sensitive_bytes)
            .sum();
    }

    fn evict_to_limits(&mut self) {
        while self.state.cache.len() > self.max_entries
            || self.state.sensitive_bytes > self.max_sensitive_bytes
        {
            let Some(oldest) = self
                .state
                .cache
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(entry) = self.state.cache.remove(&oldest) {
                self.state.sensitive_bytes = self
                    .state
                    .sensitive_bytes
                    .saturating_sub(entry.sensitive_bytes);
            }
        }
    }
}

fn sensitive_bytes(materialization: &Materialization) -> usize {
    materialization
        .environment()
        .values()
        .iter()
        .filter(|(_, value)| value.sensitivity() != Sensitivity::Public)
        .map(|(name, value)| name.len().saturating_add(value.value().len()))
        .sum()
}
