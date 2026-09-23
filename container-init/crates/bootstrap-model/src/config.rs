use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::action::{Action, ActionKind};
use crate::condition::{Condition, ConditionValue};
use crate::error::ModelError;
use crate::input::BootstrapInput;
use crate::validation::{
    is_env_name, validate_argv_value, validate_executable, validate_path_template,
    validate_user_name,
};

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapMode {
    Strict,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NonInteractivePolicy {
    Deny,
    Allow,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BootstrapConfig {
    pub schema: u32,
    pub mode: BootstrapMode,
    pub workspace_root: String,
    pub allow_workspace_overlay: bool,
    pub non_interactive: NonInteractivePolicy,
    pub identity: IdentityConfig,
    pub handoff: HandoffConfig,
    pub policy: BootstrapPolicy,
    #[serde(default)]
    pub inputs: BTreeMap<String, BootstrapInput>,
    #[serde(default)]
    pub actions: Vec<Action>,
}

impl BootstrapConfig {
    /// Validate the model without performing any side effects.
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.schema != crate::BOOTSTRAP_SCHEMA_V1 {
            return Err(ModelError::Invalid {
                location: "bootstrap.schema".to_owned(),
                message: format!(
                    "unsupported bootstrap schema {}; expected {}",
                    self.schema,
                    crate::BOOTSTRAP_SCHEMA_V1
                ),
            });
        }

        validate_path_template("bootstrap.workspace_root", &self.workspace_root)?;
        if self.workspace_root.starts_with("${") {
            return Err(ModelError::Invalid {
                location: "bootstrap.workspace_root".to_owned(),
                message: "workspace_root must be a concrete absolute path".to_owned(),
            });
        }

        self.identity.validate()?;
        self.handoff.validate()?;

        for (name, input) in &self.inputs {
            if name != &name.to_ascii_uppercase() || !is_env_name(name) {
                return Err(ModelError::Invalid {
                    location: format!("bootstrap.inputs.{name}"),
                    message: "input name must be an uppercase environment name".to_owned(),
                });
            }
            input.validate(name)?;
        }
        for (field, input_name) in [
            (
                "run_as_root_input",
                self.identity.run_as_root_input.as_deref(),
            ),
            ("uid_input", self.identity.uid_input.as_deref()),
            ("gid_input", self.identity.gid_input.as_deref()),
            ("home_input", self.identity.home_input.as_deref()),
        ] {
            if let Some(input_name) = input_name {
                let declared = self.inputs.contains_key(input_name)
                    || self
                        .inputs
                        .values()
                        .any(|input| input.aliases.iter().any(|alias| alias == input_name));
                if !declared {
                    return Err(ModelError::Invalid {
                        location: format!("bootstrap.identity.{field}"),
                        message: format!(
                            "input {input_name:?} is not declared in bootstrap.inputs"
                        ),
                    });
                }
            }
        }

        let uid_namespace = self
            .identity
            .uid_input
            .as_deref()
            .and_then(|name| input_declaration(&self.inputs, name))
            .and_then(|input| input.namespace);
        let gid_namespace = self
            .identity
            .gid_input
            .as_deref()
            .and_then(|name| input_declaration(&self.inputs, name))
            .and_then(|input| input.namespace);
        if let (Some(uid_namespace), Some(gid_namespace)) = (uid_namespace, gid_namespace) {
            if uid_namespace != gid_namespace {
                return Err(ModelError::Invalid {
                    location: "bootstrap.identity".to_owned(),
                    message: format!(
                        "identity UID input and GID input must use the same namespace; got {uid_namespace:?} and {gid_namespace:?}"
                    ),
                });
            }
        }

        let mut ids = BTreeSet::new();
        for action in &self.actions {
            if !ids.insert(action.id.clone()) {
                return Err(ModelError::DuplicateActionId(action.id.clone()));
            }
            action.validate(self)?;
        }

        self.validate_runtime_invariance()?;

        Ok(())
    }

    fn validate_runtime_invariance(&self) -> Result<(), ModelError> {
        let runtime_inputs = self
            .inputs
            .iter()
            .filter(|(_, input)| input.runtime)
            .flat_map(|(name, input)| {
                std::iter::once(name.as_str()).chain(input.aliases.iter().map(String::as_str))
            })
            .collect::<BTreeSet<_>>();

        for action in &self.actions {
            // Workspace overlays are request-scoped target actions. The
            // backend deliberately reruns them during identity reconciliation.
            if action.origin.source.is_workspace() {
                continue;
            }
            if matches!(
                action.kind,
                ActionKind::IdentityResolve
                    | ActionKind::IdentityMapUser
                    | ActionKind::IdentityEnsureHome
                    | ActionKind::ProcessSetUserShell
                    | ActionKind::ProcessDropPrivileges
                    | ActionKind::HandoffExec
            ) {
                continue;
            }

            let fields = [
                ("path", action.path.as_deref()),
                ("link", action.link.as_deref()),
                ("target", action.target.as_deref()),
                ("mode", action.mode.as_deref()),
                ("parent_mode", action.parent_mode.as_deref()),
                ("owner", action.owner.as_deref()),
                ("user", action.user.as_deref()),
                ("shell", action.shell.as_deref()),
                ("content", action.content.as_deref()),
                ("host_key_dir", action.host_key_dir.as_deref()),
                ("authorized_keys_dir", action.authorized_keys_dir.as_deref()),
                (
                    "authorized_keys_source",
                    action.authorized_keys_source.as_deref(),
                ),
                ("runtime_dir", action.runtime_dir.as_deref()),
                ("ssh_keygen", action.ssh_keygen.as_deref()),
                ("subgroup", action.subgroup.as_deref()),
                ("shadow_path", action.shadow_path.as_deref()),
            ];
            for (field, value) in fields
                .into_iter()
                .filter_map(|(field, value)| value.map(|value| (field, value)))
            {
                if let Some(reference) = runtime_reference(value, &runtime_inputs) {
                    return Err(ModelError::Invalid {
                        location: format!("bootstrap.actions.{}.{}", action.id, field),
                        message: format!(
                            "startup action depends on {reference}, but request reconciliation does not rerun it"
                        ),
                    });
                }
            }

            if let Some(condition) = action.when.as_deref() {
                let condition = Condition::parse(condition)?;
                if let Some(reference) = condition_runtime_reference(&condition, &runtime_inputs) {
                    return Err(ModelError::Invalid {
                        location: format!("bootstrap.actions.{}.when", action.id),
                        message: format!(
                            "startup action condition depends on {reference}, but request reconciliation does not rerun it"
                        ),
                    });
                }
            }
        }
        Ok(())
    }
}

fn runtime_reference(value: &str, runtime_inputs: &BTreeSet<&str>) -> Option<String> {
    if value == "identity.target" || value.contains("identity.") {
        return Some("request identity".to_owned());
    }
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let end = after.find('}')?;
        let reference = &after[..end];
        if let Some(name) = reference.strip_prefix("input.") {
            if runtime_inputs.contains(name) {
                return Some(format!("runtime input {name}"));
            }
        } else if reference.starts_with("env.") {
            return Some(format!("runtime environment {reference}"));
        } else if reference.starts_with("identity.") {
            return Some("request identity".to_owned());
        }
        rest = &after[end + 1..];
    }
    None
}

fn condition_runtime_reference(
    condition: &Condition,
    runtime_inputs: &BTreeSet<&str>,
) -> Option<String> {
    fn value_reference(value: &ConditionValue, runtime_inputs: &BTreeSet<&str>) -> Option<String> {
        let ConditionValue::Reference(reference) = value else {
            return None;
        };
        if reference.starts_with("identity.") {
            return Some("request identity".to_owned());
        }
        if let Some(name) = reference.strip_prefix("input.") {
            if runtime_inputs.contains(name) {
                return Some(format!("runtime input {name}"));
            }
        }
        if reference.starts_with("env.") {
            return Some(format!("runtime environment {reference}"));
        }
        None
    }

    match condition {
        Condition::Equal(left, right) | Condition::NotEqual(left, right) => {
            value_reference(left, runtime_inputs).or_else(|| value_reference(right, runtime_inputs))
        }
        Condition::And(parts) | Condition::Or(parts) => parts
            .iter()
            .find_map(|part| condition_runtime_reference(part, runtime_inputs)),
        Condition::Not(part) => condition_runtime_reference(part, runtime_inputs),
        Condition::Exists(path) | Condition::Writable(path) => {
            runtime_reference(path.as_str(), runtime_inputs)
        }
        Condition::InputSet(name) => runtime_inputs
            .contains(name.as_str())
            .then(|| format!("runtime input {name}")),
        Condition::Always
        | Condition::Boolean(_)
        | Condition::ContextPathExistsOrCreate
        | Condition::Feature(_) => None,
    }
}

fn input_declaration<'a>(
    inputs: &'a BTreeMap<String, BootstrapInput>,
    name: &str,
) -> Option<&'a BootstrapInput> {
    inputs.get(name).or_else(|| {
        inputs
            .values()
            .find(|input| input.aliases.iter().any(|alias| alias == name))
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IdentityConfig {
    pub default_user: Option<String>,
    pub default_uid: Option<u32>,
    pub default_gid: Option<u32>,
    pub default_home: Option<String>,
    pub auto_mapping: bool,
    pub run_as_root_input: Option<String>,
    pub uid_input: Option<String>,
    pub gid_input: Option<String>,
    pub home_input: Option<String>,
}

impl IdentityConfig {
    fn validate(&self) -> Result<(), ModelError> {
        if let Some(user) = &self.default_user {
            validate_user_name("bootstrap.identity.default_user", user)?;
        }
        if let Some(home) = &self.default_home {
            validate_path_template("bootstrap.identity.default_home", home)?;
        }
        for (field, value) in [
            ("run_as_root_input", &self.run_as_root_input),
            ("uid_input", &self.uid_input),
            ("gid_input", &self.gid_input),
            ("home_input", &self.home_input),
        ] {
            if let Some(name) = value {
                if !is_env_name(name) {
                    return Err(ModelError::Invalid {
                        location: format!("bootstrap.identity.{field}"),
                        message: format!("{name:?} is not a valid environment input name"),
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HandoffConfig {
    pub runtime: String,
    #[serde(default)]
    pub exec_prefix: Vec<String>,
    #[serde(default)]
    pub shell_prefix: Vec<String>,
    pub ssh_daemon: Option<String>,
    pub login_shell: Option<String>,
}

impl HandoffConfig {
    fn validate(&self) -> Result<(), ModelError> {
        validate_executable("bootstrap.handoff.runtime", &self.runtime)?;
        for (index, arg) in self
            .exec_prefix
            .iter()
            .chain(self.shell_prefix.iter())
            .enumerate()
        {
            validate_argv_value(&format!("bootstrap.handoff.argv[{index}]"), arg)?;
        }
        if let Some(path) = &self.ssh_daemon {
            validate_executable("bootstrap.handoff.ssh_daemon", path)?;
        }
        if let Some(path) = &self.login_shell {
            validate_executable("bootstrap.handoff.login_shell", path)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct BootstrapPolicy {
    #[serde(default)]
    pub workspace_safe_action_kinds: BTreeSet<ActionKind>,
    #[serde(default)]
    pub admin_only_action_kinds: BTreeSet<ActionKind>,
}
