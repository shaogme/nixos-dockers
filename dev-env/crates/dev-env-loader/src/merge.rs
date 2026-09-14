use crate::error::{ConflictRemedy, LoaderError, TrustViolationReason, ValueKind};
use crate::raw::{table, RawProfile};
use crate::source::{LoadedProfile, ProfileSource, SourceKind};
use dev_env_model::{
    Layer, ModelError, Origin, OverrideOperation, OverrideSpec, ParsedInput, PolicyConfig,
    ProvenanceIndex, ResolvedConfig, Sensitivity, ValueTree,
};
use std::collections::BTreeMap;

pub(crate) struct MergedConfig {
    root: toml::value::Table,
    origins: BTreeMap<String, Origin>,
    provenance: ProvenanceIndex,
}

impl Default for MergedConfig {
    fn default() -> Self {
        Self {
            root: toml::value::Table::new(),
            origins: BTreeMap::new(),
            provenance: ProvenanceIndex::default(),
        }
    }
}

impl MergedConfig {
    pub(crate) fn merge_profile(
        &mut self,
        profile: &RawProfile,
        source: &ProfileSource,
    ) -> Result<(), LoaderError> {
        let overrides = &profile.document.overrides;
        self.validate_source_permissions(profile, source)?;

        // Policy is merged first. This makes a trusted image profile's
        // workspace_can_override declarations available to later overlays.
        if let Some(policy) = table(&profile.root, "policy") {
            self.merge_table("policy", policy, source, overrides)?;
        }
        let policy = self.policy()?;

        if let Some(config) = table(&profile.root, "config") {
            self.merge_table_with_policy("", config, source, overrides, &policy)?;
        }
        if let Some(inputs) = table(&profile.root, "inputs") {
            self.merge_table_with_policy("inputs", inputs, source, overrides, &policy)?;
        }

        for (path, spec) in overrides {
            self.validate_override_permission(path, source, &policy, &profile.document.id)?;
            self.apply_override(path, spec, source)?;
        }
        Ok(())
    }

    /// Rebuild a merger from a validated config for the CLI adapter. The
    /// resulting provenance is retained and all values become the baseline.
    pub(crate) fn from_config(config: ResolvedConfig) -> Result<Self, LoaderError> {
        let value = toml::Value::try_from(config).map_err(|source| LoaderError::Serialize {
            location: "resolved config".to_owned(),
            source,
        })?;
        let toml::Value::Table(mut root) = value else {
            return Err(LoaderError::MissingRequired {
                path: "resolved config".to_owned(),
            });
        };
        let provenance: ProvenanceIndex = root
            .remove("provenance")
            .map(|value| value.try_into())
            .transpose()
            .map_err(|source| LoaderError::Parse {
                location: "resolved config.provenance".to_owned(),
                source,
            })?
            .unwrap_or_default();
        let mut origins = BTreeMap::new();
        for (path, entry) in provenance.iter() {
            if let Some(origin) = entry.origins.last() {
                origins.insert(path.to_owned(), origin.clone());
            }
        }
        Ok(Self {
            root,
            origins,
            provenance,
        })
    }

    pub(crate) fn apply_cli_patch(
        &mut self,
        path: &str,
        spec: &OverrideSpec,
    ) -> Result<(), LoaderError> {
        spec.validate(path).map_err(|source| LoaderError::Model {
            location: Some(format!("override.{path}")),
            source,
        })?;
        let policy = self.policy()?;
        if !policy.allows_cli_override(path) {
            return Err(LoaderError::TrustViolation {
                profile: "<cli>".to_owned(),
                source: SourceKind::Cli,
                path: path.to_owned(),
                reason: TrustViolationReason::CliOverrideNotAllowed,
            });
        }
        self.apply_override(
            path,
            spec,
            &ProfileSource::new("<cli>", SourceKind::Cli, ""),
        )
    }

    pub(crate) fn set_runtime_value(
        &mut self,
        path: &str,
        value: ValueTree,
        origin: Origin,
        sensitivity: Sensitivity,
    ) -> Result<(), LoaderError> {
        let value = value_tree_to_toml(path, &value)?;
        set_path(&mut self.root, path, value.clone());
        self.record_value(path, &value, &origin, sensitivity);
        self.origins.insert(path.to_owned(), origin);
        Ok(())
    }

    pub(crate) fn value_at(&self, path: &str) -> Option<&toml::Value> {
        get_path(&self.root, path)
    }

    pub(crate) fn config_snapshot(&self) -> Result<ResolvedConfig, LoaderError> {
        let mut root = self.root.clone();
        root.entry("environment".to_owned())
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
        let mut config: ResolvedConfig =
            toml::Value::Table(root)
                .try_into()
                .map_err(|source| LoaderError::Parse {
                    location: "runtime input configuration".to_owned(),
                    source,
                })?;
        config.provenance = self.provenance.clone();
        config.validate().map_err(|source| LoaderError::Model {
            location: Some("runtime input configuration".to_owned()),
            source,
        })?;
        Ok(config)
    }

    pub(crate) fn finish(
        mut self,
        profile_chain: Vec<LoadedProfile>,
    ) -> Result<(ResolvedConfig, Vec<LoadedProfile>), LoaderError> {
        for required in ["workspace", "shell", "shells"] {
            if !self.root.contains_key(required) {
                return Err(LoaderError::MissingRequired {
                    path: required.to_owned(),
                });
            }
        }
        self.root
            .entry("environment".to_owned())
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));

        let value = toml::Value::Table(self.root.clone());
        let mut config: ResolvedConfig = value.try_into().map_err(|source| LoaderError::Parse {
            location: "merged environment configuration".to_owned(),
            source,
        })?;
        config.provenance = self.provenance;
        config.validate().map_err(|source| LoaderError::Model {
            location: Some("merged environment configuration".to_owned()),
            source,
        })?;
        validate_input_names(&config)?;
        validate_input_targets(&config, &self.root)?;
        Ok((config, profile_chain))
    }

    fn validate_source_permissions(
        &self,
        profile: &RawProfile,
        source: &ProfileSource,
    ) -> Result<(), LoaderError> {
        if source.source.is_trusted() {
            return Ok(());
        }
        if profile.root.get("policy").is_some() {
            return Err(LoaderError::TrustViolation {
                profile: profile.document.id.clone(),
                source: source.source,
                path: "policy".to_owned(),
                reason: TrustViolationReason::OnlyTrustedSourcesMayChangePolicy,
            });
        }
        if profile.root.get("inputs").is_some() {
            return Err(LoaderError::TrustViolation {
                profile: profile.document.id.clone(),
                source: source.source,
                path: "inputs".to_owned(),
                reason: TrustViolationReason::OnlyTrustedSourcesMayDeclareInputs,
            });
        }
        if profile.document.overrides.keys().any(|path| {
            path == "policy"
                || path.starts_with("policy.")
                || path == "inputs"
                || path.starts_with("inputs.")
        }) {
            let path = profile
                .document
                .overrides
                .keys()
                .find(|path| {
                    *path == "policy"
                        || path.starts_with("policy.")
                        || *path == "inputs"
                        || path.starts_with("inputs.")
                })
                .cloned()
                .expect("the preceding any call found an entry");
            return Err(LoaderError::TrustViolation {
                profile: profile.document.id.clone(),
                source: source.source,
                path,
                reason: TrustViolationReason::OnlyTrustedSourcesMayChangePolicy,
            });
        }
        Ok(())
    }

    fn merge_table(
        &mut self,
        prefix: &str,
        incoming: &toml::value::Table,
        source: &ProfileSource,
        overrides: &BTreeMap<String, OverrideSpec>,
    ) -> Result<(), LoaderError> {
        let policy = self.policy()?;
        self.merge_table_with_policy(prefix, incoming, source, overrides, &policy)
    }

    fn merge_table_with_policy(
        &mut self,
        prefix: &str,
        incoming: &toml::value::Table,
        source: &ProfileSource,
        overrides: &BTreeMap<String, OverrideSpec>,
        policy: &PolicyConfig,
    ) -> Result<(), LoaderError> {
        for (key, value) in incoming {
            let path = if prefix.is_empty() {
                key.to_owned()
            } else {
                format!("{prefix}.{key}")
            };
            self.merge_path(&path, value.clone(), source, overrides, policy)?;
        }
        Ok(())
    }

    fn merge_path(
        &mut self,
        path: &str,
        incoming: toml::Value,
        source: &ProfileSource,
        overrides: &BTreeMap<String, OverrideSpec>,
        policy: &PolicyConfig,
    ) -> Result<(), LoaderError> {
        // An explicit operation owns the whole path. Do not first merge the
        // child declaration and then apply the operation a second time.
        if overrides.contains_key(path) {
            return Ok(());
        }
        self.reject_untrusted_provider_addition(path, &incoming, source)?;

        let existing = get_path(&self.root, path).cloned();
        let Some(existing) = existing else {
            set_path(&mut self.root, path, incoming.clone());
            let origin = source.origin(&source.id, None);
            self.record_value(path, &incoming, &origin, Sensitivity::Public);
            self.origins.insert(path.to_owned(), origin);
            return Ok(());
        };

        match (existing, incoming.clone()) {
            (toml::Value::Table(_), toml::Value::Table(table)) => {
                for (key, value) in table {
                    self.merge_path(&format!("{path}.{key}"), value, source, overrides, policy)?;
                }
                Ok(())
            }
            (existing, incoming) if existing == incoming => {
                let origin = source.origin(&source.id, None);
                self.record_value(path, &incoming, &origin, Sensitivity::Public);
                Ok(())
            }
            (_existing, incoming) => {
                let origin = source.origin(&source.id, None);
                if policy.merge == dev_env_model::MergePolicy::PreferChild
                    && source.source.is_trusted()
                {
                    set_path(&mut self.root, path, incoming.clone());
                    self.record_value(path, &incoming, &origin, Sensitivity::Public);
                    self.origins.insert(path.to_owned(), origin);
                    return Ok(());
                }
                match source.source {
                    SourceKind::WorkspaceOverlay if !policy.allows_workspace_override(path) => {
                        Err(LoaderError::Model {
                            location: Some(path.to_owned()),
                            source: ModelError::WorkspaceOverrideNotAllowed {
                                path: path.to_owned(),
                            },
                        })
                    }
                    SourceKind::Cli if !policy.allows_cli_override(path) => {
                        Err(LoaderError::Model {
                            location: Some(path.to_owned()),
                            source: ModelError::CliOverrideNotAllowed {
                                path: path.to_owned(),
                            },
                        })
                    }
                    _ => Err(self.conflict(path, origin)),
                }
            }
        }
    }

    fn reject_untrusted_provider_addition(
        &self,
        path: &str,
        incoming: &toml::Value,
        source: &ProfileSource,
    ) -> Result<(), LoaderError> {
        if source.source.is_trusted() || !path.starts_with("providers") {
            return Ok(());
        }
        let provider_id = path
            .strip_prefix("providers.")
            .and_then(|rest| rest.split('.').next())
            .or_else(|| {
                if path == "providers" {
                    incoming
                        .as_table()
                        .and_then(|table| table.keys().next())
                        .map(String::as_str)
                } else {
                    None
                }
            });
        let Some(provider_id) = provider_id else {
            return Ok(());
        };
        if get_path(&self.root, &format!("providers.{provider_id}")).is_none() {
            return Err(LoaderError::TrustViolation {
                profile: source.id.clone(),
                source: source.source,
                path: format!("providers.{provider_id}"),
                reason: TrustViolationReason::ProviderNotDeclared,
            });
        }
        Ok(())
    }

    fn validate_override_permission(
        &self,
        path: &str,
        source: &ProfileSource,
        policy: &PolicyConfig,
        profile: &str,
    ) -> Result<(), LoaderError> {
        if source.source.is_trusted() {
            return Ok(());
        }
        let (allowed, reason) = match source.source {
            SourceKind::WorkspaceOverlay => (
                policy.allows_workspace_override(path),
                TrustViolationReason::WorkspaceOverrideNotAllowed,
            ),
            SourceKind::Cli => (
                policy.allows_cli_override(path),
                TrustViolationReason::CliOverrideNotAllowed,
            ),
            _ => (false, TrustViolationReason::UntrustedSourceMayNotOverride),
        };
        if allowed {
            Ok(())
        } else {
            Err(LoaderError::TrustViolation {
                profile: profile.to_owned(),
                source: source.source,
                path: path.to_owned(),
                reason,
            })
        }
    }

    fn apply_override(
        &mut self,
        path: &str,
        spec: &OverrideSpec,
        source: &ProfileSource,
    ) -> Result<(), LoaderError> {
        let origin = source.origin(&source.id, Some(spec.reason.clone()));
        match spec.op {
            OverrideOperation::Set => {
                let value = spec
                    .value
                    .as_ref()
                    .expect("validated set override has a value");
                let value = value_tree_to_toml(path, value)?;
                set_path(&mut self.root, path, value.clone());
                self.record_value(path, &value, &origin, Sensitivity::Public);
                self.origins.insert(path.to_owned(), origin);
            }
            OverrideOperation::Unset => {
                remove_path(&mut self.root, path);
                self.provenance
                    .insert(path.to_owned(), origin.clone(), Sensitivity::Public);
                self.origins.remove(path);
            }
            OverrideOperation::Replace | OverrideOperation::Append | OverrideOperation::Remove => {
                let values = spec
                    .values
                    .iter()
                    .map(|value| value_tree_to_toml(path, value))
                    .collect::<Result<Vec<_>, _>>()?;
                let current = get_path(&self.root, path).cloned();
                let next = match current {
                    None if spec.op == OverrideOperation::Remove => None,
                    None => Some(toml::Value::Array(values)),
                    Some(toml::Value::Array(mut current)) => {
                        match spec.op {
                            OverrideOperation::Replace => current = values,
                            OverrideOperation::Append => current.extend(values),
                            OverrideOperation::Remove => {
                                current.retain(|value| !values.iter().any(|item| item == value))
                            }
                            _ => unreachable!(),
                        }
                        Some(toml::Value::Array(current))
                    }
                    Some(value) => {
                        return Err(LoaderError::OverrideTypeMismatch {
                            path: path.to_owned(),
                            operation: spec.op,
                            actual: value_kind(&value),
                        })
                    }
                };
                if let Some(value) = next {
                    set_path(&mut self.root, path, value.clone());
                    self.record_value(path, &value, &origin, Sensitivity::Public);
                    self.origins.insert(path.to_owned(), origin);
                } else {
                    self.provenance
                        .insert(path.to_owned(), origin, Sensitivity::Public);
                }
            }
        }
        Ok(())
    }

    fn conflict(&self, path: &str, incoming: Origin) -> LoaderError {
        LoaderError::Conflict {
            path: path.to_owned(),
            previous: Box::new(
                self.origins
                    .get(path)
                    .cloned()
                    .unwrap_or_else(Origin::default),
            ),
            incoming: Box::new(incoming),
            remedy: ConflictRemedy::ExplicitOverride {
                path: path.to_owned(),
            },
        }
    }

    fn policy(&self) -> Result<PolicyConfig, LoaderError> {
        let Some(policy) = self.root.get("policy") else {
            return Ok(PolicyConfig::default());
        };
        policy
            .clone()
            .try_into()
            .map_err(|source| LoaderError::Parse {
                location: "merged policy".to_owned(),
                source,
            })
    }

    fn record_value(
        &mut self,
        path: &str,
        value: &toml::Value,
        origin: &Origin,
        sensitivity: Sensitivity,
    ) {
        self.provenance
            .insert(path.to_owned(), origin.clone(), sensitivity);
        if let toml::Value::Table(table) = value {
            for (key, value) in table {
                self.record_value(&format!("{path}.{key}"), value, origin, sensitivity);
            }
        } else if let toml::Value::Array(values) = value {
            for (index, value) in values.iter().enumerate() {
                self.record_value(&format!("{path}[{index}]"), value, origin, sensitivity);
            }
        }
    }
}

fn get_path<'a>(root: &'a toml::value::Table, path: &str) -> Option<&'a toml::Value> {
    let mut current = None;
    for (index, part) in path.split('.').enumerate() {
        current = if index == 0 {
            root.get(part)
        } else {
            current?.as_table()?.get(part)
        };
    }
    current
}

fn set_path(root: &mut toml::value::Table, path: &str, value: toml::Value) {
    let parts = path.split('.').collect::<Vec<_>>();
    let mut table = root;
    for part in &parts[..parts.len() - 1] {
        table = table
            .entry((*part).to_owned())
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
            .as_table_mut()
            .expect("config paths cannot address through scalar values");
    }
    table.insert(parts[parts.len() - 1].to_owned(), value);
}

fn remove_path(root: &mut toml::value::Table, path: &str) -> Option<toml::Value> {
    let parts = path.split('.').collect::<Vec<_>>();
    if parts.len() == 1 {
        return root.remove(parts[0]);
    }
    let mut table = root;
    for part in &parts[..parts.len() - 1] {
        table = table.get_mut(*part)?.as_table_mut()?;
    }
    table.remove(parts[parts.len() - 1])
}

fn value_tree_to_toml(path: &str, value: &ValueTree) -> Result<toml::Value, LoaderError> {
    match value {
        ValueTree::Null => Err(LoaderError::UnrepresentableOverride {
            path: path.to_owned(),
        }),
        ValueTree::Bool(value) => Ok(toml::Value::Boolean(*value)),
        ValueTree::Integer(value) => Ok(toml::Value::Integer(*value)),
        ValueTree::Float(value) => Ok(toml::Value::Float(*value)),
        ValueTree::String(value) => Ok(toml::Value::String(value.clone())),
        ValueTree::Array(values) => values
            .iter()
            .map(|value| value_tree_to_toml(path, value))
            .collect::<Result<Vec<_>, _>>()
            .map(toml::Value::Array),
        ValueTree::Map(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), value_tree_to_toml(path, value)?)))
            .collect::<Result<toml::value::Table, LoaderError>>()
            .map(toml::Value::Table),
    }
}

fn value_kind(value: &toml::Value) -> ValueKind {
    match value {
        toml::Value::Array(_) => ValueKind::Array,
        toml::Value::Table(_) => ValueKind::Table,
        _ => ValueKind::Scalar,
    }
}

fn validate_input_names(config: &ResolvedConfig) -> Result<(), LoaderError> {
    let mut names = BTreeMap::<String, String>::new();
    for name in config.inputs.keys() {
        register_input_name(&mut names, name, name)?;
    }
    for (name, input) in &config.inputs {
        if let Some(export_as) = &input.export_as {
            register_input_name(&mut names, export_as, name)?;
        }
    }
    Ok(())
}

fn register_input_name(
    names: &mut BTreeMap<String, String>,
    name: &str,
    owner: &str,
) -> Result<(), LoaderError> {
    if let Some(previous) = names.insert(name.to_owned(), owner.to_owned()) {
        if previous != owner {
            return Err(LoaderError::DuplicateInputName {
                name: name.to_owned(),
                first: previous,
                second: owner.to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_input_targets(
    config: &ResolvedConfig,
    root: &toml::value::Table,
) -> Result<(), LoaderError> {
    for (name, input) in &config.inputs {
        if get_path(root, &input.target).is_none() {
            return Err(LoaderError::InputTargetMissing {
                name: name.to_owned(),
                target: input.target.clone(),
            });
        }
    }
    Ok(())
}

fn parsed_input_to_value(value: ParsedInput) -> ValueTree {
    match value {
        ParsedInput::Bool(value) => ValueTree::Bool(value),
        ParsedInput::Enum(value) | ParsedInput::Path(value) | ParsedInput::String(value) => {
            ValueTree::String(value)
        }
        ParsedInput::Integer(value) => ValueTree::Integer(value),
    }
}

pub(crate) fn parsed_input_value(value: ParsedInput) -> ValueTree {
    parsed_input_to_value(value)
}

#[allow(dead_code)]
fn _layer_is_runtime(layer: Layer) -> bool {
    layer == Layer::Runtime
}
