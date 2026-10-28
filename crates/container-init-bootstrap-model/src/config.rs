mod handoff;
mod identity;

pub use handoff::{
    HandoffConfig, MAX_HANDOFF_PREFIX_ITEMS, MAX_HANDOFF_PREFIX_ITEM_BYTES,
    MAX_HANDOFF_PREFIX_TOTAL_BYTES,
};
pub use identity::IdentityConfig;

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    iter::once,
};

use crate::{
    action::{Action, ActionKind, RunAs},
    condition::{Condition, ConditionValue, PathTemplate},
    error::ModelError,
    input::BootstrapInput,
    plan::Plan,
    BOOTSTRAP_SCHEMA_V1,
};

pub const MAX_BOOTSTRAP_ACTIONS: usize = 8_192;

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

#[derive(Clone, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct BootstrapPolicy {
    #[serde(default)]
    pub workspace_safe_action_kinds: BTreeSet<ActionKind>,
    #[serde(default)]
    pub admin_only_action_kinds: BTreeSet<ActionKind>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BootstrapConfig {
    schema: u32,
    mode: BootstrapMode,
    workspace_root: String,
    allow_workspace_overlay: bool,
    non_interactive: NonInteractivePolicy,
    identity: IdentityConfig,
    handoff: HandoffConfig,
    policy: BootstrapPolicy,
    #[serde(default)]
    inputs: BTreeMap<String, BootstrapInput>,
    #[serde(default)]
    actions: Vec<Action>,
}

impl BootstrapConfig {
    pub fn new(
        workspace_root: impl Into<String>,
        identity: IdentityConfig,
        handoff: HandoffConfig,
        inputs: BTreeMap<String, BootstrapInput>,
        actions: Vec<Action>,
    ) -> Self {
        Self {
            schema: BOOTSTRAP_SCHEMA_V1,
            mode: BootstrapMode::Strict,
            workspace_root: workspace_root.into(),
            allow_workspace_overlay: false,
            non_interactive: NonInteractivePolicy::Deny,
            identity,
            handoff,
            policy: BootstrapPolicy::default(),
            inputs,
            actions,
        }
    }

    pub fn schema(&self) -> u32 {
        self.schema
    }

    pub fn set_schema(&mut self, schema: u32) {
        self.schema = schema;
    }

    pub fn mode(&self) -> &BootstrapMode {
        &self.mode
    }

    pub fn set_mode(&mut self, mode: BootstrapMode) {
        self.mode = mode;
    }

    pub fn workspace_root(&self) -> &str {
        &self.workspace_root
    }

    pub fn set_workspace_root(&mut self, workspace_root: impl Into<String>) {
        self.workspace_root = workspace_root.into();
    }

    pub fn allow_workspace_overlay(&self) -> bool {
        self.allow_workspace_overlay
    }

    pub fn set_allow_workspace_overlay(&mut self, allow_workspace_overlay: bool) {
        self.allow_workspace_overlay = allow_workspace_overlay;
    }

    pub fn non_interactive(&self) -> &NonInteractivePolicy {
        &self.non_interactive
    }

    pub fn set_non_interactive(&mut self, non_interactive: NonInteractivePolicy) {
        self.non_interactive = non_interactive;
    }

    pub fn identity(&self) -> &IdentityConfig {
        &self.identity
    }

    pub fn identity_mut(&mut self) -> &mut IdentityConfig {
        &mut self.identity
    }

    pub fn set_identity(&mut self, identity: IdentityConfig) {
        self.identity = identity;
    }

    pub fn handoff(&self) -> &HandoffConfig {
        &self.handoff
    }

    pub fn handoff_mut(&mut self) -> &mut HandoffConfig {
        &mut self.handoff
    }

    pub fn set_handoff(&mut self, handoff: HandoffConfig) {
        self.handoff = handoff;
    }

    pub fn policy(&self) -> &BootstrapPolicy {
        &self.policy
    }

    pub fn policy_mut(&mut self) -> &mut BootstrapPolicy {
        &mut self.policy
    }

    pub fn set_policy(&mut self, policy: BootstrapPolicy) {
        self.policy = policy;
    }

    pub fn inputs(&self) -> &BTreeMap<String, BootstrapInput> {
        &self.inputs
    }

    pub fn inputs_mut(&mut self) -> &mut BTreeMap<String, BootstrapInput> {
        &mut self.inputs
    }

    pub fn set_inputs(&mut self, inputs: BTreeMap<String, BootstrapInput>) {
        self.inputs = inputs;
    }

    pub fn actions(&self) -> &[Action] {
        &self.actions
    }

    pub fn actions_mut(&mut self) -> &mut Vec<Action> {
        &mut self.actions
    }

    pub fn set_actions(&mut self, actions: Vec<Action>) {
        self.actions = actions;
    }

    /// Validate the model without performing any side effects.
    pub fn build_plan(&self) -> Result<Plan, ModelError> {
        Plan::from_config(self)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if self.actions.len() > MAX_BOOTSTRAP_ACTIONS {
            return Err(ModelError::Invalid {
                location: "bootstrap.actions".to_owned(),
                message: format!(
                    "contains {} actions; maximum is {MAX_BOOTSTRAP_ACTIONS}",
                    self.actions.len()
                ),
            });
        }
        if self.schema != BOOTSTRAP_SCHEMA_V1 {
            return Err(ModelError::Invalid {
                location: "bootstrap.schema".to_owned(),
                message: format!(
                    "unsupported bootstrap schema {}; expected {}",
                    self.schema, BOOTSTRAP_SCHEMA_V1
                ),
            });
        }

        PathTemplate::validate_template("bootstrap.workspace_root", &self.workspace_root)?;
        if self.workspace_root.starts_with("${") {
            return Err(ModelError::Invalid {
                location: "bootstrap.workspace_root".to_owned(),
                message: "workspace_root must be a concrete absolute path".to_owned(),
            });
        }

        self.identity.validate()?;
        self.handoff.validate()?;

        for (name, input) in &self.inputs {
            if name != &name.to_ascii_uppercase() || !BootstrapInput::is_env_name(name) {
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
                self.identity.run_as_root_input().as_deref(),
            ),
            ("uid_input", self.identity.uid_input().as_deref()),
            ("gid_input", self.identity.gid_input().as_deref()),
            ("home_input", self.identity.home_input().as_deref()),
        ] {
            if let Some(input_name) = input_name {
                let declared = self.inputs.contains_key(input_name)
                    || self
                        .inputs
                        .values()
                        .any(|input| input.aliases().iter().any(|alias| alias == input_name));
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
            .uid_input()
            .as_deref()
            .and_then(|name| input_declaration(&self.inputs, name))
            .and_then(BootstrapInput::namespace);
        let gid_namespace = self
            .identity
            .gid_input()
            .as_deref()
            .and_then(|name| input_declaration(&self.inputs, name))
            .and_then(BootstrapInput::namespace);
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
            if !ids.insert(action.id().to_owned()) {
                return Err(ModelError::DuplicateActionId(action.id().to_owned()));
            }
            self.validate_action(action)?;
        }

        self.validate_runtime_invariance()?;

        Ok(())
    }

    fn validate_runtime_invariance(&self) -> Result<(), ModelError> {
        let runtime_inputs = self
            .inputs
            .iter()
            .filter(|(_, input)| input.runtime())
            .flat_map(|(name, input)| {
                once(name.as_str()).chain(input.aliases().iter().map(String::as_str))
            })
            .collect::<BTreeSet<_>>();

        for action in &self.actions {
            // Workspace overlays are request-scoped target actions. The
            // backend deliberately reruns them during identity reconciliation.
            if action.origin().source().is_workspace() {
                continue;
            }
            if matches!(
                action.kind(),
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
                ("path", action.path().as_deref()),
                ("link", action.link().as_deref()),
                ("target", action.target().as_deref()),
                ("mode", action.mode().as_deref()),
                ("parent_mode", action.parent_mode().as_deref()),
                ("owner", action.owner().as_deref()),
                ("user", action.user().as_deref()),
                ("shell", action.shell().as_deref()),
                ("content", action.content().as_deref()),
                ("host_key_dir", action.host_key_dir().as_deref()),
                (
                    "authorized_keys_dir",
                    action.authorized_keys_dir().as_deref(),
                ),
                (
                    "authorized_keys_source",
                    action.authorized_keys_source().as_deref(),
                ),
                ("runtime_dir", action.runtime_dir().as_deref()),
                ("ssh_keygen", action.ssh_keygen().as_deref()),
                ("subgroup", action.subgroup().as_deref()),
                ("shadow_path", action.shadow_path().as_deref()),
            ];
            for (field, value) in fields
                .into_iter()
                .filter_map(|(field, value)| value.map(|value| (field, value)))
            {
                if let Some(reference) = runtime_reference(value, &runtime_inputs) {
                    return Err(ModelError::Invalid {
                        location: format!("bootstrap.actions.{}.{}", action.id(), field),
                        message: format!(
                            "startup action depends on {reference}, but request reconciliation does not rerun it"
                        ),
                    });
                }
            }

            if let Some(condition) = action.when().as_deref() {
                let condition = Condition::parse(condition)?;
                if let Some(reference) = condition_runtime_reference(&condition, &runtime_inputs) {
                    return Err(ModelError::Invalid {
                        location: format!("bootstrap.actions.{}.when", action.id()),
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

impl BootstrapConfig {
    fn validate_action(&self, action: &Action) -> Result<(), ModelError> {
        if action.id().is_empty() || !Action::is_action_id(action.id()) {
            return Err(ModelError::Invalid {
                location: format!("bootstrap.actions[{}].id", action.id()),
                message: "action id must contain only letters, digits, '.', '_' or '-' and may not be empty".to_owned(),
            });
        }

        if action.kind().intrinsic_admin_only() && !action.origin().source().is_trusted() {
            return Err(ModelError::TrustViolation {
                action: action.id().to_owned(),
                source: action.origin().source().clone(),
                message: "this action is restricted to an image or admin profile".to_owned(),
            });
        }
        if self
            .policy()
            .admin_only_action_kinds
            .contains(&action.kind())
            && !action.origin().source().is_trusted()
        {
            return Err(ModelError::TrustViolation {
                action: action.id().to_owned(),
                source: action.origin().source().clone(),
                message: "the configured policy marks this action kind admin-only".to_owned(),
            });
        }
        if action.run_as() == RunAs::Root && !action.origin().source().is_trusted() {
            return Err(ModelError::TrustViolation {
                action: action.id().to_owned(),
                source: action.origin().source().clone(),
                message: "root actions require an image or admin profile".to_owned(),
            });
        }
        if action.origin().source().is_workspace() {
            if !self.allow_workspace_overlay() {
                return Err(ModelError::WorkspaceOverlayDisabled(action.id().to_owned()));
            }
            if !self
                .policy()
                .workspace_safe_action_kinds
                .contains(&action.kind())
            {
                return Err(ModelError::TrustViolation {
                    action: action.id().to_owned(),
                    source: action.origin().source().clone(),
                    message: "workspace overlays may use only workspace-safe action kinds"
                        .to_owned(),
                });
            }
            if action.run_as() != RunAs::Target {
                return Err(ModelError::TrustViolation {
                    action: action.id().to_owned(),
                    source: action.origin().source().clone(),
                    message: "workspace actions must run as identity.target".to_owned(),
                });
            }
        }
        if !action.origin().source().is_trusted() && !action.origin().source().is_workspace() {
            return Err(ModelError::TrustViolation {
                action: action.id().to_owned(),
                source: action.origin().source().clone(),
                message: "bootstrap actions may originate only from an image, admin, or explicitly allowed workspace profile".to_owned(),
            });
        }

        if matches!(
            action.kind(),
            ActionKind::IdentityResolve
                | ActionKind::IdentityMapUser
                | ActionKind::IdentityEnsureHome
                | ActionKind::ServiceSshPrepare
                | ActionKind::ProcessDropPrivileges
                | ActionKind::CgroupV2Init
        ) && action.run_as() != RunAs::Root
        {
            return Err(ModelError::Invalid {
                location: format!("bootstrap.actions.{}.run_as", action.id()),
                message: "this action kind must run as root".to_owned(),
            });
        }

        if let Some(when) = action.when() {
            Condition::parse(when)?;
        }
        for field in [
            ("path", action.path().as_deref()),
            ("link", action.link().as_deref()),
            ("target", action.target().as_deref()),
            ("host_key_dir", action.host_key_dir().as_deref()),
            (
                "authorized_keys_dir",
                action.authorized_keys_dir().as_deref(),
            ),
            (
                "authorized_keys_source",
                action.authorized_keys_source().as_deref(),
            ),
            ("runtime_dir", action.runtime_dir().as_deref()),
            ("shadow_path", action.shadow_path().as_deref()),
        ] {
            if let Some(value) = field.1 {
                PathTemplate::validate_template(
                    &format!("bootstrap.actions.{}.{}", action.id(), field.0),
                    value,
                )?;
            }
        }
        if action.kind() != ActionKind::CgroupV2Init {
            if let Some(mode) = action.mode() {
                Action::parse_mode(mode).map_err(|message| ModelError::Invalid {
                    location: format!("bootstrap.actions.{}.mode", action.id()),
                    message,
                })?;
            }
        }
        if let Some(mode) = action.parent_mode() {
            Action::parse_mode(mode).map_err(|message| ModelError::Invalid {
                location: format!("bootstrap.actions.{}.parent_mode", action.id()),
                message,
            })?;
        }
        if let Some(shell) = action.shell() {
            Action::validate_executable(
                &format!("bootstrap.actions.{}.shell", action.id()),
                shell,
            )?;
        }
        if let Some(keygen) = action.ssh_keygen() {
            Action::validate_executable(
                &format!("bootstrap.actions.{}.ssh_keygen", action.id()),
                keygen,
            )?;
        }
        if let Some(user) = action.user() {
            if user != "identity.target" {
                Action::validate_user_name(
                    &format!("bootstrap.actions.{}.user", action.id()),
                    user,
                )?;
            }
        }
        if let Some(owner) = action.owner() {
            Action::validate_owner(&format!("bootstrap.actions.{}.owner", action.id()), owner)?;
        }
        if let Some(content) = action.content() {
            if content.contains('\0') {
                return Err(ModelError::Invalid {
                    location: format!("bootstrap.actions.{}.content", action.id()),
                    message: "content may not contain NUL".to_owned(),
                });
            }
        }
        for dependency in action.depends_on() {
            if !Action::is_action_id(dependency) {
                return Err(ModelError::Invalid {
                    location: format!("bootstrap.actions.{}.depends_on", action.id()),
                    message: format!("invalid dependency id {dependency:?}"),
                });
            }
        }

        Self::validate_action_kind_fields(action)
    }

    fn validate_action_kind_fields(action: &Action) -> Result<(), ModelError> {
        let require = |name: &str, value: Option<&String>| {
            if value.is_none() {
                Err(ModelError::Invalid {
                    location: format!("bootstrap.actions.{}.{}", action.id(), name),
                    message: "field is required for this action kind".to_owned(),
                })
            } else {
                Ok(())
            }
        };
        match action.kind() {
            ActionKind::FilesystemEnsureDir => require("path", action.path().as_ref())?,
            ActionKind::FilesystemEnsureFile => {
                require("path", action.path().as_ref())?;
                if action.content().is_none() && action.mode().is_none() {
                    return Err(ModelError::Invalid {
                        location: format!("bootstrap.actions.{}.content", action.id()),
                        message: "ensure_file requires content or mode".to_owned(),
                    });
                }
            }
            ActionKind::FilesystemEnsureSymlink => {
                require("link", action.link().as_ref())?;
                require("target", action.target().as_ref())?;
            }
            ActionKind::FilesystemChown => {
                require("path", action.path().as_ref())?;
                require("owner", action.owner().as_ref())?;
            }
            ActionKind::FilesystemChmod => {
                require("path", action.path().as_ref())?;
                require("mode", action.mode().as_ref())?;
            }
            ActionKind::ProcessSetUserShell => {
                require("user", action.user().as_ref())?;
                require("shell", action.shell().as_ref())?;
            }
            ActionKind::ServiceSshPrepare => {
                // The generic path/link/target spellings are retained as a
                // compatibility convenience for early Bootstrap DSL users:
                // path = host-key dir, target = authorized-keys dir, and
                // link = runtime dir. The named fields are canonical.
                if action.host_key_dir().is_none() && action.path().is_none() {
                    require("host_key_dir", None)?;
                }
                if action.authorized_keys_dir().is_none() && action.target().is_none() {
                    require("authorized_keys_dir", None)?;
                }
                if action.runtime_dir().is_none() && action.link().is_none() {
                    require("runtime_dir", None)?;
                }
                if let Some(types) = &action.host_key_types() {
                    if types.is_empty() {
                        return Err(ModelError::Invalid {
                            location: format!("bootstrap.actions.{}.host_key_types", action.id()),
                            message: "at least one host key type is required".to_owned(),
                        });
                    }
                    let mut seen = BTreeSet::new();
                    for key_type in types {
                        if !matches!(key_type.as_str(), "rsa" | "ed25519" | "ecdsa") {
                            return Err(ModelError::Invalid {
                                location: format!(
                                    "bootstrap.actions.{}.host_key_types",
                                    action.id()
                                ),
                                message: format!("unsupported SSH host key type {key_type:?}"),
                            });
                        }
                        if !seen.insert(key_type) {
                            return Err(ModelError::Invalid {
                                location: format!(
                                    "bootstrap.actions.{}.host_key_types",
                                    action.id()
                                ),
                                message: format!("duplicate SSH host key type {key_type:?}"),
                            });
                        }
                    }
                }
                if action.authorized_keys_source().is_some() && action.content().is_some() {
                    return Err(ModelError::Invalid {
                        location: format!("bootstrap.actions.{}", action.id()),
                        message: "authorized_keys_source and content are mutually exclusive"
                            .to_owned(),
                    });
                }
            }
            ActionKind::CgroupV2Init => {
                let resolved_mount_mode =
                    action.mount_mode().as_deref().or(action.mode().as_deref());
                if let Some(mode_str) = resolved_mount_mode {
                    match mode_str {
                        "default" | "direct" | "bind_mount" | "mount_shadow" | "shadow" => {}
                        _ => {
                            return Err(ModelError::Invalid {
                                location: format!("bootstrap.actions.{}.mount_mode", action.id()),
                                message: format!(
                                    "invalid mount_mode {mode_str:?}; expected 'default' or 'bind_mount'"
                                ),
                            });
                        }
                    }
                }
                if let Some(subgroup) = &action.subgroup() {
                    if subgroup.is_empty()
                        || subgroup.contains('/')
                        || subgroup.contains("..")
                        || subgroup.contains('\0')
                    {
                        return Err(ModelError::Invalid {
                            location: format!("bootstrap.actions.{}.subgroup", action.id()),
                            message: "subgroup must not be empty or contain '/', '..', or NUL"
                                .to_owned(),
                        });
                    }
                }
                let mut seen = BTreeSet::new();
                for (field, controllers) in [
                    ("controllers", action.controllers().as_ref()),
                    (
                        "optional_controllers",
                        action.optional_controllers().as_ref(),
                    ),
                ] {
                    let Some(controllers) = controllers else {
                        continue;
                    };
                    if controllers.is_empty() {
                        return Err(ModelError::Invalid {
                            location: format!("bootstrap.actions.{}.{}", action.id(), field),
                            message: "controllers list may not be empty if specified".to_owned(),
                        });
                    }
                    for controller in controllers {
                        if controller.is_empty()
                            || controller.contains(char::is_whitespace)
                            || controller.contains('\0')
                        {
                            return Err(ModelError::Invalid {
                                location: format!("bootstrap.actions.{}.{}", action.id(), field),
                                message: format!("invalid controller name {controller:?}"),
                            });
                        }
                        if !seen.insert(controller) {
                            return Err(ModelError::Invalid {
                                location: format!("bootstrap.actions.{}.{}", action.id(), field),
                                message: format!("duplicate controller name {controller:?}"),
                            });
                        }
                    }
                }
                if let Some(subgroup_type) = &action.subgroup_type() {
                    if action.subgroup().is_none()
                        || subgroup_type.is_empty()
                        || subgroup_type.contains(char::is_whitespace)
                        || subgroup_type.contains('\0')
                    {
                        return Err(ModelError::Invalid {
                            location: format!(
                                "bootstrap.actions.{}.subgroup_type",
                                action.id()
                            ),
                            message: "subgroup_type requires a subgroup and must be a single non-empty token".to_owned(),
                        });
                    }
                }
                if let Some(controllers) = &action.subgroup_controllers() {
                    if action.subgroup().is_none() || controllers.is_empty() {
                        return Err(ModelError::Invalid {
                            location: format!(
                                "bootstrap.actions.{}.subgroup_controllers",
                                action.id()
                            ),
                            message: "subgroup_controllers requires a non-empty subgroup list"
                                .to_owned(),
                        });
                    }
                    let mut seen = BTreeSet::new();
                    for controller in controllers {
                        if controller.is_empty()
                            || controller.contains(char::is_whitespace)
                            || controller.contains('\0')
                            || !seen.insert(controller)
                        {
                            return Err(ModelError::Invalid {
                                location: format!(
                                    "bootstrap.actions.{}.subgroup_controllers",
                                    action.id()
                                ),
                                message: format!(
                                    "invalid or duplicate subgroup controller {controller:?}"
                                ),
                            });
                        }
                    }
                }
                if let Some(values) = &action.subgroup_controller_values() {
                    if action.subgroup().is_none() || values.is_empty() {
                        return Err(ModelError::Invalid {
                            location: format!(
                                "bootstrap.actions.{}.subgroup_controller_values",
                                action.id()
                            ),
                            message: "subgroup_controller_values requires a non-empty subgroup"
                                .to_owned(),
                        });
                    }
                    for (target, source) in values {
                        for (field, value) in [("target", target), ("source", source)] {
                            if value.is_empty()
                                || value.starts_with('/')
                                || value.contains('/')
                                || value.contains("..")
                                || value.contains('\0')
                            {
                                return Err(ModelError::Invalid {
                                    location: format!(
                                        "bootstrap.actions.{}.subgroup_controller_values.{}",
                                        action.id(), field
                                    ),
                                    message: format!(
                                        "{field} must be a single relative cgroup file name, got {value:?}"
                                    ),
                                });
                            }
                        }
                    }
                }
            }
            _ => {}
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
            .find(|input| input.aliases().iter().any(|alias| alias == name))
    })
}
