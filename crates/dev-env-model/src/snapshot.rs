use crate::config::{ResolvedConfig, WorkspaceSearch};
use crate::identity::{EffectiveIdentity, IdentityError};
use crate::input::InputSpec;
use crate::provenance::ProvenanceIndex;
use crate::provider::{MissingProviderPolicy, ProviderConfig};
use crate::shell::ShellConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Monotonically increasing configuration generation owned by a backend.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct Generation(u64);

pub type SnapshotGeneration = Generation;

impl Generation {
    pub const INITIAL: Self = Self(1);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Result<Self, SnapshotError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(SnapshotError::GenerationExhausted)
    }
}

impl From<u64> for Generation {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<Generation> for u64 {
    fn from(value: Generation) -> Self {
        value.0
    }
}

/// How a provider's prepare operation is memoized by a backend generation.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum PreparePolicy {
    #[default]
    OncePerKey,
    Always,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderPolicy {
    #[serde(default)]
    pub prepare: PreparePolicy,
    #[serde(default = "default_true")]
    pub cache_shellenv: bool,
    #[serde(default)]
    pub missing: MissingProviderPolicy,
}

impl Default for ProviderPolicy {
    fn default() -> Self {
        Self {
            prepare: PreparePolicy::OncePerKey,
            cache_shellenv: true,
            missing: MissingProviderPolicy::Error,
        }
    }
}

impl ProviderPolicy {
    pub fn from_config(config: &ProviderConfig) -> Self {
        Self {
            prepare: PreparePolicy::OncePerKey,
            cache_shellenv: true,
            missing: config.missing,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableResolutionRules {
    /// Whether a provider executable may be resolved through PATH.
    #[serde(default = "default_true")]
    pub allow_path_search: bool,
    /// Explicit roots accepted for absolute provider executables.
    #[serde(default)]
    pub allowed_roots: Vec<PathBuf>,
    /// Workspace overlays never get to alter executable resolution.
    #[serde(default)]
    pub allow_workspace_override: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceOverlayPolicy {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub search: WorkspaceSearch,
    #[serde(default)]
    pub allowed_paths: Vec<String>,
}

impl Default for WorkspaceOverlayPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            search: WorkspaceSearch::Upward,
            allowed_paths: Vec::new(),
        }
    }
}

/// All configuration needed by a backend request, captured at startup or
/// reload. Runtime requests carry context only and cannot replace these
/// fields.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSnapshot {
    pub generation: Generation,
    pub config_fingerprint: [u8; 32],
    pub profile_chain: Vec<String>,
    pub resolved_config: ResolvedConfig,
    pub source_provenance: ProvenanceIndex,
    pub trust_fingerprint: [u8; 32],
    pub shells: BTreeMap<String, ShellConfig>,
    pub runtime_inputs: BTreeMap<String, InputSpec>,
    pub provider_order: Vec<String>,
    pub provider_policy: BTreeMap<String, ProviderPolicy>,
    pub executable_resolution: ExecutableResolutionRules,
    pub workspace_root: PathBuf,
    pub workspace_overlay: WorkspaceOverlayPolicy,
}

pub type ConfigSnapshotRef = Arc<ConfigSnapshot>;

impl ConfigSnapshot {
    pub fn new(
        generation: Generation,
        profile_chain: Vec<String>,
        resolved_config: ResolvedConfig,
        source_provenance: ProvenanceIndex,
        trust_fingerprint: [u8; 32],
        config_fingerprint: [u8; 32],
    ) -> Result<Self, SnapshotError> {
        resolved_config.validate().map_err(SnapshotError::Model)?;
        let provider_order = resolved_config
            .provider_order()
            .map_err(SnapshotError::Model)?;
        let provider_policy = resolved_config
            .providers
            .iter()
            .map(|(id, config)| (id.clone(), ProviderPolicy::from_config(config)))
            .collect();
        let snapshot = Self {
            generation,
            config_fingerprint,
            profile_chain,
            shells: resolved_config.shells.clone(),
            runtime_inputs: resolved_config.inputs.clone(),
            workspace_root: PathBuf::from(&resolved_config.workspace.root),
            resolved_config,
            source_provenance,
            trust_fingerprint,
            provider_order,
            provider_policy,
            executable_resolution: ExecutableResolutionRules::default(),
            workspace_overlay: WorkspaceOverlayPolicy::default(),
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn from_resolved_config(
        generation: Generation,
        profile_chain: Vec<String>,
        resolved_config: ResolvedConfig,
        source_provenance: ProvenanceIndex,
        trust_fingerprint: [u8; 32],
    ) -> Result<Self, SnapshotError> {
        let encoded = serde_json::to_vec(&resolved_config)
            .map_err(|source| SnapshotError::Fingerprint { source })?;
        let fingerprint = sha256(&encoded);
        Self::new(
            generation,
            profile_chain,
            resolved_config,
            source_provenance,
            trust_fingerprint,
            fingerprint,
        )
    }

    pub fn validate(&self) -> Result<(), SnapshotError> {
        self.resolved_config
            .validate()
            .map_err(SnapshotError::Model)?;
        if self.workspace_root.as_os_str().is_empty() || !self.workspace_root.is_absolute() {
            return Err(SnapshotError::InvalidWorkspaceRoot(
                self.workspace_root.clone(),
            ));
        }
        let expected_order = self
            .resolved_config
            .provider_order()
            .map_err(SnapshotError::Model)?;
        let expected_policies = self
            .resolved_config
            .providers
            .iter()
            .map(|(id, config)| (id.clone(), ProviderPolicy::from_config(config)))
            .collect::<BTreeMap<_, _>>();
        if self.provider_order != expected_order
            || self.shells != self.resolved_config.shells
            || self.runtime_inputs != self.resolved_config.inputs
            || self.provider_policy != expected_policies
        {
            return Err(SnapshotError::DerivedStateMismatch);
        }
        Ok(())
    }

    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }

    pub fn runtime_input_schema(&self) -> &BTreeMap<String, InputSpec> {
        &self.runtime_inputs
    }

    pub fn provider_dependency_order(&self) -> &[String] {
        &self.provider_order
    }

    pub fn workspace_overlay_policy(&self) -> &WorkspaceOverlayPolicy {
        &self.workspace_overlay
    }
}

/// The complete input to the materialization cache. The value is kept
/// structured for diagnostics and hashed canonically for cache metrics and
/// persistence-free lookups.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MaterializationKey {
    pub generation: Generation,
    pub config_fingerprint: [u8; 32],
    pub workspace: PathBuf,
    pub cwd: PathBuf,
    pub shell: String,
    pub identity: EffectiveIdentity,
    #[serde(default)]
    pub runtime_inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub ambient_environment: BTreeMap<String, String>,
    /// Maps provider id to the fingerprint of the files that affect detect.
    #[serde(default)]
    pub provider_detect_fingerprints: BTreeMap<String, [u8; 32]>,
}

impl MaterializationKey {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        generation: Generation,
        config_fingerprint: [u8; 32],
        workspace: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
        shell: impl Into<String>,
        identity: EffectiveIdentity,
        runtime_inputs: BTreeMap<String, String>,
        ambient_environment: BTreeMap<String, String>,
        provider_detect_fingerprints: BTreeMap<String, [u8; 32]>,
    ) -> Result<Self, MaterializationKeyError> {
        let workspace = normalize_absolute_path(&workspace.into());
        let cwd = normalize_absolute_path(&cwd.into());
        let key = Self {
            generation,
            config_fingerprint,
            workspace,
            cwd,
            shell: shell.into(),
            identity,
            runtime_inputs,
            ambient_environment,
            provider_detect_fingerprints,
        };
        key.validate()?;
        Ok(key)
    }

    pub fn validate(&self) -> Result<(), MaterializationKeyError> {
        validate_absolute("workspace", &self.workspace)?;
        validate_absolute("cwd", &self.cwd)?;
        if !self.cwd.starts_with(&self.workspace) {
            return Err(MaterializationKeyError::CwdOutsideWorkspace);
        }
        if self.shell.is_empty() || self.shell.contains('\0') {
            return Err(MaterializationKeyError::InvalidShell);
        }
        self.identity
            .validate()
            .map_err(MaterializationKeyError::Identity)?;
        validate_values(&self.runtime_inputs)?;
        validate_values(&self.ambient_environment)?;
        Ok(())
    }

    /// Return the stable SHA-256 digest used by backend cache implementations.
    pub fn fingerprint(&self) -> Result<[u8; 32], MaterializationKeyError> {
        self.validate()?;
        let encoded = serde_json::to_vec(self)
            .map_err(|source| MaterializationKeyError::Encoding { source })?;
        Ok(sha256(&encoded))
    }

    pub fn digest(&self) -> Result<[u8; 32], MaterializationKeyError> {
        self.fingerprint()
    }
}

#[derive(Debug)]
pub enum SnapshotError {
    Model(crate::ModelError),
    Fingerprint { source: serde_json::Error },
    GenerationExhausted,
    InvalidWorkspaceRoot(PathBuf),
    ProviderOrderMismatch,
    DerivedStateMismatch,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model(source) => write!(formatter, "snapshot configuration is invalid: {source}"),
            Self::Fingerprint { source } => {
                write!(formatter, "snapshot fingerprint failed: {source}")
            }
            Self::GenerationExhausted => formatter.write_str("snapshot generation exhausted"),
            Self::InvalidWorkspaceRoot(path) => {
                write!(
                    formatter,
                    "snapshot workspace root is not absolute: {}",
                    path.display()
                )
            }
            Self::ProviderOrderMismatch => {
                formatter.write_str("snapshot provider order is inconsistent")
            }
            Self::DerivedStateMismatch => {
                formatter.write_str("snapshot derived configuration is inconsistent")
            }
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Model(source) => Some(source),
            Self::Fingerprint { source } => Some(source),
            Self::GenerationExhausted
            | Self::InvalidWorkspaceRoot(_)
            | Self::ProviderOrderMismatch
            | Self::DerivedStateMismatch => None,
        }
    }
}

#[derive(Debug)]
pub enum MaterializationKeyError {
    InvalidPath { name: &'static str, path: PathBuf },
    CwdOutsideWorkspace,
    InvalidShell,
    InvalidEnvironmentName(String),
    NulValue(String),
    Identity(IdentityError),
    Encoding { source: serde_json::Error },
}

impl std::fmt::Display for MaterializationKeyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath { name, path } => {
                write!(
                    formatter,
                    "materialization {name} must be absolute: {}",
                    path.display()
                )
            }
            Self::CwdOutsideWorkspace => {
                formatter.write_str("materialization cwd is outside workspace")
            }
            Self::InvalidShell => formatter.write_str("materialization shell is invalid"),
            Self::InvalidEnvironmentName(name) => {
                write!(
                    formatter,
                    "materialization environment name {name:?} is invalid"
                )
            }
            Self::NulValue(name) => {
                write!(formatter, "materialization value {name:?} contains NUL")
            }
            Self::Identity(source) => source.fmt(formatter),
            Self::Encoding { source } => {
                write!(formatter, "materialization key encoding failed: {source}")
            }
        }
    }
}

impl std::error::Error for MaterializationKeyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Identity(source) => Some(source),
            Self::Encoding { source } => Some(source),
            _ => None,
        }
    }
}

fn default_true() -> bool {
    true
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(bytes);
    digest.finalize().into()
}

fn validate_absolute(name: &'static str, path: &Path) -> Result<(), MaterializationKeyError> {
    if path.as_os_str().is_empty() || !path.is_absolute() || path.to_string_lossy().contains('\0') {
        return Err(MaterializationKeyError::InvalidPath {
            name,
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized
                    .components()
                    .next_back()
                    .is_some_and(|last| !matches!(last, Component::RootDir | Component::Prefix(_)))
                {
                    normalized.pop();
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    normalized
}

fn validate_values(values: &BTreeMap<String, String>) -> Result<(), MaterializationKeyError> {
    for (name, value) in values {
        let mut bytes = name.bytes();
        let valid_name = matches!(bytes.next(), Some(byte) if byte.is_ascii_uppercase() || byte == b'_')
            && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
        if !valid_name {
            return Err(MaterializationKeyError::InvalidEnvironmentName(
                name.clone(),
            ));
        }
        if value.contains('\0') {
            return Err(MaterializationKeyError::NulValue(name.clone()));
        }
    }
    Ok(())
}
