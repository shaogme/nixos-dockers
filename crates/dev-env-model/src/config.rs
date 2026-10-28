use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, convert::Infallible};

use crate::{
    environment::{
        ConditionalEnvironmentVariable, ConfiguredValuePrecedence, EnvironmentConfig,
        EnvironmentPath,
    },
    input::InputSpec,
    provenance::{Layer, ProvenanceIndex},
    provider::ProviderConfig,
    shell::ShellConfig,
    validation::StringValidation,
    ModelError, ModelErrorReason, ValueTree, DEV_ENV_SCHEMA_V1,
};
use profile_graph::{
    traverse, validate_extends, validate_profile_id, ProfileGraphError, TraverseError,
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergePolicy {
    #[default]
    Strict,
    PreferChild,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnknownInputPolicy {
    #[default]
    Error,
    Ignore,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UntrustedWorkspacePolicy {
    #[default]
    Prompt,
    Deny,
    Allow,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    #[serde(default)]
    merge: MergePolicy,
    #[serde(default)]
    workspace_can_override: Vec<String>,
    #[serde(default)]
    cli_can_override: Vec<String>,
    #[serde(default)]
    unknown_input: UnknownInputPolicy,
    #[serde(default)]
    untrusted_workspace: UntrustedWorkspacePolicy,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            merge: MergePolicy::Strict,
            workspace_can_override: Vec::new(),
            cli_can_override: Vec::new(),
            unknown_input: UnknownInputPolicy::Error,
            untrusted_workspace: UntrustedWorkspacePolicy::Prompt,
        }
    }
}

impl PolicyConfig {
    pub fn with_merge(mut self, merge: MergePolicy) -> Self {
        self.merge = merge;
        self
    }

    pub fn with_workspace_can_override(mut self, paths: Vec<String>) -> Self {
        self.workspace_can_override = paths;
        self
    }

    pub fn with_untrusted_workspace(mut self, policy: UntrustedWorkspacePolicy) -> Self {
        self.untrusted_workspace = policy;
        self
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        for path in self
            .workspace_can_override
            .iter()
            .chain(self.cli_can_override.iter())
        {
            path.validate_config_path("policy.override")?;
        }
        Ok(())
    }

    pub fn allows_workspace_override(&self, path: &str) -> bool {
        self.workspace_can_override
            .iter()
            .any(|pattern| path_matches(pattern, path))
    }

    pub fn allows_cli_override(&self, path: &str) -> bool {
        self.cli_can_override
            .iter()
            .any(|pattern| path_matches(pattern, path))
    }

    pub fn validate_for_layer(&self, layer: Layer) -> Result<(), ModelError> {
        self.validate()?;
        if self.merge == MergePolicy::PreferChild && !matches!(layer, Layer::Profile | Layer::Admin)
        {
            return Err(ModelError::PreferChildPolicyNotTrusted);
        }
        Ok(())
    }
}

fn path_matches(pattern: &str, path: &str) -> bool {
    pattern == path
        || pattern.strip_suffix(".*").is_some_and(|prefix| {
            path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'.')
        })
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSearch {
    #[default]
    Upward,
    Fixed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    root: String,
    #[serde(default)]
    search: WorkspaceSearch,
}

impl WorkspaceConfig {
    pub fn new(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            search: WorkspaceSearch::Upward,
        }
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        self.root.validate_concrete_path("workspace.root")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShellSelection {
    default: String,
}

impl ShellSelection {
    pub fn new(default: impl Into<String>) -> Self {
        Self {
            default: default.into(),
        }
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        self.default.validate_id("shell.default")
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentLayer {
    inherit_process: Option<bool>,
    configured_value_precedence: Option<ConfiguredValuePrecedence>,
    #[serde(default)]
    variables: BTreeMap<String, String>,
    #[serde(default)]
    conditional_variables: BTreeMap<String, ConditionalEnvironmentVariable>,
    #[serde(default)]
    path: EnvironmentPath,
}

impl EnvironmentLayer {
    pub fn validate(&self) -> Result<(), ModelError> {
        if let Some(precedence) = self.configured_value_precedence {
            let _ = precedence;
        }
        EnvironmentConfig::new(
            self.inherit_process.unwrap_or(true),
            self.configured_value_precedence.unwrap_or_default(),
            self.variables.clone(),
            self.conditional_variables.clone(),
            self.path.clone(),
        )
        .validate()
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigLayer {
    workspace: Option<WorkspaceConfig>,
    shell: Option<ShellSelection>,
    #[serde(default)]
    shells: BTreeMap<String, ShellConfig>,
    #[serde(default)]
    environment: EnvironmentLayer,
    #[serde(default)]
    features: ValueTree,
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfig>,
}

impl ConfigLayer {
    pub fn validate(&self) -> Result<(), ModelError> {
        if let Some(workspace) = &self.workspace {
            workspace.validate()?;
        }
        if let Some(shell) = &self.shell {
            shell.validate()?;
        }
        for (id, shell) in &self.shells {
            shell.validate(id)?;
        }
        self.environment.validate()?;
        self.features.validate("config.features")?;
        for (id, provider) in &self.providers {
            provider.validate(id)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverrideOperation {
    Set,
    Unset,
    Replace,
    Append,
    Remove,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OverrideSpec {
    op: OverrideOperation,
    value: Option<ValueTree>,
    #[serde(default)]
    values: Vec<ValueTree>,
    reason: String,
}

impl OverrideSpec {
    pub fn new(op: OverrideOperation, reason: impl Into<String>) -> Self {
        Self {
            op,
            value: None,
            values: Vec::new(),
            reason: reason.into(),
        }
    }

    pub fn with_value(mut self, value: ValueTree) -> Self {
        self.value = Some(value);
        self
    }

    pub fn with_values(mut self, values: Vec<ValueTree>) -> Self {
        self.values = values;
        self
    }

    pub fn validate(&self, path: &str) -> Result<(), ModelError> {
        path.validate_config_path(&format!("override.{path}"))?;
        if self.reason.is_empty() || self.reason.contains('\0') {
            return Err(ModelError::InvalidOverride {
                path: path.to_owned(),
                reason: ModelErrorReason::Empty,
            });
        }
        match self.op {
            OverrideOperation::Set => {
                if self.value.is_none() || !self.values.is_empty() {
                    return Err(ModelError::InvalidOverride {
                        path: path.to_owned(),
                        reason: ModelErrorReason::MustBeExplicit,
                    });
                }
            }
            OverrideOperation::Unset => {
                if self.value.is_some() || !self.values.is_empty() {
                    return Err(ModelError::InvalidOverride {
                        path: path.to_owned(),
                        reason: ModelErrorReason::MustBeExplicit,
                    });
                }
            }
            OverrideOperation::Replace | OverrideOperation::Append | OverrideOperation::Remove => {
                if self.values.is_empty() || self.value.is_some() {
                    return Err(ModelError::InvalidOverride {
                        path: path.to_owned(),
                        reason: ModelErrorReason::MustBeNonEmpty,
                    });
                }
            }
        }
        if let Some(value) = &self.value {
            value.validate(&format!("override.{path}.value"))?;
        }
        for (index, value) in self.values.iter().enumerate() {
            value.validate(&format!("override.{path}.values[{index}]"))?;
        }
        if matches!(
            path,
            "environment.path.prepend" | "environment.path.append" | "environment.path.remove"
        ) {
            for value in &self.values {
                let Some(path_value) = value.as_str() else {
                    return Err(ModelError::InvalidOverride {
                        path: path.to_owned(),
                        reason: ModelErrorReason::MustBeScalar,
                    });
                };
                path_value.validate_concrete_path(&format!("override.{path}.values"))?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDocument {
    schema: u32,
    id: String,
    #[serde(default)]
    extends: Vec<String>,
    #[serde(default)]
    policy: PolicyConfig,
    #[serde(default)]
    config: ConfigLayer,
    #[serde(rename = "override", default)]
    overrides: BTreeMap<String, OverrideSpec>,
    #[serde(default)]
    inputs: BTreeMap<String, InputSpec>,
}

impl ProfileDocument {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            schema: DEV_ENV_SCHEMA_V1,
            id: id.into(),
            extends: Vec::new(),
            policy: PolicyConfig::default(),
            config: ConfigLayer::default(),
            overrides: BTreeMap::new(),
            inputs: BTreeMap::new(),
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn with_extends(mut self, extends: Vec<String>) -> Self {
        self.extends = extends;
        self
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if self.schema != DEV_ENV_SCHEMA_V1 {
            return Err(ModelError::UnsupportedSchema {
                found: self.schema,
                expected: DEV_ENV_SCHEMA_V1,
            });
        }
        validate_profile_id(&self.id).map_err(|_| ModelError::InvalidProfileId {
            id: self.id.clone(),
        })?;
        validate_extends(&self.id, &self.extends).map_err(|error| match error {
            ProfileGraphError::InvalidId(reference) => ModelError::InvalidProfileReference {
                profile: self.id.clone(),
                reference,
            },
            ProfileGraphError::DuplicateParent { .. } => ModelError::InvalidValue {
                location: format!("profile {}.extends", self.id),
                reason: ModelErrorReason::Duplicate,
            },
            _ => unreachable!("extends validation only returns id and duplicate errors"),
        })?;
        self.policy.validate()?;
        self.config.validate()?;
        for (path, override_spec) in &self.overrides {
            override_spec.validate(path)?;
        }
        for (name, input) in &self.inputs {
            input.validate(name)?;
        }
        Ok(())
    }

    pub fn validate_for_layer(&self, layer: Layer) -> Result<(), ModelError> {
        self.validate()?;
        self.policy.validate_for_layer(layer)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProfileSet {
    profiles: BTreeMap<String, ProfileDocument>,
    default_profile: String,
}

impl ProfileSet {
    pub fn new(
        profiles: impl IntoIterator<Item = ProfileDocument>,
        default_profile: impl Into<String>,
    ) -> Result<Self, ModelError> {
        let mut indexed = BTreeMap::new();
        for profile in profiles {
            profile.validate()?;
            if indexed
                .insert(profile.id.clone(), profile.clone())
                .is_some()
            {
                return Err(ModelError::DuplicateProfileId { id: profile.id });
            }
        }
        let set = Self {
            profiles: indexed,
            default_profile: default_profile.into(),
        };
        set.resolve_chain(&set.default_profile)?;
        Ok(set)
    }

    pub fn profiles(&self) -> impl Iterator<Item = &ProfileDocument> {
        self.profiles.values()
    }

    pub fn default_profile(&self) -> &str {
        &self.default_profile
    }

    /// Return parents before children, preserving each profile's `extends`
    /// declaration order.
    pub fn chain(&self) -> Result<Vec<&ProfileDocument>, ModelError> {
        self.resolve_chain(&self.default_profile)
    }

    pub fn chain_for(&self, id: &str) -> Result<Vec<&ProfileDocument>, ModelError> {
        self.resolve_chain(id)
    }

    fn resolve_chain(&self, id: &str) -> Result<Vec<&ProfileDocument>, ModelError> {
        let mut result = Vec::new();
        traverse(
            id,
            |profile_id| {
                Ok::<_, Infallible>(
                    self.profiles
                        .get(profile_id)
                        .map(|profile| (profile, profile.extends.clone())),
                )
            },
            |_, profile| {
                result.push(profile);
                Ok(())
            },
        )
        .map_err(|error| match error {
            TraverseError::Source(never) => match never {},
            TraverseError::Graph(ProfileGraphError::MissingProfile(_)) => {
                ModelError::UnknownDefaultProfile { id: id.to_owned() }
            }
            TraverseError::Graph(ProfileGraphError::MissingParent { profile, parent }) => {
                ModelError::MissingParentProfile { profile, parent }
            }
            TraverseError::Graph(ProfileGraphError::Cycle(profiles)) => {
                ModelError::ProfileCycle { profiles }
            }
            TraverseError::Graph(ProfileGraphError::InvalidId(reference)) => {
                ModelError::InvalidProfileReference {
                    profile: id.to_owned(),
                    reference,
                }
            }
            TraverseError::Graph(ProfileGraphError::DuplicateParent { profile, .. }) => {
                ModelError::InvalidValue {
                    location: format!("profile {profile}.extends"),
                    reason: ModelErrorReason::Duplicate,
                }
            }
            TraverseError::Graph(ProfileGraphError::InheritanceDepthExceeded { limit: _ }) => {
                ModelError::InvalidValue {
                    location: "profile inheritance".to_owned(),
                    reason: ModelErrorReason::Unsupported,
                }
            }
        })?;
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisitState {
    Visiting,
    Done,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedConfig {
    workspace: WorkspaceConfig,
    shell: ShellSelection,
    #[serde(default)]
    shells: BTreeMap<String, ShellConfig>,
    environment: EnvironmentConfig,
    #[serde(default)]
    features: ValueTree,
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    inputs: BTreeMap<String, InputSpec>,
    #[serde(default)]
    policy: PolicyConfig,
    #[serde(default)]
    provenance: ProvenanceIndex,
}

impl ResolvedConfig {
    pub fn new(
        workspace: WorkspaceConfig,
        shell: ShellSelection,
        environment: EnvironmentConfig,
    ) -> Self {
        Self {
            workspace,
            shell,
            shells: BTreeMap::new(),
            environment,
            features: ValueTree::default(),
            providers: BTreeMap::new(),
            inputs: BTreeMap::new(),
            policy: PolicyConfig::default(),
            provenance: ProvenanceIndex::default(),
        }
    }

    pub fn with_shells(mut self, shells: BTreeMap<String, ShellConfig>) -> Self {
        self.shells = shells;
        self
    }

    pub fn with_providers(mut self, providers: BTreeMap<String, ProviderConfig>) -> Self {
        self.providers = providers;
        self
    }

    pub fn with_inputs(mut self, inputs: BTreeMap<String, InputSpec>) -> Self {
        self.inputs = inputs;
        self
    }

    pub fn with_policy(mut self, policy: PolicyConfig) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_features(mut self, features: ValueTree) -> Self {
        self.features = features;
        self
    }

    pub fn with_provenance(mut self, provenance: ProvenanceIndex) -> Self {
        self.provenance = provenance;
        self
    }

    pub fn with_environment(mut self, environment: EnvironmentConfig) -> Self {
        self.environment = environment;
        self
    }

    pub fn with_shell(mut self, shell: ShellSelection) -> Self {
        self.shell = shell;
        self
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        self.workspace.validate()?;
        self.shell.validate()?;
        if !self.shells.contains_key(&self.shell.default) {
            return Err(ModelError::MissingDefaultShell {
                shell: self.shell.default.clone(),
            });
        }
        for (id, shell) in &self.shells {
            shell.validate(id)?;
        }
        self.environment.validate()?;
        self.features.validate("features")?;
        for (id, provider) in &self.providers {
            provider.validate(id)?;
        }
        validate_provider_graph(&self.providers)?;
        for (name, input) in &self.inputs {
            input.validate(name)?;
        }
        self.policy.validate()
    }

    pub fn provider_order(&self) -> Result<Vec<String>, ModelError> {
        validate_provider_graph(&self.providers)?;
        let mut result = Vec::new();
        let mut state = BTreeMap::new();
        let mut stack = Vec::new();
        for id in self.providers.keys() {
            visit_provider(id, &self.providers, &mut state, &mut stack, &mut result)?;
        }
        Ok(result)
    }
}

fn validate_provider_graph(providers: &BTreeMap<String, ProviderConfig>) -> Result<(), ModelError> {
    for (id, provider) in providers {
        for dependency in provider.depends_on() {
            if !providers.contains_key(dependency) {
                return Err(ModelError::MissingProviderDependency {
                    provider: id.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
    }
    let mut state = BTreeMap::new();
    let mut result = Vec::new();
    let mut stack = Vec::new();
    for id in providers.keys() {
        visit_provider(id, providers, &mut state, &mut stack, &mut result)?;
    }
    Ok(())
}

fn visit_provider(
    id: &str,
    providers: &BTreeMap<String, ProviderConfig>,
    state: &mut BTreeMap<String, VisitState>,
    stack: &mut Vec<String>,
    result: &mut Vec<String>,
) -> Result<(), ModelError> {
    match state.get(id) {
        Some(VisitState::Done) => return Ok(()),
        Some(VisitState::Visiting) => {
            let start = stack.iter().position(|item| item == id).unwrap_or(0);
            let mut cycle = stack[start..].to_vec();
            cycle.push(id.to_owned());
            return Err(ModelError::ProviderDependencyCycle { providers: cycle });
        }
        None => {}
    }
    let provider = providers
        .get(id)
        .ok_or_else(|| ModelError::MissingProviderDependency {
            provider: id.to_owned(),
            dependency: id.to_owned(),
        })?;
    state.insert(id.to_owned(), VisitState::Visiting);
    stack.push(id.to_owned());
    for dependency in provider.depends_on() {
        visit_provider(dependency, providers, state, stack, result)?;
    }
    stack.pop();
    state.insert(id.to_owned(), VisitState::Done);
    result.push(id.to_owned());
    Ok(())
}

impl PolicyConfig {
    pub fn merge(&self) -> MergePolicy {
        self.merge
    }

    pub fn workspace_can_override(&self) -> &Vec<String> {
        &self.workspace_can_override
    }

    pub fn cli_can_override(&self) -> &Vec<String> {
        &self.cli_can_override
    }

    pub fn unknown_input(&self) -> UnknownInputPolicy {
        self.unknown_input
    }

    pub fn untrusted_workspace(&self) -> UntrustedWorkspacePolicy {
        self.untrusted_workspace
    }
}

impl WorkspaceConfig {
    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn search(&self) -> WorkspaceSearch {
        self.search
    }
}

impl ShellSelection {
    pub fn default(&self) -> &str {
        &self.default
    }
}

impl EnvironmentLayer {
    pub fn inherit_process(&self) -> Option<bool> {
        self.inherit_process
    }

    pub fn configured_value_precedence(&self) -> &Option<ConfiguredValuePrecedence> {
        &self.configured_value_precedence
    }

    pub fn variables(&self) -> &BTreeMap<String, String> {
        &self.variables
    }

    pub fn conditional_variables(&self) -> &BTreeMap<String, ConditionalEnvironmentVariable> {
        &self.conditional_variables
    }

    pub fn path(&self) -> &EnvironmentPath {
        &self.path
    }
}

impl ConfigLayer {
    pub fn workspace(&self) -> &Option<WorkspaceConfig> {
        &self.workspace
    }

    pub fn shell(&self) -> &Option<ShellSelection> {
        &self.shell
    }

    pub fn shells(&self) -> &BTreeMap<String, ShellConfig> {
        &self.shells
    }

    pub fn environment(&self) -> &EnvironmentLayer {
        &self.environment
    }

    pub fn features(&self) -> &ValueTree {
        &self.features
    }

    pub fn providers(&self) -> &BTreeMap<String, ProviderConfig> {
        &self.providers
    }
}

impl OverrideSpec {
    pub fn op(&self) -> OverrideOperation {
        self.op
    }

    pub fn value(&self) -> &Option<ValueTree> {
        &self.value
    }

    pub fn values(&self) -> &Vec<ValueTree> {
        &self.values
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl ProfileDocument {
    pub fn schema(&self) -> u32 {
        self.schema
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn extends(&self) -> &Vec<String> {
        &self.extends
    }

    pub fn policy(&self) -> &PolicyConfig {
        &self.policy
    }

    pub fn config(&self) -> &ConfigLayer {
        &self.config
    }

    pub fn overrides(&self) -> &BTreeMap<String, OverrideSpec> {
        &self.overrides
    }

    pub fn inputs(&self) -> &BTreeMap<String, InputSpec> {
        &self.inputs
    }
}

impl ResolvedConfig {
    pub fn workspace(&self) -> &WorkspaceConfig {
        &self.workspace
    }

    pub fn shell(&self) -> &ShellSelection {
        &self.shell
    }

    pub fn shells(&self) -> &BTreeMap<String, ShellConfig> {
        &self.shells
    }

    pub fn environment(&self) -> &EnvironmentConfig {
        &self.environment
    }

    pub fn features(&self) -> &ValueTree {
        &self.features
    }

    pub fn providers(&self) -> &BTreeMap<String, ProviderConfig> {
        &self.providers
    }

    pub fn inputs(&self) -> &BTreeMap<String, InputSpec> {
        &self.inputs
    }

    pub fn policy(&self) -> &PolicyConfig {
        &self.policy
    }

    pub fn provenance(&self) -> &ProvenanceIndex {
        &self.provenance
    }
}
