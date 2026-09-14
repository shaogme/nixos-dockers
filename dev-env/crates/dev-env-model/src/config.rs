use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::environment::EnvironmentPath;
use crate::input::InputSpec;
use crate::provenance::{Layer, ProvenanceIndex};
use crate::provider::ProviderConfig;
use crate::shell::ShellConfig;
use crate::validation::{validate_concrete_path, validate_config_path, validate_id};
use crate::{ModelError, ModelErrorReason, ValueTree, DEV_ENV_SCHEMA_V1};

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
    pub merge: MergePolicy,
    #[serde(default)]
    pub workspace_can_override: Vec<String>,
    #[serde(default)]
    pub cli_can_override: Vec<String>,
    #[serde(default)]
    pub unknown_input: UnknownInputPolicy,
    #[serde(default)]
    pub untrusted_workspace: UntrustedWorkspacePolicy,
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
    pub fn validate(&self) -> Result<(), ModelError> {
        for path in self
            .workspace_can_override
            .iter()
            .chain(self.cli_can_override.iter())
        {
            validate_config_path("policy.override", path)?;
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
    pub root: String,
    #[serde(default)]
    pub search: WorkspaceSearch,
}

impl WorkspaceConfig {
    pub fn validate(&self) -> Result<(), ModelError> {
        validate_concrete_path("workspace.root", &self.root)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShellSelection {
    pub default: String,
}

impl ShellSelection {
    pub fn validate(&self) -> Result<(), ModelError> {
        validate_id("shell.default", &self.default)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentLayer {
    pub inherit_process: Option<bool>,
    pub configured_value_precedence: Option<crate::ConfiguredValuePrecedence>,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    #[serde(default)]
    pub conditional_variables: BTreeMap<String, crate::ConditionalEnvironmentVariable>,
    #[serde(default)]
    pub path: EnvironmentPath,
}

impl EnvironmentLayer {
    pub fn validate(&self) -> Result<(), ModelError> {
        if let Some(precedence) = self.configured_value_precedence {
            let _ = precedence;
        }
        crate::EnvironmentConfig {
            inherit_process: self.inherit_process.unwrap_or(true),
            configured_value_precedence: self.configured_value_precedence.unwrap_or_default(),
            variables: self.variables.clone(),
            conditional_variables: self.conditional_variables.clone(),
            path: self.path.clone(),
        }
        .validate()
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigLayer {
    pub workspace: Option<WorkspaceConfig>,
    pub shell: Option<ShellSelection>,
    #[serde(default)]
    pub shells: BTreeMap<String, ShellConfig>,
    #[serde(default)]
    pub environment: EnvironmentLayer,
    #[serde(default)]
    pub features: ValueTree,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
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
    pub op: OverrideOperation,
    pub value: Option<ValueTree>,
    #[serde(default)]
    pub values: Vec<ValueTree>,
    pub reason: String,
}

impl OverrideSpec {
    pub fn validate(&self, path: &str) -> Result<(), ModelError> {
        validate_config_path(&format!("override.{path}"), path)?;
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
                validate_concrete_path(&format!("override.{path}.values"), path_value)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDocument {
    pub schema: u32,
    pub id: String,
    #[serde(default)]
    pub extends: Vec<String>,
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub config: ConfigLayer,
    #[serde(rename = "override", default)]
    pub overrides: BTreeMap<String, OverrideSpec>,
    #[serde(default)]
    pub inputs: BTreeMap<String, InputSpec>,
}

impl ProfileDocument {
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.schema != DEV_ENV_SCHEMA_V1 {
            return Err(ModelError::UnsupportedSchema {
                found: self.schema,
                expected: DEV_ENV_SCHEMA_V1,
            });
        }
        validate_id("profile.id", &self.id).map_err(|_| ModelError::InvalidProfileId {
            id: self.id.clone(),
        })?;
        for parent in &self.extends {
            validate_id("profile.extends", parent).map_err(|_| {
                ModelError::InvalidProfileReference {
                    profile: self.id.clone(),
                    reference: parent.clone(),
                }
            })?;
        }
        if self.extends.iter().collect::<BTreeSet<_>>().len() != self.extends.len() {
            return Err(ModelError::InvalidValue {
                location: format!("profile {}.extends", self.id),
                reason: ModelErrorReason::Duplicate,
            });
        }
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
        if !self.profiles.contains_key(id) {
            return Err(ModelError::UnknownDefaultProfile { id: id.to_owned() });
        }
        let mut state = BTreeMap::<String, VisitState>::new();
        let mut stack = Vec::new();
        let mut result = Vec::new();
        self.visit(id, &mut state, &mut stack, &mut result)?;
        Ok(result)
    }

    fn visit<'a>(
        &'a self,
        id: &str,
        state: &mut BTreeMap<String, VisitState>,
        stack: &mut Vec<String>,
        result: &mut Vec<&'a ProfileDocument>,
    ) -> Result<(), ModelError> {
        match state.get(id) {
            Some(VisitState::Done) => return Ok(()),
            Some(VisitState::Visiting) => {
                let start = stack.iter().position(|item| item == id).unwrap_or(0);
                let mut profiles = stack[start..].to_vec();
                profiles.push(id.to_owned());
                return Err(ModelError::ProfileCycle { profiles });
            }
            None => {}
        }
        let profile = self
            .profiles
            .get(id)
            .ok_or_else(|| ModelError::MissingParentProfile {
                profile: stack.last().cloned().unwrap_or_else(|| id.to_owned()),
                parent: id.to_owned(),
            })?;
        state.insert(id.to_owned(), VisitState::Visiting);
        stack.push(id.to_owned());
        for parent in &profile.extends {
            if !self.profiles.contains_key(parent) {
                return Err(ModelError::MissingParentProfile {
                    profile: id.to_owned(),
                    parent: parent.clone(),
                });
            }
            self.visit(parent, state, stack, result)?;
        }
        stack.pop();
        state.insert(id.to_owned(), VisitState::Done);
        result.push(profile);
        Ok(())
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
    pub workspace: WorkspaceConfig,
    pub shell: ShellSelection,
    #[serde(default)]
    pub shells: BTreeMap<String, ShellConfig>,
    pub environment: crate::EnvironmentConfig,
    #[serde(default)]
    pub features: ValueTree,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub inputs: BTreeMap<String, InputSpec>,
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub provenance: ProvenanceIndex,
}

impl ResolvedConfig {
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
        for dependency in &provider.depends_on {
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
    for dependency in &provider.depends_on {
        visit_provider(dependency, providers, state, stack, result)?;
    }
    stack.pop();
    state.insert(id.to_owned(), VisitState::Done);
    result.push(id.to_owned());
    Ok(())
}
