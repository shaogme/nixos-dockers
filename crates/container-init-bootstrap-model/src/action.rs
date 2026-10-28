use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{
    condition::{Condition, PathTemplate},
    error::ModelError,
    input::BootstrapInput,
    plan::PlanPhase,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunAs {
    Root,
    Target,
    Current,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePolicy {
    #[default]
    Error,
    Warn,
    Ignore,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    #[default]
    Public,
    Sensitive,
    Secret,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Idempotency {
    Idempotent,
    NonIdempotent,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    ImageProfile,
    AdminProfile,
    UserOverlay,
    WorkspaceOverlay,
    Runtime,
    Cli,
    Unknown,
}

impl SourceKind {
    pub fn is_trusted(&self) -> bool {
        matches!(self, Self::ImageProfile | Self::AdminProfile)
    }

    pub fn is_workspace(&self) -> bool {
        matches!(self, Self::WorkspaceOverlay)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Origin {
    profile: String,
    source: SourceKind,
    location: Option<String>,
}

impl Origin {
    pub fn new(profile: impl Into<String>, source: SourceKind, location: Option<String>) -> Self {
        Self {
            profile: profile.into(),
            source,
            location,
        }
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }

    pub fn set_profile(&mut self, profile: impl Into<String>) {
        self.profile = profile.into();
    }

    pub fn source(&self) -> &SourceKind {
        &self.source
    }

    pub fn set_source(&mut self, source: SourceKind) {
        self.source = source;
    }

    pub fn location(&self) -> &Option<String> {
        &self.location
    }

    pub fn set_location(&mut self, location: Option<String>) {
        self.location = location;
    }

    pub fn image(profile: impl Into<String>) -> Self {
        Self::new(profile, SourceKind::ImageProfile, None)
    }

    pub fn admin(profile: impl Into<String>) -> Self {
        Self::new(profile, SourceKind::AdminProfile, None)
    }

    pub fn workspace(profile: impl Into<String>) -> Self {
        Self::new(profile, SourceKind::WorkspaceOverlay, None)
    }

    pub fn unknown() -> Self {
        Self::new("<unknown>", SourceKind::Unknown, None)
    }
}

impl Default for Origin {
    fn default() -> Self {
        Self::unknown()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    IdentityResolve,
    IdentityMapUser,
    IdentityEnsureHome,
    FilesystemEnsureDir,
    FilesystemEnsureFile,
    FilesystemEnsureSymlink,
    FilesystemChown,
    FilesystemChmod,
    ProcessSetUserShell,
    ProcessDropPrivileges,
    ServiceSshPrepare,
    #[serde(alias = "cgroup_init")]
    CgroupV2Init,
    HandoffExec,
}

impl ActionKind {
    pub fn idempotency(self) -> Idempotency {
        match self {
            Self::HandoffExec => Idempotency::NonIdempotent,
            _ => Idempotency::Idempotent,
        }
    }

    pub(crate) fn intrinsic_admin_only(self) -> bool {
        matches!(
            self,
            Self::IdentityMapUser
                | Self::ProcessSetUserShell
                | Self::ProcessDropPrivileges
                | Self::ServiceSshPrepare
                | Self::CgroupV2Init
                | Self::HandoffExec
        )
    }

    fn phase(self, run_as: RunAs) -> PlanPhase {
        match self {
            Self::HandoffExec | Self::ProcessDropPrivileges => PlanPhase::Handoff,
            Self::IdentityResolve | Self::IdentityMapUser | Self::IdentityEnsureHome
                if run_as == RunAs::Root =>
            {
                PlanPhase::Root
            }
            Self::CgroupV2Init => PlanPhase::Root,
            _ => match run_as {
                RunAs::Root => PlanPhase::Root,
                RunAs::Current => PlanPhase::Current,
                RunAs::Target => PlanPhase::Target,
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    id: String,
    kind: ActionKind,
    path: Option<String>,
    link: Option<String>,
    target: Option<String>,
    mode: Option<String>,
    parent_mode: Option<String>,
    owner: Option<String>,
    user: Option<String>,
    shell: Option<String>,
    content: Option<String>,
    /// SSH service capability fields. These keep every service path in the
    /// profile instead of introducing container-init-wide SSH defaults.
    #[serde(alias = "ssh_host_key_dir")]
    host_key_dir: Option<String>,
    #[serde(alias = "ssh_authorized_keys_dir")]
    authorized_keys_dir: Option<String>,
    #[serde(alias = "ssh_authorized_keys_source", alias = "authorized_keys_file")]
    authorized_keys_source: Option<String>,
    #[serde(alias = "ssh_runtime_dir")]
    runtime_dir: Option<String>,
    #[serde(alias = "key_types")]
    host_key_types: Option<Vec<String>>,
    #[serde(alias = "keygen")]
    ssh_keygen: Option<String>,
    #[serde(alias = "cgroup_subgroup", alias = "init_subgroup")]
    subgroup: Option<String>,
    #[serde(
        alias = "cgroup_controllers",
        alias = "required_controllers",
        alias = "cgroup_required_controllers"
    )]
    controllers: Option<Vec<String>>,
    #[serde(alias = "cgroup_optional_controllers")]
    optional_controllers: Option<Vec<String>>,
    /// Optional explicit configuration for the action's subgroup. These are
    /// kept separate from the controllers delegated at the hierarchy root.
    subgroup_type: Option<String>,
    subgroup_controllers: Option<Vec<String>>,
    subgroup_controller_values: Option<BTreeMap<String, String>>,
    #[serde(alias = "cgroup_mount_mode")]
    mount_mode: Option<String>,
    #[serde(alias = "cgroup_shadow_path")]
    shadow_path: Option<String>,
    when: Option<String>,
    #[serde(default)]
    failure: FailurePolicy,
    #[serde(default = "default_run_as")]
    run_as: RunAs,
    #[serde(default)]
    depends_on: Vec<String>,
    reason: Option<String>,
    #[serde(default)]
    sensitivity: Sensitivity,
    #[serde(default)]
    recursive: bool,
    /// Set by the profile loader; omitted from the flat Bootstrap DSL.
    #[serde(skip)]
    origin: Origin,
}

fn default_run_as() -> RunAs {
    RunAs::Current
}

impl Action {
    pub fn new(id: impl Into<String>, kind: ActionKind, origin: Origin) -> Self {
        Self {
            id: id.into(),
            kind,
            path: None,
            link: None,
            target: None,
            mode: None,
            parent_mode: None,
            owner: None,
            user: None,
            shell: None,
            content: None,
            host_key_dir: None,
            authorized_keys_dir: None,
            authorized_keys_source: None,
            runtime_dir: None,
            host_key_types: None,
            ssh_keygen: None,
            subgroup: None,
            controllers: None,
            optional_controllers: None,
            subgroup_type: None,
            subgroup_controllers: None,
            subgroup_controller_values: None,
            mount_mode: None,
            shadow_path: None,
            when: None,
            failure: FailurePolicy::Error,
            run_as: RunAs::Current,
            depends_on: Vec::new(),
            reason: None,
            sensitivity: Sensitivity::Public,
            recursive: false,
            origin,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn set_id(&mut self, id: String) {
        self.id = id;
    }

    pub fn kind(&self) -> ActionKind {
        self.kind
    }

    pub fn set_kind(&mut self, kind: ActionKind) {
        self.kind = kind;
    }

    pub fn path(&self) -> &Option<String> {
        &self.path
    }

    pub fn set_path(&mut self, path: Option<String>) {
        self.path = path;
    }

    pub fn link(&self) -> &Option<String> {
        &self.link
    }

    pub fn set_link(&mut self, link: Option<String>) {
        self.link = link;
    }

    pub fn target(&self) -> &Option<String> {
        &self.target
    }

    pub fn set_target(&mut self, target: Option<String>) {
        self.target = target;
    }

    pub fn mode(&self) -> &Option<String> {
        &self.mode
    }

    pub fn set_mode(&mut self, mode: Option<String>) {
        self.mode = mode;
    }

    pub fn parent_mode(&self) -> &Option<String> {
        &self.parent_mode
    }

    pub fn set_parent_mode(&mut self, parent_mode: Option<String>) {
        self.parent_mode = parent_mode;
    }

    pub fn owner(&self) -> &Option<String> {
        &self.owner
    }

    pub fn set_owner(&mut self, owner: Option<String>) {
        self.owner = owner;
    }

    pub fn user(&self) -> &Option<String> {
        &self.user
    }

    pub fn set_user(&mut self, user: Option<String>) {
        self.user = user;
    }

    pub fn shell(&self) -> &Option<String> {
        &self.shell
    }

    pub fn set_shell(&mut self, shell: Option<String>) {
        self.shell = shell;
    }

    pub fn content(&self) -> &Option<String> {
        &self.content
    }

    pub fn set_content(&mut self, content: Option<String>) {
        self.content = content;
    }

    pub fn host_key_dir(&self) -> &Option<String> {
        &self.host_key_dir
    }

    pub fn set_host_key_dir(&mut self, host_key_dir: Option<String>) {
        self.host_key_dir = host_key_dir;
    }

    pub fn authorized_keys_dir(&self) -> &Option<String> {
        &self.authorized_keys_dir
    }

    pub fn set_authorized_keys_dir(&mut self, authorized_keys_dir: Option<String>) {
        self.authorized_keys_dir = authorized_keys_dir;
    }

    pub fn authorized_keys_source(&self) -> &Option<String> {
        &self.authorized_keys_source
    }

    pub fn set_authorized_keys_source(&mut self, authorized_keys_source: Option<String>) {
        self.authorized_keys_source = authorized_keys_source;
    }

    pub fn runtime_dir(&self) -> &Option<String> {
        &self.runtime_dir
    }

    pub fn set_runtime_dir(&mut self, runtime_dir: Option<String>) {
        self.runtime_dir = runtime_dir;
    }

    pub fn host_key_types(&self) -> &Option<Vec<String>> {
        &self.host_key_types
    }

    pub fn set_host_key_types(&mut self, host_key_types: Option<Vec<String>>) {
        self.host_key_types = host_key_types;
    }

    pub fn ssh_keygen(&self) -> &Option<String> {
        &self.ssh_keygen
    }

    pub fn set_ssh_keygen(&mut self, ssh_keygen: Option<String>) {
        self.ssh_keygen = ssh_keygen;
    }

    pub fn subgroup(&self) -> &Option<String> {
        &self.subgroup
    }

    pub fn set_subgroup(&mut self, subgroup: Option<String>) {
        self.subgroup = subgroup;
    }

    pub fn controllers(&self) -> &Option<Vec<String>> {
        &self.controllers
    }

    pub fn set_controllers(&mut self, controllers: Option<Vec<String>>) {
        self.controllers = controllers;
    }

    pub fn optional_controllers(&self) -> &Option<Vec<String>> {
        &self.optional_controllers
    }

    pub fn set_optional_controllers(&mut self, optional_controllers: Option<Vec<String>>) {
        self.optional_controllers = optional_controllers;
    }

    pub fn subgroup_type(&self) -> &Option<String> {
        &self.subgroup_type
    }

    pub fn set_subgroup_type(&mut self, subgroup_type: Option<String>) {
        self.subgroup_type = subgroup_type;
    }

    pub fn subgroup_controllers(&self) -> &Option<Vec<String>> {
        &self.subgroup_controllers
    }

    pub fn set_subgroup_controllers(&mut self, subgroup_controllers: Option<Vec<String>>) {
        self.subgroup_controllers = subgroup_controllers;
    }

    pub fn subgroup_controller_values(&self) -> &Option<BTreeMap<String, String>> {
        &self.subgroup_controller_values
    }

    pub fn set_subgroup_controller_values(
        &mut self,
        subgroup_controller_values: Option<BTreeMap<String, String>>,
    ) {
        self.subgroup_controller_values = subgroup_controller_values;
    }

    pub fn mount_mode(&self) -> &Option<String> {
        &self.mount_mode
    }

    pub fn set_mount_mode(&mut self, mount_mode: Option<String>) {
        self.mount_mode = mount_mode;
    }

    pub fn shadow_path(&self) -> &Option<String> {
        &self.shadow_path
    }

    pub fn set_shadow_path(&mut self, shadow_path: Option<String>) {
        self.shadow_path = shadow_path;
    }

    pub fn when(&self) -> &Option<String> {
        &self.when
    }

    pub fn set_when(&mut self, when: Option<String>) {
        self.when = when;
    }

    pub fn failure(&self) -> FailurePolicy {
        self.failure
    }

    pub fn set_failure(&mut self, failure: FailurePolicy) {
        self.failure = failure;
    }

    pub fn run_as(&self) -> RunAs {
        self.run_as
    }

    pub fn set_run_as(&mut self, run_as: RunAs) {
        self.run_as = run_as;
    }

    pub fn depends_on(&self) -> &[String] {
        &self.depends_on
    }

    pub fn set_depends_on(&mut self, depends_on: Vec<String>) {
        self.depends_on = depends_on;
    }

    pub fn reason(&self) -> &Option<String> {
        &self.reason
    }

    pub fn set_reason(&mut self, reason: Option<String>) {
        self.reason = reason;
    }

    pub fn sensitivity(&self) -> Sensitivity {
        self.sensitivity
    }

    pub fn set_sensitivity(&mut self, sensitivity: Sensitivity) {
        self.sensitivity = sensitivity;
    }

    pub fn recursive(&self) -> bool {
        self.recursive
    }

    pub fn set_recursive(&mut self, recursive: bool) {
        self.recursive = recursive;
    }

    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    pub fn set_origin(&mut self, origin: Origin) {
        self.origin = origin;
    }

    pub fn references_identity(&self) -> bool {
        self.kind != ActionKind::IdentityResolve
            && [
                self.path.as_deref(),
                self.link.as_deref(),
                self.target.as_deref(),
                self.owner.as_deref(),
                self.user.as_deref(),
                self.shell.as_deref(),
                self.host_key_dir.as_deref(),
                self.authorized_keys_dir.as_deref(),
                self.authorized_keys_source.as_deref(),
                self.runtime_dir.as_deref(),
                self.subgroup.as_deref(),
                self.shadow_path.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|value| value.contains("${identity."))
            || matches!(self.owner.as_deref(), Some("identity.target"))
            || matches!(self.user.as_deref(), Some("identity.target"))
            || matches!(
                self.kind,
                ActionKind::IdentityMapUser
                    | ActionKind::IdentityEnsureHome
                    | ActionKind::ServiceSshPrepare
                    | ActionKind::ProcessDropPrivileges
            )
    }

    pub fn phase(&self) -> PlanPhase {
        self.kind.phase(self.run_as)
    }

    pub fn condition(&self) -> Result<Condition, ModelError> {
        self.when
            .as_deref()
            .map(Condition::parse)
            .unwrap_or(Ok(Condition::Always))
    }

    pub fn ssh_host_key_dir(&self) -> Option<&str> {
        self.host_key_dir.as_deref().or(self.path.as_deref())
    }

    pub fn ssh_authorized_keys_dir(&self) -> Option<&str> {
        self.authorized_keys_dir
            .as_deref()
            .or(self.target.as_deref())
    }

    pub fn ssh_runtime_dir(&self) -> Option<&str> {
        self.runtime_dir.as_deref().or(self.link.as_deref())
    }

    pub fn ssh_host_key_types(&self) -> &[String] {
        self.host_key_types.as_deref().unwrap_or(&[])
    }

    pub fn cgroup_mount_mode(&self) -> &str {
        match self.mount_mode.as_deref().or(self.mode.as_deref()) {
            Some("bind_mount" | "mount_shadow" | "shadow") => "bind_mount",
            _ => "default",
        }
    }

    pub(crate) fn is_action_id(value: &str) -> bool {
        !value.is_empty()
            && value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
            })
    }

    pub(crate) fn parse_mode(value: &str) -> Result<u32, String> {
        if value.is_empty()
            || value.len() > 4
            || !value
                .chars()
                .all(|character| ('0'..='7').contains(&character))
        {
            return Err("mode must be an octal string containing at most four digits".to_owned());
        }
        u32::from_str_radix(value, 8).map_err(|_| "mode is outside the supported range".to_owned())
    }

    pub(crate) fn validate_executable(location: &str, value: &str) -> Result<(), ModelError> {
        Self::validate_argv_value(location, value)?;
        if value.contains("${") || value.chars().any(char::is_whitespace) {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "executable path must be a concrete single path".to_owned(),
            });
        }
        PathTemplate::validate_template(location, value)?;
        if !value.starts_with('/') {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "executable path must be absolute".to_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn validate_argv_value(location: &str, value: &str) -> Result<(), ModelError> {
        if value.is_empty() || value.contains('\0') || value.contains('\n') || value.contains('\r')
        {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "argv value must be non-empty and contain no NUL/newline".to_owned(),
            });
        }
        if value.contains("$(") || value.contains('`') || value.contains(';') || value.contains('|')
        {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "shell expressions are not allowed in argv values".to_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn validate_user_name(location: &str, value: &str) -> Result<(), ModelError> {
        if value.is_empty()
            || value.len() > 32
            || !value.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
        {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: format!("invalid POSIX user name {value:?}"),
            });
        }
        Ok(())
    }

    pub(crate) fn validate_owner(location: &str, value: &str) -> Result<(), ModelError> {
        if value == "root" || value == "identity.target" {
            return Ok(());
        }
        let Some((uid, gid)) = value.split_once(':') else {
            return Err(ModelError::Invalid {
                location: location.to_owned(),
                message: "owner must be root, identity.target, or uid:gid".to_owned(),
            });
        };
        BootstrapInput::parse_id("uid", uid).map_err(|_| ModelError::Invalid {
            location: location.to_owned(),
            message: "owner uid:gid contains an invalid uid".to_owned(),
        })?;
        BootstrapInput::parse_id("gid", gid).map_err(|_| ModelError::Invalid {
            location: location.to_owned(),
            message: "owner uid:gid contains an invalid gid".to_owned(),
        })?;
        Ok(())
    }
}
