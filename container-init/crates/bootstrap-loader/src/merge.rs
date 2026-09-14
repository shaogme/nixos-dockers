use crate::error::LoaderError;
use crate::raw::{OverrideCatalog, RawBootstrap, RawProfile};
use crate::source::{LoadedProfile, ProfileSource};
use bootstrap_model::{
    Action, BootstrapConfig, BootstrapMode, BootstrapPolicy, HandoffConfig, IdentityConfig,
    NonInteractivePolicy, Origin, SourceKind,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default)]
pub(crate) struct MergedBootstrap {
    schema: Option<u32>,
    mode: Option<BootstrapMode>,
    workspace_root: Option<String>,
    allow_workspace_overlay: Option<bool>,
    non_interactive: Option<NonInteractivePolicy>,
    identity: MergedIdentity,
    handoff: MergedHandoff,
    policy: BootstrapPolicy,
    inputs: BTreeMap<String, bootstrap_model::BootstrapInput>,
    input_origins: BTreeMap<String, Origin>,
    actions: Vec<Action>,
    action_indexes: BTreeMap<String, usize>,
    field_origins: BTreeMap<String, Origin>,
    saw_bootstrap: bool,
}

#[derive(Clone, Debug, Default)]
struct MergedIdentity {
    default_user: Option<String>,
    default_uid: Option<u32>,
    default_gid: Option<u32>,
    auto_mapping: Option<bool>,
    run_as_root_input: Option<String>,
    uid_input: Option<String>,
    gid_input: Option<String>,
    home_input: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct MergedHandoff {
    runtime: Option<String>,
    exec_prefix: Option<Vec<String>>,
    shell_prefix: Option<Vec<String>>,
    ssh_daemon: Option<String>,
    login_shell: Option<String>,
}

impl MergedBootstrap {
    pub(crate) fn merge_profile(
        &mut self,
        profile: &RawProfile,
        source: &ProfileSource,
        profile_info: &LoadedProfile,
    ) -> Result<(), LoaderError> {
        let overrides = OverrideCatalog::new(profile, &source.source)?;
        let Some(bootstrap) = profile.bootstrap.as_ref() else {
            if overrides.has_bootstrap_entries() {
                return Err(LoaderError::Invalid {
                    location: format!("profile {}.override", profile.id),
                    message: "bootstrap overrides require a bootstrap namespace".to_owned(),
                });
            }
            return Ok(());
        };
        self.saw_bootstrap = true;

        let origin = profile_origin(profile_info, source, "bootstrap");
        if !source.source.is_trusted() {
            if source.source != SourceKind::WorkspaceOverlay {
                return Err(LoaderError::TrustViolation {
                    profile: profile.id.clone(),
                    source: source.source.clone(),
                    path: "bootstrap".to_owned(),
                    message: "only workspace overlays may contribute bootstrap actions".to_owned(),
                });
            }
            reject_untrusted_bootstrap_fields(bootstrap, source)?;
            if overrides.has_bootstrap_entries() {
                return Err(LoaderError::TrustViolation {
                    profile: profile.id.clone(),
                    source: source.source.clone(),
                    path: "override".to_owned(),
                    message: "untrusted profiles may not override bootstrap declarations"
                        .to_owned(),
                });
            }
        }

        let profile_schema = bootstrap.schema.or(profile.schema);
        merge_scalar(
            &mut self.schema,
            "bootstrap.schema",
            profile_schema,
            &origin,
            &overrides,
            &mut self.field_origins,
        )?;
        merge_scalar(
            &mut self.mode,
            "bootstrap.mode",
            bootstrap.mode.clone(),
            &origin,
            &overrides,
            &mut self.field_origins,
        )?;
        merge_scalar(
            &mut self.workspace_root,
            "bootstrap.workspace_root",
            bootstrap.workspace_root.clone(),
            &origin,
            &overrides,
            &mut self.field_origins,
        )?;
        merge_scalar(
            &mut self.allow_workspace_overlay,
            "bootstrap.allow_workspace_overlay",
            bootstrap.allow_workspace_overlay,
            &origin,
            &overrides,
            &mut self.field_origins,
        )?;
        merge_scalar(
            &mut self.non_interactive,
            "bootstrap.non_interactive",
            bootstrap.non_interactive.clone(),
            &origin,
            &overrides,
            &mut self.field_origins,
        )?;

        if let Some(identity) = &bootstrap.identity {
            merge_scalar(
                &mut self.identity.default_user,
                "bootstrap.identity.default_user",
                identity.default_user.clone(),
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
            merge_scalar(
                &mut self.identity.default_uid,
                "bootstrap.identity.default_uid",
                identity.default_uid,
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
            merge_scalar(
                &mut self.identity.default_gid,
                "bootstrap.identity.default_gid",
                identity.default_gid,
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
            merge_scalar(
                &mut self.identity.auto_mapping,
                "bootstrap.identity.auto_mapping",
                identity.auto_mapping,
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
            merge_scalar(
                &mut self.identity.run_as_root_input,
                "bootstrap.identity.run_as_root_input",
                identity.run_as_root_input.clone(),
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
            merge_scalar(
                &mut self.identity.uid_input,
                "bootstrap.identity.uid_input",
                identity.uid_input.clone(),
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
            merge_scalar(
                &mut self.identity.gid_input,
                "bootstrap.identity.gid_input",
                identity.gid_input.clone(),
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
            merge_scalar(
                &mut self.identity.home_input,
                "bootstrap.identity.home_input",
                identity.home_input.clone(),
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
        }

        if let Some(handoff) = &bootstrap.handoff {
            merge_scalar(
                &mut self.handoff.runtime,
                "bootstrap.handoff.runtime",
                handoff.runtime.clone(),
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
            merge_scalar(
                &mut self.handoff.exec_prefix,
                "bootstrap.handoff.exec_prefix",
                handoff.exec_prefix.clone(),
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
            merge_scalar(
                &mut self.handoff.shell_prefix,
                "bootstrap.handoff.shell_prefix",
                handoff.shell_prefix.clone(),
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
            merge_scalar(
                &mut self.handoff.ssh_daemon,
                "bootstrap.handoff.ssh_daemon",
                handoff.ssh_daemon.clone(),
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
            merge_scalar(
                &mut self.handoff.login_shell,
                "bootstrap.handoff.login_shell",
                handoff.login_shell.clone(),
                &origin,
                &overrides,
                &mut self.field_origins,
            )?;
        }

        if let Some(policy) = &bootstrap.policy {
            // Policy sets are additive. A child can add restrictions or safe
            // kinds, but no profile can silently remove a parent's restriction.
            self.policy
                .workspace_safe_action_kinds
                .extend(policy.workspace_safe_action_kinds.iter().copied());
            self.policy
                .admin_only_action_kinds
                .extend(policy.admin_only_action_kinds.iter().copied());
        }

        for (name, input) in &bootstrap.inputs {
            let input_origin =
                profile_origin(profile_info, source, &format!("bootstrap.inputs.{name}"));
            match self.inputs.get(name) {
                None => {
                    self.inputs.insert(name.clone(), input.clone());
                    self.input_origins.insert(name.clone(), input_origin);
                }
                Some(previous) if previous == input => {}
                Some(_) => {
                    let path = format!("bootstrap.inputs.{name}");
                    if !overrides.contains(&path) {
                        return Err(conflict_error(
                            &path,
                            self.input_origins.get(name).expect("input origin exists"),
                            &input_origin,
                            "declare an explicit override with a non-empty reason",
                        ));
                    }
                    self.inputs.insert(name.clone(), input.clone());
                    self.input_origins.insert(name.clone(), input_origin);
                }
            }
        }

        let mut seen_actions = BTreeSet::new();
        for (index, action) in bootstrap.actions.iter().enumerate() {
            if !seen_actions.insert(action.id.clone()) {
                return Err(LoaderError::DuplicateActionId {
                    profile: profile.id.clone(),
                    action: action.id.clone(),
                });
            }
            let mut action = action.clone();
            action.origin =
                profile_origin(profile_info, source, &format!("bootstrap.actions[{index}]"));
            let Some(existing_index) = self.action_indexes.get(&action.id).copied() else {
                self.action_indexes
                    .insert(action.id.clone(), self.actions.len());
                self.actions.push(action);
                continue;
            };

            let path = format!("bootstrap.actions.{}", action.id);
            if same_action_definition(&self.actions[existing_index], &action) {
                continue;
            }
            if !source.source.is_trusted() {
                return Err(LoaderError::TrustViolation {
                    profile: profile.id.clone(),
                    source: source.source.clone(),
                    path,
                    message: "untrusted profiles may not replace inherited actions".to_owned(),
                });
            }
            let Some(override_reason) = overrides.reason(&path) else {
                return Err(conflict_error(
                    &path,
                    &self.actions[existing_index].origin,
                    &action.origin,
                    "declare override.\"bootstrap.actions.<id>\" with a non-empty reason",
                ));
            };
            if action.reason.is_none() {
                action.reason = Some(override_reason.to_owned());
            }
            self.actions[existing_index] = action;
        }

        Ok(())
    }

    pub(crate) fn finish(self) -> Result<BootstrapConfig, LoaderError> {
        if !self.saw_bootstrap {
            return Err(LoaderError::Invalid {
                location: "bootstrap".to_owned(),
                message: "the selected profile graph declares no bootstrap namespace".to_owned(),
            });
        }
        let config = BootstrapConfig {
            schema: self.schema.ok_or_else(|| missing("bootstrap.schema"))?,
            mode: self.mode.unwrap_or(BootstrapMode::Strict),
            workspace_root: self
                .workspace_root
                .ok_or_else(|| missing("bootstrap.workspace_root"))?,
            allow_workspace_overlay: self.allow_workspace_overlay.unwrap_or(false),
            non_interactive: self.non_interactive.unwrap_or(NonInteractivePolicy::Deny),
            identity: IdentityConfig {
                default_user: self.identity.default_user,
                default_uid: self.identity.default_uid,
                default_gid: self.identity.default_gid,
                auto_mapping: self.identity.auto_mapping.unwrap_or(false),
                run_as_root_input: self.identity.run_as_root_input,
                uid_input: self.identity.uid_input,
                gid_input: self.identity.gid_input,
                home_input: self.identity.home_input,
            },
            handoff: HandoffConfig {
                runtime: self
                    .handoff
                    .runtime
                    .ok_or_else(|| missing("bootstrap.handoff.runtime"))?,
                exec_prefix: self.handoff.exec_prefix.unwrap_or_default(),
                shell_prefix: self.handoff.shell_prefix.unwrap_or_default(),
                ssh_daemon: self.handoff.ssh_daemon,
                login_shell: self.handoff.login_shell,
            },
            policy: self.policy,
            inputs: self.inputs,
            actions: self.actions,
        };
        config.validate().map_err(LoaderError::Model)?;
        validate_input_aliases(&config)?;
        Ok(config)
    }
}

fn merge_scalar<T>(
    slot: &mut Option<T>,
    path: &str,
    incoming: Option<T>,
    incoming_origin: &Origin,
    overrides: &OverrideCatalog,
    origins: &mut BTreeMap<String, Origin>,
) -> Result<(), LoaderError>
where
    T: Clone + PartialEq,
{
    let Some(incoming) = incoming else {
        return Ok(());
    };
    match slot {
        None => {
            *slot = Some(incoming);
            origins.insert(path.to_owned(), incoming_origin.clone());
        }
        Some(current) if *current == incoming => {}
        Some(current) => {
            if !overrides.contains(path) {
                return Err(conflict_error(
                    path,
                    origins.get(path).expect("scalar origin exists"),
                    incoming_origin,
                    "declare an explicit override with a non-empty reason",
                ));
            }
            *current = incoming;
            origins.insert(path.to_owned(), incoming_origin.clone());
        }
    }
    Ok(())
}

fn same_action_definition(left: &Action, right: &Action) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.origin = Origin::unknown();
    right.origin = Origin::unknown();
    left == right
}

fn profile_origin(profile: &LoadedProfile, source: &ProfileSource, suffix: &str) -> Origin {
    Origin {
        profile: profile.id.clone(),
        source: source.source.clone(),
        location: Some(match &source.location {
            Some(location) => format!("{location}:{suffix}"),
            None => format!("{}:{suffix}", profile.id),
        }),
    }
}

fn conflict_error(path: &str, previous: &Origin, incoming: &Origin, remedy: &str) -> LoaderError {
    LoaderError::Conflict {
        path: path.to_owned(),
        previous: Box::new(previous.clone()),
        incoming: Box::new(incoming.clone()),
        remedy: remedy.to_owned(),
    }
}

fn missing(location: &str) -> LoaderError {
    LoaderError::Invalid {
        location: location.to_owned(),
        message: "required bootstrap value is missing from the profile graph".to_owned(),
    }
}

fn reject_untrusted_bootstrap_fields(
    bootstrap: &RawBootstrap,
    source: &ProfileSource,
) -> Result<(), LoaderError> {
    let has_configuration = bootstrap.schema.is_some()
        || bootstrap.mode.is_some()
        || bootstrap.workspace_root.is_some()
        || bootstrap.allow_workspace_overlay.is_some()
        || bootstrap.non_interactive.is_some()
        || bootstrap.identity.is_some()
        || bootstrap.handoff.is_some()
        || bootstrap.policy.is_some()
        || !bootstrap.inputs.is_empty()
        || !bootstrap.overrides.is_empty();
    if has_configuration {
        return Err(LoaderError::TrustViolation {
            profile: source.id.clone(),
            source: source.source.clone(),
            path: "bootstrap".to_owned(),
            message: "untrusted profiles may contribute only bootstrap.actions".to_owned(),
        });
    }
    Ok(())
}

fn validate_input_aliases(config: &BootstrapConfig) -> Result<(), LoaderError> {
    let mut names = BTreeMap::<String, String>::new();
    for (name, input) in &config.inputs {
        register_input_name(&mut names, name, name)?;
        for alias in &input.aliases {
            register_input_name(&mut names, alias, name)?;
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
            return Err(LoaderError::Invalid {
                location: "bootstrap.inputs".to_owned(),
                message: format!(
                    "input name or alias {name:?} is declared by both {previous:?} and {owner:?}"
                ),
            });
        }
    }
    Ok(())
}
