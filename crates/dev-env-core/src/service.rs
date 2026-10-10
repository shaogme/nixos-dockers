use crate::{CoreError, Materialization, MaterializationMetadata, Materializer, RuntimeContext};
use dev_env_model::MaterializationKey;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The result returned by the backend-owned materialization service.
#[derive(Clone, Debug)]
pub struct MaterializationServiceResult {
    pub materialization: Arc<Materialization>,
    pub cache_hit: bool,
}

impl MaterializationServiceResult {
    pub fn environment(&self) -> &dev_env_model::MaterializedEnv {
        self.materialization.environment()
    }

    pub fn diagnostics(&self) -> &[crate::MaterializationDiagnostic] {
        self.materialization.diagnostics()
    }
}

/// Errors returned by the service.  The owner of a single-flight operation
/// receives the original structured [`CoreError`]. Waiters receive the same
/// failure message after the operation has completed; failed results are never
/// inserted into the cache.
#[derive(Debug)]
pub enum MaterializationServiceError {
    Core(CoreError),
    SharedFailure(String),
}

impl std::fmt::Display for MaterializationServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Core(source) => source.fmt(formatter),
            Self::SharedFailure(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for MaterializationServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Core(source) => Some(source),
            Self::SharedFailure(_) => None,
        }
    }
}

struct CacheEntry {
    value: Arc<Materialization>,
    last_used: Instant,
    sensitive_bytes: usize,
}

struct Flight {
    result: Mutex<Option<Result<Arc<Materialization>, String>>>,
    ready: Condvar,
}

struct ServiceState {
    cache: HashMap<MaterializationKey, CacheEntry>,
    in_flight: HashMap<MaterializationKey, Arc<Flight>>,
    sensitive_bytes: usize,
}

/// Owns a materializer for one immutable backend snapshot. The materializer
/// is constructed once, successful values are retained in memory, and
/// concurrent requests for the same key share one provider execution.
pub struct MaterializationService {
    materializer: Materializer,
    state: Mutex<ServiceState>,
    max_entries: usize,
    max_sensitive_bytes: usize,
    ttl: Option<Duration>,
}

impl MaterializationService {
    pub fn new(materializer: Materializer) -> Self {
        Self {
            materializer,
            state: Mutex::new(ServiceState {
                cache: HashMap::new(),
                in_flight: HashMap::new(),
                sensitive_bytes: 0,
            }),
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

    pub fn clear(&self) {
        let mut state = self
            .state
            .lock()
            .expect("materialization cache lock poisoned");
        state.cache.clear();
        state.sensitive_bytes = 0;
    }

    pub fn len(&self) -> usize {
        self.state
            .lock()
            .expect("materialization cache lock poisoned")
            .cache
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn prepare(
        &self,
        key: MaterializationKey,
        context: &RuntimeContext,
        metadata: &MaterializationMetadata,
    ) -> Result<MaterializationServiceResult, MaterializationServiceError> {
        key.validate()
            .map_err(|error| MaterializationServiceError::SharedFailure(error.to_string()))?;

        let (flight, owner) = {
            let mut state = self
                .state
                .lock()
                .expect("materialization cache lock poisoned");
            let now = Instant::now();
            let expired = self.ttl.is_some_and(|ttl| {
                state
                    .cache
                    .values()
                    .any(|entry| now.duration_since(entry.last_used) > ttl)
            });
            if expired {
                if let Some(ttl) = self.ttl {
                    state
                        .cache
                        .retain(|_, entry| now.duration_since(entry.last_used) <= ttl);
                    state.sensitive_bytes = state
                        .cache
                        .values()
                        .map(|entry| entry.sensitive_bytes)
                        .sum();
                }
            }
            if let Some(entry) = state.cache.get_mut(&key) {
                entry.last_used = now;
                return Ok(MaterializationServiceResult {
                    materialization: Arc::clone(&entry.value),
                    cache_hit: true,
                });
            }
            if let Some(flight) = state.in_flight.get(&key) {
                (Arc::clone(flight), false)
            } else {
                let flight = Arc::new(Flight {
                    result: Mutex::new(None),
                    ready: Condvar::new(),
                });
                state.in_flight.insert(key.clone(), Arc::clone(&flight));
                (flight, true)
            }
        };

        if !owner {
            let mut result = flight.result.lock().expect("single-flight lock poisoned");
            while result.is_none() {
                result = flight
                    .ready
                    .wait(result)
                    .expect("single-flight lock poisoned");
            }
            return match result.as_ref().expect("single-flight result missing") {
                Ok(value) => Ok(MaterializationServiceResult {
                    materialization: Arc::clone(value),
                    cache_hit: true,
                }),
                Err(message) => Err(MaterializationServiceError::SharedFailure(message.clone())),
            };
        }

        let result = self
            .materializer
            .materialize_with_metadata(context, metadata)
            .map(Arc::new);
        let shared = result.as_ref().map(Arc::clone).map_err(ToString::to_string);
        {
            let mut state = self
                .state
                .lock()
                .expect("materialization cache lock poisoned");
            if let Ok(value) = &result {
                let sensitive_bytes = sensitive_bytes(value);
                if sensitive_bytes <= self.max_sensitive_bytes {
                    if let Some(previous) = state.cache.insert(
                        key.clone(),
                        CacheEntry {
                            value: Arc::clone(value),
                            last_used: Instant::now(),
                            sensitive_bytes,
                        },
                    ) {
                        state.sensitive_bytes = state
                            .sensitive_bytes
                            .saturating_sub(previous.sensitive_bytes);
                    }
                    state.sensitive_bytes = state.sensitive_bytes.saturating_add(sensitive_bytes);
                }
                while state.cache.len() > self.max_entries
                    || state.sensitive_bytes > self.max_sensitive_bytes
                {
                    let Some(oldest) = state
                        .cache
                        .iter()
                        .min_by_key(|(_, entry)| entry.last_used)
                        .map(|(key, _)| key.clone())
                    else {
                        break;
                    };
                    if let Some(entry) = state.cache.remove(&oldest) {
                        state.sensitive_bytes =
                            state.sensitive_bytes.saturating_sub(entry.sensitive_bytes);
                    }
                }
            }
            // Publish the result while the flight is still registered. This
            // prevents a failed operation from having a gap where a second
            // caller starts a duplicate provider job before waiters wake up.
            *flight.result.lock().expect("single-flight lock poisoned") = Some(shared);
            state.in_flight.remove(&key);
        }
        flight.ready.notify_all();

        result
            .map(|materialization| MaterializationServiceResult {
                materialization,
                cache_hit: false,
            })
            .map_err(MaterializationServiceError::Core)
    }
}

fn sensitive_bytes(materialization: &Materialization) -> usize {
    materialization
        .environment()
        .values
        .iter()
        .filter(|(_, value)| value.sensitivity != dev_env_model::Sensitivity::Public)
        .map(|(name, value)| name.len().saturating_add(value.value.len()))
        .sum()
}
