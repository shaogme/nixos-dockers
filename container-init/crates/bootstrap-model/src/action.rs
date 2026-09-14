use serde::{Deserialize, Serialize};

use crate::condition::Condition;
use crate::config::BootstrapConfig;
use crate::error::ModelError;
use crate::plan::PlanPhase;
use crate::provenance::Origin;
use crate::validation::{
    is_action_id, parse_mode, validate_executable, validate_owner, validate_path_template,
    validate_user_name,
};

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
    HandoffExec,
}

impl ActionKind {
    pub fn idempotency(self) -> Idempotency {
        match self {
            Self::HandoffExec => Idempotency::NonIdempotent,
            _ => Idempotency::Idempotent,
        }
    }

    fn intrinsic_admin_only(self) -> bool {
        matches!(
            self,
            Self::IdentityMapUser
                | Self::ProcessSetUserShell
                | Self::ProcessDropPrivileges
                | Self::ServiceSshPrepare
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
            _ => match run_as {
                RunAs::Root => PlanPhase::Root,
                RunAs::Current => PlanPhase::Current,
                RunAs::Target => PlanPhase::Target,
            },
        }
    }
}

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub kind: ActionKind,
    pub path: Option<String>,
    pub link: Option<String>,
    pub target: Option<String>,
    pub mode: Option<String>,
    pub parent_mode: Option<String>,
    pub owner: Option<String>,
    pub user: Option<String>,
    pub shell: Option<String>,
    pub content: Option<String>,
    /// SSH service capability fields. These keep every service path in the
    /// profile instead of introducing container-init-wide SSH defaults.
    #[serde(alias = "ssh_host_key_dir")]
    pub host_key_dir: Option<String>,
    #[serde(alias = "ssh_authorized_keys_dir")]
    pub authorized_keys_dir: Option<String>,
    #[serde(alias = "ssh_authorized_keys_source", alias = "authorized_keys_file")]
    pub authorized_keys_source: Option<String>,
    #[serde(alias = "ssh_runtime_dir")]
    pub runtime_dir: Option<String>,
    #[serde(alias = "key_types")]
    pub host_key_types: Option<Vec<String>>,
    #[serde(alias = "keygen")]
    pub ssh_keygen: Option<String>,
    pub when: Option<String>,
    #[serde(default)]
    pub failure: FailurePolicy,
    #[serde(default = "default_run_as")]
    pub run_as: RunAs,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub reason: Option<String>,
    #[serde(default)]
    pub sensitivity: Sensitivity,
    #[serde(default)]
    pub recursive: bool,
    /// Set by the profile loader; omitted from the flat Bootstrap DSL.
    #[serde(skip)]
    pub origin: Origin,
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

    pub(crate) fn validate(&self, config: &BootstrapConfig) -> Result<(), ModelError> {
        if self.id.is_empty() || !is_action_id(&self.id) {
            return Err(ModelError::Invalid {
                location: format!("bootstrap.actions[{}].id", self.id),
                message: "action id must contain only letters, digits, '.', '_' or '-' and may not be empty".to_owned(),
            });
        }

        if self.kind.intrinsic_admin_only() && !self.origin.source.is_trusted() {
            return Err(ModelError::TrustViolation {
                action: self.id.clone(),
                source: self.origin.source.clone(),
                message: "this action is restricted to an image or admin profile".to_owned(),
            });
        }
        if config.policy.admin_only_action_kinds.contains(&self.kind)
            && !self.origin.source.is_trusted()
        {
            return Err(ModelError::TrustViolation {
                action: self.id.clone(),
                source: self.origin.source.clone(),
                message: "the configured policy marks this action kind admin-only".to_owned(),
            });
        }
        if self.run_as == RunAs::Root && !self.origin.source.is_trusted() {
            return Err(ModelError::TrustViolation {
                action: self.id.clone(),
                source: self.origin.source.clone(),
                message: "root actions require an image or admin profile".to_owned(),
            });
        }
        if self.origin.source.is_workspace() {
            if !config.allow_workspace_overlay {
                return Err(ModelError::WorkspaceOverlayDisabled(self.id.clone()));
            }
            if !config
                .policy
                .workspace_safe_action_kinds
                .contains(&self.kind)
            {
                return Err(ModelError::TrustViolation {
                    action: self.id.clone(),
                    source: self.origin.source.clone(),
                    message: "workspace overlays may use only workspace-safe action kinds"
                        .to_owned(),
                });
            }
            if self.run_as != RunAs::Target {
                return Err(ModelError::TrustViolation {
                    action: self.id.clone(),
                    source: self.origin.source.clone(),
                    message: "workspace actions must run as identity.target".to_owned(),
                });
            }
        }
        if !self.origin.source.is_trusted() && !self.origin.source.is_workspace() {
            return Err(ModelError::TrustViolation {
                action: self.id.clone(),
                source: self.origin.source.clone(),
                message: "bootstrap actions may originate only from an image, admin, or explicitly allowed workspace profile".to_owned(),
            });
        }

        if matches!(
            self.kind,
            ActionKind::IdentityResolve
                | ActionKind::IdentityMapUser
                | ActionKind::IdentityEnsureHome
                | ActionKind::ServiceSshPrepare
                | ActionKind::ProcessDropPrivileges
        ) && self.run_as != RunAs::Root
        {
            return Err(ModelError::Invalid {
                location: format!("bootstrap.actions.{}.run_as", self.id),
                message: "this action kind must run as root".to_owned(),
            });
        }

        if let Some(when) = &self.when {
            Condition::parse(when)?;
        }
        for field in [
            ("path", self.path.as_deref()),
            ("link", self.link.as_deref()),
            ("target", self.target.as_deref()),
            ("host_key_dir", self.host_key_dir.as_deref()),
            ("authorized_keys_dir", self.authorized_keys_dir.as_deref()),
            (
                "authorized_keys_source",
                self.authorized_keys_source.as_deref(),
            ),
            ("runtime_dir", self.runtime_dir.as_deref()),
        ] {
            if let Some(value) = field.1 {
                validate_path_template(
                    &format!("bootstrap.actions.{}.{}", self.id, field.0),
                    value,
                )?;
            }
        }
        if let Some(mode) = &self.mode {
            parse_mode(mode).map_err(|message| ModelError::Invalid {
                location: format!("bootstrap.actions.{}.mode", self.id),
                message,
            })?;
        }
        if let Some(mode) = &self.parent_mode {
            parse_mode(mode).map_err(|message| ModelError::Invalid {
                location: format!("bootstrap.actions.{}.parent_mode", self.id),
                message,
            })?;
        }
        if let Some(shell) = &self.shell {
            validate_executable(&format!("bootstrap.actions.{}.shell", self.id), shell)?;
        }
        if let Some(keygen) = &self.ssh_keygen {
            validate_executable(&format!("bootstrap.actions.{}.ssh_keygen", self.id), keygen)?;
        }
        if let Some(user) = &self.user {
            if user != "identity.target" {
                validate_user_name(&format!("bootstrap.actions.{}.user", self.id), user)?;
            }
        }
        if let Some(owner) = &self.owner {
            validate_owner(&format!("bootstrap.actions.{}.owner", self.id), owner)?;
        }
        if let Some(content) = &self.content {
            if content.contains('\0') {
                return Err(ModelError::Invalid {
                    location: format!("bootstrap.actions.{}.content", self.id),
                    message: "content may not contain NUL".to_owned(),
                });
            }
        }
        for dependency in &self.depends_on {
            if !is_action_id(dependency) {
                return Err(ModelError::Invalid {
                    location: format!("bootstrap.actions.{}.depends_on", self.id),
                    message: format!("invalid dependency id {dependency:?}"),
                });
            }
        }

        self.validate_kind_fields()
    }

    fn validate_kind_fields(&self) -> Result<(), ModelError> {
        let require = |name: &str, value: Option<&String>| {
            if value.is_none() {
                Err(ModelError::Invalid {
                    location: format!("bootstrap.actions.{}.{}", self.id, name),
                    message: "field is required for this action kind".to_owned(),
                })
            } else {
                Ok(())
            }
        };
        match self.kind {
            ActionKind::FilesystemEnsureDir => require("path", self.path.as_ref())?,
            ActionKind::FilesystemEnsureFile => {
                require("path", self.path.as_ref())?;
                if self.content.is_none() && self.mode.is_none() {
                    return Err(ModelError::Invalid {
                        location: format!("bootstrap.actions.{}.content", self.id),
                        message: "ensure_file requires content or mode".to_owned(),
                    });
                }
            }
            ActionKind::FilesystemEnsureSymlink => {
                require("link", self.link.as_ref())?;
                require("target", self.target.as_ref())?;
            }
            ActionKind::FilesystemChown => {
                require("path", self.path.as_ref())?;
                require("owner", self.owner.as_ref())?;
            }
            ActionKind::FilesystemChmod => {
                require("path", self.path.as_ref())?;
                require("mode", self.mode.as_ref())?;
            }
            ActionKind::ProcessSetUserShell => {
                require("user", self.user.as_ref())?;
                require("shell", self.shell.as_ref())?;
            }
            ActionKind::ServiceSshPrepare => {
                // The generic path/link/target spellings are retained as a
                // compatibility convenience for early Bootstrap DSL users:
                // path = host-key dir, target = authorized-keys dir, and
                // link = runtime dir. The named fields are canonical.
                if self.host_key_dir.is_none() && self.path.is_none() {
                    require("host_key_dir", None)?;
                }
                if self.authorized_keys_dir.is_none() && self.target.is_none() {
                    require("authorized_keys_dir", None)?;
                }
                if self.runtime_dir.is_none() && self.link.is_none() {
                    require("runtime_dir", None)?;
                }
                if let Some(types) = &self.host_key_types {
                    if types.is_empty() {
                        return Err(ModelError::Invalid {
                            location: format!("bootstrap.actions.{}.host_key_types", self.id),
                            message: "at least one host key type is required".to_owned(),
                        });
                    }
                    let mut seen = std::collections::BTreeSet::new();
                    for key_type in types {
                        if !matches!(key_type.as_str(), "rsa" | "ed25519" | "ecdsa") {
                            return Err(ModelError::Invalid {
                                location: format!("bootstrap.actions.{}.host_key_types", self.id),
                                message: format!("unsupported SSH host key type {key_type:?}"),
                            });
                        }
                        if !seen.insert(key_type) {
                            return Err(ModelError::Invalid {
                                location: format!("bootstrap.actions.{}.host_key_types", self.id),
                                message: format!("duplicate SSH host key type {key_type:?}"),
                            });
                        }
                    }
                }
                if self.authorized_keys_source.is_some() && self.content.is_some() {
                    return Err(ModelError::Invalid {
                        location: format!("bootstrap.actions.{}", self.id),
                        message: "authorized_keys_source and content are mutually exclusive"
                            .to_owned(),
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }
}
