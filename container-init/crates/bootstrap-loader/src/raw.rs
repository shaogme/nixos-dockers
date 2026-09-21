use crate::error::LoaderError;
use bootstrap_model::{
    Action, ActionKind, BootstrapInput, BootstrapMode, NonInteractivePolicy, SourceKind,
};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RawProfile {
    pub(crate) schema: Option<u32>,
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) extends: Vec<String>,
    #[serde(default)]
    pub(crate) bootstrap: Option<RawBootstrap>,
    /// The environment DSL also uses this table. The loader validates only
    /// entries under the bootstrap namespace and leaves the rest untouched.
    #[serde(default, rename = "override")]
    pub(crate) overrides: BTreeMap<String, toml::Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawBootstrap {
    pub(crate) schema: Option<u32>,
    pub(crate) mode: Option<BootstrapMode>,
    pub(crate) workspace_root: Option<String>,
    pub(crate) allow_workspace_overlay: Option<bool>,
    pub(crate) non_interactive: Option<NonInteractivePolicy>,
    pub(crate) identity: Option<RawIdentity>,
    pub(crate) handoff: Option<RawHandoff>,
    pub(crate) policy: Option<RawPolicy>,
    #[serde(default)]
    pub(crate) inputs: BTreeMap<String, BootstrapInput>,
    #[serde(default)]
    pub(crate) actions: Vec<Action>,
    /// Accepted as a convenience for bootstrap-only profiles. The canonical
    /// spelling remains the top-level override table.
    #[serde(default, rename = "override")]
    pub(crate) overrides: BTreeMap<String, toml::Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawIdentity {
    pub(crate) default_user: Option<String>,
    pub(crate) default_uid: Option<u32>,
    pub(crate) default_gid: Option<u32>,
    pub(crate) default_home: Option<String>,
    pub(crate) auto_mapping: Option<bool>,
    pub(crate) run_as_root_input: Option<String>,
    pub(crate) uid_input: Option<String>,
    pub(crate) gid_input: Option<String>,
    pub(crate) home_input: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawHandoff {
    pub(crate) runtime: Option<String>,
    pub(crate) exec_prefix: Option<Vec<String>>,
    pub(crate) shell_prefix: Option<Vec<String>>,
    pub(crate) ssh_daemon: Option<String>,
    pub(crate) login_shell: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPolicy {
    #[serde(default)]
    pub(crate) workspace_safe_action_kinds: BTreeSet<ActionKind>,
    #[serde(default)]
    pub(crate) admin_only_action_kinds: BTreeSet<ActionKind>,
}

pub(crate) fn parse_profile(contents: &str, location: String) -> Result<RawProfile, LoaderError> {
    let mut document: toml::Value =
        toml::from_str(contents).map_err(|source| LoaderError::Parse {
            location: location.clone(),
            source,
        })?;
    normalize_dsl_action_kinds(&mut document);
    let profile: RawProfile = document.try_into().map_err(|source| LoaderError::Parse {
        location: location.clone(),
        source,
    })?;
    validate_profile_id(&profile.id).map_err(|error| match error {
        LoaderError::Invalid { message, .. } => LoaderError::Invalid {
            location: location.clone(),
            message,
        },
        other => other,
    })?;
    if profile.schema == Some(0) {
        return Err(LoaderError::Invalid {
            location,
            message: "profile schema must be a positive integer".to_owned(),
        });
    }
    Ok(profile)
}

/// The DSL spells action kinds with dots (`filesystem.ensure_dir`), while
/// `bootstrap-model` deliberately serializes Rust enum names as snake case.
/// Normalize only the bootstrap action-kind fields at this boundary; values
/// in the environment namespace are never interpreted or rewritten.
fn normalize_dsl_action_kinds(document: &mut toml::Value) {
    let Some(root) = document.as_table_mut() else {
        return;
    };
    let Some(bootstrap) = root
        .get_mut("bootstrap")
        .and_then(toml::Value::as_table_mut)
    else {
        return;
    };
    if let Some(actions) = bootstrap
        .get_mut("actions")
        .and_then(toml::Value::as_array_mut)
    {
        for action in actions {
            if let Some(kind) = action
                .as_table_mut()
                .and_then(|action| action.get_mut("kind"))
                .and_then(|kind| kind.as_str())
            {
                let normalized = kind.replace('.', "_");
                if let Some(kind_value) = action
                    .as_table_mut()
                    .and_then(|action| action.get_mut("kind"))
                {
                    *kind_value = toml::Value::String(normalized);
                }
            }
        }
    }
    if let Some(policy) = bootstrap
        .get_mut("policy")
        .and_then(toml::Value::as_table_mut)
    {
        for field in ["workspace_safe_action_kinds", "admin_only_action_kinds"] {
            if let Some(kinds) = policy.get_mut(field).and_then(toml::Value::as_array_mut) {
                for kind in kinds {
                    if let Some(kind_name) = kind.as_str() {
                        *kind = toml::Value::String(kind_name.replace('.', "_"));
                    }
                }
            }
        }
    }
}

pub(crate) fn validate_profile_id(id: &str) -> Result<(), LoaderError> {
    if id.is_empty()
        || !id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
    {
        return Err(LoaderError::Invalid {
            location: "id".to_owned(),
            message: format!("invalid profile id {id:?}"),
        });
    }
    Ok(())
}

pub(crate) fn validate_extends(extends: &[String], profile: &str) -> Result<(), LoaderError> {
    let mut seen = BTreeSet::new();
    for parent in extends {
        validate_profile_id(parent)?;
        if !seen.insert(parent) {
            return Err(LoaderError::Invalid {
                location: format!("profile {profile}.extends"),
                message: format!("parent profile {parent:?} is listed more than once"),
            });
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub(crate) struct OverrideCatalog {
    entries: BTreeMap<String, String>,
}

impl OverrideCatalog {
    pub(crate) fn new(profile: &RawProfile, source: &SourceKind) -> Result<Self, LoaderError> {
        let mut raw_entries = profile.overrides.clone();
        if let Some(bootstrap) = &profile.bootstrap {
            for (path, value) in &bootstrap.overrides {
                let full_path = if path.starts_with("bootstrap.") {
                    path.clone()
                } else {
                    format!("bootstrap.{path}")
                };
                if raw_entries
                    .insert(full_path.clone(), value.clone())
                    .is_some()
                {
                    return Err(LoaderError::Invalid {
                        location: format!("profile {}.override", profile.id),
                        message: format!("duplicate override declaration {full_path:?}"),
                    });
                }
            }
        }

        let mut entries = BTreeMap::new();
        for (path, value) in raw_entries {
            if !path.starts_with("bootstrap.") {
                continue;
            }
            validate_bootstrap_override_path(&path, &profile.id)?;
            if !source.is_trusted() {
                return Err(LoaderError::TrustViolation {
                    profile: profile.id.clone(),
                    source: source.clone(),
                    path,
                    message: "only image and admin profiles may declare bootstrap overrides"
                        .to_owned(),
                });
            }
            let table = value.as_table().ok_or_else(|| LoaderError::Invalid {
                location: format!("profile {}.override", profile.id),
                message: "bootstrap override must be a table".to_owned(),
            })?;
            if let Some(op) = table.get("op").and_then(toml::Value::as_str) {
                if op != "set" {
                    return Err(LoaderError::Invalid {
                        location: format!("profile {}.override.{path}", profile.id),
                        message: format!("unsupported bootstrap override operation {op:?}"),
                    });
                }
            }
            let reason = table
                .get("reason")
                .and_then(toml::Value::as_str)
                .filter(|reason| !reason.trim().is_empty())
                .ok_or_else(|| LoaderError::Invalid {
                    location: format!("profile {}.override.{path}", profile.id),
                    message: "bootstrap override requires a non-empty reason".to_owned(),
                })?;
            entries.insert(path, reason.to_owned());
        }
        Ok(Self { entries })
    }

    pub(crate) fn has_bootstrap_entries(&self) -> bool {
        !self.entries.is_empty()
    }

    pub(crate) fn contains(&self, path: &str) -> bool {
        self.entries.contains_key(path)
    }

    pub(crate) fn reason(&self, path: &str) -> Option<&str> {
        self.entries.get(path).map(String::as_str)
    }
}

fn validate_bootstrap_override_path(path: &str, profile: &str) -> Result<(), LoaderError> {
    let valid = [
        "bootstrap.schema",
        "bootstrap.mode",
        "bootstrap.workspace_root",
        "bootstrap.allow_workspace_overlay",
        "bootstrap.non_interactive",
        "bootstrap.identity.default_user",
        "bootstrap.identity.default_uid",
        "bootstrap.identity.default_gid",
        "bootstrap.identity.default_home",
        "bootstrap.identity.auto_mapping",
        "bootstrap.identity.run_as_root_input",
        "bootstrap.identity.uid_input",
        "bootstrap.identity.gid_input",
        "bootstrap.identity.home_input",
        "bootstrap.handoff.runtime",
        "bootstrap.handoff.exec_prefix",
        "bootstrap.handoff.shell_prefix",
        "bootstrap.handoff.ssh_daemon",
        "bootstrap.handoff.login_shell",
    ];
    if valid.contains(&path) {
        return Ok(());
    }
    if let Some(action_id) = path.strip_prefix("bootstrap.actions.") {
        if !action_id.is_empty()
            && action_id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
            })
        {
            return Ok(());
        }
    }
    if let Some(input_name) = path.strip_prefix("bootstrap.inputs.") {
        if !input_name.is_empty()
            && input_name.chars().all(|character| {
                character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
            })
        {
            return Ok(());
        }
    }
    Err(LoaderError::Invalid {
        location: format!("profile {profile}.override"),
        message: format!("unsupported bootstrap override path {path:?}"),
    })
}
