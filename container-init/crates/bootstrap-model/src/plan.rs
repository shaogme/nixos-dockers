use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use crate::action::{Action, ActionKind, Idempotency, RunAs};
use crate::config::BootstrapConfig;
use crate::error::ModelError;
use crate::provenance::Origin;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanPhase {
    Root,
    Current,
    Target,
    Handoff,
}

/// Side-effect summary exposed by `plan`. It intentionally omits fixed file
/// content so plan output cannot disclose an action payload or secret.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEffect {
    IdentityResolve,
    IdentityMapUser,
    IdentityEnsureHome {
        path: Option<String>,
        mode: Option<String>,
        owner: Option<String>,
    },
    FilesystemEnsureDir {
        path: String,
        mode: Option<String>,
        owner: Option<String>,
    },
    FilesystemEnsureFile {
        path: String,
        mode: Option<String>,
        owner: Option<String>,
    },
    FilesystemEnsureSymlink {
        link: String,
        target: String,
        owner: Option<String>,
    },
    FilesystemChown {
        path: String,
        owner: String,
        recursive: bool,
    },
    FilesystemChmod {
        path: String,
        mode: String,
        recursive: bool,
    },
    ProcessSetUserShell {
        user: String,
        shell: String,
    },
    ProcessDropPrivileges,
    ServiceSshPrepare {
        host_key_dir: String,
        authorized_keys_dir: String,
        runtime_dir: String,
        host_key_types: Vec<String>,
        authorized_keys: bool,
    },
    CgroupV2Init {
        path: Option<String>,
        mount_mode: String,
        shadow_path: Option<String>,
        subgroup: Option<String>,
        controllers: Option<Vec<String>>,
        optional_controllers: Option<Vec<String>>,
        subgroup_type: Option<String>,
        subgroup_controllers: Option<Vec<String>>,
        subgroup_controller_values: Option<BTreeMap<String, String>>,
    },
    HandoffExec,
}

impl PlanEffect {
    pub(crate) fn from_action(action: &Action) -> Self {
        match action.kind {
            ActionKind::IdentityResolve => Self::IdentityResolve,
            ActionKind::IdentityMapUser => Self::IdentityMapUser,
            ActionKind::IdentityEnsureHome => Self::IdentityEnsureHome {
                path: action.path.clone(),
                mode: action.mode.clone(),
                owner: action.owner.clone(),
            },
            ActionKind::FilesystemEnsureDir => Self::FilesystemEnsureDir {
                path: action.path.clone().expect("validated action path"),
                mode: action.mode.clone(),
                owner: action.owner.clone(),
            },
            ActionKind::FilesystemEnsureFile => Self::FilesystemEnsureFile {
                path: action.path.clone().expect("validated action path"),
                mode: action.mode.clone(),
                owner: action.owner.clone(),
            },
            ActionKind::FilesystemEnsureSymlink => Self::FilesystemEnsureSymlink {
                link: action.link.clone().expect("validated action link"),
                target: action.target.clone().expect("validated action target"),
                owner: action.owner.clone(),
            },
            ActionKind::FilesystemChown => Self::FilesystemChown {
                path: action.path.clone().expect("validated action path"),
                owner: action.owner.clone().expect("validated action owner"),
                recursive: action.recursive,
            },
            ActionKind::FilesystemChmod => Self::FilesystemChmod {
                path: action.path.clone().expect("validated action path"),
                mode: action.mode.clone().expect("validated action mode"),
                recursive: action.recursive,
            },
            ActionKind::ProcessSetUserShell => Self::ProcessSetUserShell {
                user: action.user.clone().expect("validated action user"),
                shell: action.shell.clone().expect("validated action shell"),
            },
            ActionKind::ProcessDropPrivileges => Self::ProcessDropPrivileges,
            ActionKind::ServiceSshPrepare => Self::ServiceSshPrepare {
                host_key_dir: action
                    .ssh_host_key_dir()
                    .expect("validated SSH host-key directory")
                    .to_owned(),
                authorized_keys_dir: action
                    .ssh_authorized_keys_dir()
                    .expect("validated SSH authorized-keys directory")
                    .to_owned(),
                runtime_dir: action
                    .ssh_runtime_dir()
                    .expect("validated SSH runtime directory")
                    .to_owned(),
                host_key_types: if action.ssh_host_key_types().is_empty() {
                    vec!["rsa".to_owned(), "ed25519".to_owned()]
                } else {
                    action.ssh_host_key_types().to_vec()
                },
                authorized_keys: action.content.is_some()
                    || action.authorized_keys_source.is_some(),
            },
            ActionKind::CgroupV2Init => Self::CgroupV2Init {
                path: action.path.clone(),
                mount_mode: action.cgroup_mount_mode().to_owned(),
                shadow_path: action.shadow_path.clone(),
                subgroup: action.subgroup.clone(),
                controllers: action.controllers.clone(),
                optional_controllers: action.optional_controllers.clone(),
                subgroup_type: action.subgroup_type.clone(),
                subgroup_controllers: action.subgroup_controllers.clone(),
                subgroup_controller_values: action.subgroup_controller_values.clone(),
            },
            ActionKind::HandoffExec => Self::HandoffExec,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PlannedAction {
    pub id: String,
    pub kind: ActionKind,
    pub phase: PlanPhase,
    pub run_as: RunAs,
    pub depends_on: Vec<String>,
    pub origin: Origin,
    pub idempotency: Idempotency,
    pub effect: PlanEffect,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Plan {
    actions: Vec<PlannedAction>,
}

impl Plan {
    pub fn actions(&self) -> &[PlannedAction] {
        &self.actions
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.actions.iter().map(|action| action.id.as_str())
    }
}

impl BootstrapConfig {
    /// Build a deterministic static action plan. This does not resolve
    /// runtime inputs and never touches the host filesystem.
    pub fn build_plan(&self) -> Result<Plan, ModelError> {
        self.validate()?;

        let mut by_id = BTreeMap::new();
        for (index, action) in self.actions.iter().enumerate() {
            by_id.insert(action.id.as_str(), (index, action));
        }

        let identity_resolve = self
            .actions
            .iter()
            .filter(|action| action.kind == ActionKind::IdentityResolve)
            .collect::<Vec<_>>();
        if identity_resolve.len() > 1 {
            return Err(ModelError::Invalid {
                location: "bootstrap.actions".to_owned(),
                message: "identity.resolve may appear at most once".to_owned(),
            });
        }

        let identity_resolve_id = identity_resolve.first().map(|action| action.id.clone());
        let mut dependencies = Vec::with_capacity(self.actions.len());
        for action in &self.actions {
            let mut action_dependencies = BTreeSet::new();
            for dependency in &action.depends_on {
                if dependency == &action.id {
                    return Err(ModelError::DependencyCycle(vec![action.id.clone()]));
                }
                if !by_id.contains_key(dependency.as_str()) {
                    return Err(ModelError::MissingDependency {
                        action: action.id.clone(),
                        dependency: dependency.clone(),
                    });
                }
                action_dependencies.insert(dependency.clone());
            }

            if action.kind != ActionKind::IdentityResolve {
                if action.references_identity() && identity_resolve_id.is_none() {
                    return Err(ModelError::MissingIdentityResolve(action.id.clone()));
                }
                if let Some(resolve_id) = &identity_resolve_id {
                    action_dependencies.insert(resolve_id.clone());
                }
            }
            dependencies.push(action_dependencies.into_iter().collect::<Vec<_>>());
        }

        let handoff_count = self
            .actions
            .iter()
            .filter(|action| action.kind == ActionKind::HandoffExec)
            .count();
        if handoff_count > 1 {
            return Err(ModelError::Invalid {
                location: "bootstrap.actions".to_owned(),
                message: "handoff.exec may appear at most once".to_owned(),
            });
        }
        let drop_privilege_indexes = self
            .actions
            .iter()
            .enumerate()
            .filter_map(|(index, action)| {
                (action.kind == ActionKind::ProcessDropPrivileges).then_some(index)
            })
            .collect::<Vec<_>>();
        if drop_privilege_indexes.len() > 1 {
            return Err(ModelError::Invalid {
                location: "bootstrap.actions".to_owned(),
                message: "process.drop_privileges may appear at most once".to_owned(),
            });
        }
        if let Some(handoff_index) = self
            .actions
            .iter()
            .position(|action| action.kind == ActionKind::HandoffExec)
        {
            if let Some(drop_index) = drop_privilege_indexes.first() {
                dependencies[handoff_index].push(self.actions[*drop_index].id.clone());
                dependencies[handoff_index].sort();
                dependencies[handoff_index].dedup();
            }
        }

        let phases = self.actions.iter().map(Action::phase).collect::<Vec<_>>();
        for (index, action) in self.actions.iter().enumerate() {
            for dependency in &dependencies[index] {
                let dependency_index = by_id[dependency.as_str()].0;
                if phases[dependency_index] > phases[index] {
                    return Err(ModelError::PhaseViolation {
                        action: action.id.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }

        let order = stable_topological_order(&self.actions, &dependencies)?;
        let mut planned = Vec::with_capacity(order.len());
        for index in order {
            let action = &self.actions[index];
            planned.push(PlannedAction {
                id: action.id.clone(),
                kind: action.kind,
                phase: phases[index],
                run_as: action.run_as,
                depends_on: dependencies[index].clone(),
                origin: action.origin.clone(),
                idempotency: action.kind.idempotency(),
                effect: PlanEffect::from_action(action),
            });
        }

        Ok(Plan { actions: planned })
    }
}

fn stable_topological_order(
    actions: &[Action],
    dependencies: &[Vec<String>],
) -> Result<Vec<usize>, ModelError> {
    let indexes = actions
        .iter()
        .enumerate()
        .map(|(index, action)| (action.id.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let mut indegree = vec![0usize; actions.len()];
    let mut dependents = vec![Vec::<usize>::new(); actions.len()];
    for (index, action_dependencies) in dependencies.iter().enumerate() {
        indegree[index] = action_dependencies.len();
        for dependency in action_dependencies {
            dependents[indexes[dependency.as_str()]].push(index);
        }
    }

    let mut ready = BTreeSet::new();
    for (index, degree) in indegree.iter().enumerate() {
        if *degree == 0 {
            ready.insert((actions[index].phase(), index));
        }
    }

    let mut order = Vec::with_capacity(actions.len());
    while let Some((_, index)) = ready.pop_first() {
        order.push(index);
        for dependent in &dependents[index] {
            indegree[*dependent] -= 1;
            if indegree[*dependent] == 0 {
                ready.insert((actions[*dependent].phase(), *dependent));
            }
        }
    }
    if order.len() != actions.len() {
        let remaining = indegree
            .iter()
            .enumerate()
            .filter(|(_, degree)| **degree > 0)
            .map(|(index, _)| actions[index].id.clone())
            .collect();
        return Err(ModelError::DependencyCycle(remaining));
    }
    Ok(order)
}
