use super::transaction::ExecTransaction;
use container_init_bootstrap_model::{
    ActionKind, BootstrapConfig, Condition, ConditionValue, Plan,
};
use container_init_core::{CoreError, ExecutionOptions, ResolvedIdentity};
use container_init_protocol::{BackendError, BackendState, BackendStatus};
use serde::Serialize;
use serde_json::to_vec;
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

#[derive(Serialize)]
struct SnapshotDescription<'a> {
    profile: &'a str,
    config: &'a BootstrapConfig,
    plan: &'a Plan,
}

pub(super) struct AllowedClientValues {
    runtime_inputs: BTreeSet<String>,
    environment_names: BTreeSet<String>,
}

pub(super) struct BackendRuntimeOptions {
    pub(super) profile: String,
    pub(super) snapshot_id: String,
    pub(super) config: Arc<BootstrapConfig>,
    pub(super) plan: Arc<Plan>,
    pub(super) request_config: Arc<BootstrapConfig>,
    pub(super) request_plan: Arc<Plan>,
    pub(super) startup_identity: ResolvedIdentity,
    pub(super) allowed: AllowedClientValues,
    pub(super) execution_options: ExecutionOptions,
    pub(super) status: BackendStatus,
}

pub(super) struct BackendRuntime {
    profile: String,
    snapshot_id: String,
    config: Arc<BootstrapConfig>,
    plan: Arc<Plan>,
    request_config: Arc<BootstrapConfig>,
    request_plan: Arc<Plan>,
    startup_identity: ResolvedIdentity,
    allowed: AllowedClientValues,
    transactions: HashMap<String, ExecTransaction>,
    execution_options: ExecutionOptions,
    status: BackendStatus,
    active_connections: usize,
}

impl BackendRuntime {
    pub(super) fn new(options: BackendRuntimeOptions) -> Self {
        Self {
            profile: options.profile,
            snapshot_id: options.snapshot_id,
            config: options.config,
            plan: options.plan,
            request_config: options.request_config,
            request_plan: options.request_plan,
            startup_identity: options.startup_identity,
            allowed: options.allowed,
            transactions: HashMap::new(),
            execution_options: options.execution_options,
            status: options.status,
            active_connections: 0,
        }
    }

    pub(super) fn status(&self) -> BackendStatus {
        let mut status = self.status.clone();
        status.active_connections = self.active_connections;
        status
    }

    pub(super) fn profile(&self) -> &str {
        &self.profile
    }

    pub(super) fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }

    pub(super) fn config(&self) -> &BootstrapConfig {
        &self.config
    }

    pub(super) fn plan(&self) -> &Plan {
        &self.plan
    }

    pub(super) fn request_config(&self) -> &Arc<BootstrapConfig> {
        &self.request_config
    }

    pub(super) fn request_plan(&self) -> &Arc<Plan> {
        &self.request_plan
    }

    pub(super) fn startup_identity(&self) -> &ResolvedIdentity {
        &self.startup_identity
    }

    pub(super) fn allows_runtime_input(&self, name: &str) -> bool {
        self.allowed.runtime_inputs.contains(name)
    }

    pub(super) fn runtime_inputs(&self) -> Vec<String> {
        self.allowed.runtime_inputs.iter().cloned().collect()
    }

    pub(super) fn allows_environment_name(&self, name: &str) -> bool {
        self.allowed.environment_names.contains(name)
    }

    pub(super) fn environment_names(&self) -> Vec<String> {
        self.allowed.environment_names.iter().cloned().collect()
    }

    pub(super) fn execution_options(&self) -> &ExecutionOptions {
        &self.execution_options
    }

    pub(super) fn set_active_connections(&mut self, count: usize) {
        self.active_connections = count;
    }

    pub(super) fn transactions(&mut self) -> &mut HashMap<String, ExecTransaction> {
        &mut self.transactions
    }

    pub(super) fn mark_stopping(&mut self) {
        self.status.state = BackendState::Stopping;
    }

    pub(super) fn startup_config(config: &BootstrapConfig) -> BootstrapConfig {
        startup_config(config)
    }

    pub(super) fn trimmed_request_config(config: &BootstrapConfig) -> BootstrapConfig {
        request_config(config)
    }

    pub(super) fn allowed_client_values(
        config: &BootstrapConfig,
    ) -> Result<AllowedClientValues, CoreError> {
        allowed_client_values(config)
    }

    pub(super) fn calculate_snapshot_id(
        profile: &str,
        config: &BootstrapConfig,
        plan: &Plan,
    ) -> Result<String, CoreError> {
        snapshot_id(profile, config, plan)
    }
}

pub(super) struct RuntimeErrors;

impl RuntimeErrors {
    pub(super) fn backend(class: &str, retryable: bool, message: &str) -> BackendError {
        BackendError {
            class: class.to_owned(),
            retryable,
            message: message.to_owned(),
            action_id: None,
            path: None,
        }
    }

    pub(super) fn core(error: &CoreError) -> BackendError {
        BackendError {
            class: format!("{:?}", error.class()).to_ascii_lowercase(),
            retryable: false,
            message: "request identity could not be reconciled".to_owned(),
            action_id: None,
            path: None,
        }
    }
}

fn startup_config(config: &BootstrapConfig) -> BootstrapConfig {
    let mut startup = config.clone();
    startup.actions_mut().retain(|action| {
        !action.origin().source().is_workspace()
            && !matches!(
                action.kind(),
                ActionKind::ProcessDropPrivileges | ActionKind::HandoffExec
            )
    });
    let action_ids = startup
        .actions()
        .iter()
        .map(|action| action.id().to_owned())
        .collect::<BTreeSet<_>>();
    for action in startup.actions_mut() {
        let dependencies = action
            .depends_on()
            .iter()
            .filter(|dependency| action_ids.contains(*dependency))
            .cloned()
            .collect();
        action.set_depends_on(dependencies);
    }
    startup
}

fn request_config(config: &BootstrapConfig) -> BootstrapConfig {
    let mut request = config.clone();
    request.actions_mut().retain(|action| {
        action.origin().source().is_workspace()
            || matches!(
                action.kind(),
                ActionKind::IdentityResolve
                    | ActionKind::IdentityMapUser
                    | ActionKind::IdentityEnsureHome
                    | ActionKind::ProcessSetUserShell
            )
    });
    let action_ids = request
        .actions()
        .iter()
        .map(|action| action.id().to_owned())
        .collect::<BTreeSet<_>>();
    for action in request.actions_mut() {
        let dependencies = action
            .depends_on()
            .iter()
            .filter(|dependency| action_ids.contains(*dependency))
            .cloned()
            .collect();
        action.set_depends_on(dependencies);
    }
    request
}

fn allowed_client_values(config: &BootstrapConfig) -> Result<AllowedClientValues, CoreError> {
    let mut runtime_inputs = BTreeSet::new();
    for (name, input) in config.inputs() {
        if input.runtime() {
            runtime_inputs.insert(name.clone());
            runtime_inputs.extend(input.aliases().iter().cloned());
        }
    }
    let mut environment_names = runtime_inputs.clone();
    for action in config.actions() {
        if matches!(
            action.kind(),
            ActionKind::IdentityResolve
                | ActionKind::IdentityMapUser
                | ActionKind::IdentityEnsureHome
                | ActionKind::ProcessSetUserShell
        ) {
            let condition = action.condition().map_err(CoreError::Model)?;
            collect_environment_names(&condition, &mut environment_names);
        }
    }
    Ok(AllowedClientValues {
        runtime_inputs,
        environment_names,
    })
}

fn collect_environment_names(condition: &Condition, names: &mut BTreeSet<String>) {
    fn value(reference: &ConditionValue, names: &mut BTreeSet<String>) {
        if let ConditionValue::Reference(reference) = reference {
            if let Some(name) = reference.strip_prefix("env.") {
                names.insert(name.to_owned());
            }
        }
    }
    match condition {
        Condition::Equal(left, right) | Condition::NotEqual(left, right) => {
            value(left, names);
            value(right, names);
        }
        Condition::And(parts) | Condition::Or(parts) => {
            for part in parts {
                collect_environment_names(part, names);
            }
        }
        Condition::Not(part) => collect_environment_names(part, names),
        Condition::Always
        | Condition::Boolean(_)
        | Condition::ContextPathExistsOrCreate
        | Condition::Exists(_)
        | Condition::Writable(_)
        | Condition::InputSet(_)
        | Condition::Feature(_) => {}
    }
}

fn snapshot_id(profile: &str, config: &BootstrapConfig, plan: &Plan) -> Result<String, CoreError> {
    let bytes = to_vec(&SnapshotDescription {
        profile,
        config,
        plan,
    })
    .map_err(|source| CoreError::Serialization {
        operation: "serialize backend snapshot id".to_owned(),
        source,
    })?;
    let hash = bytes
        .into_iter()
        .fold(0xcbf29ce484222325_u64, |mut hash, byte| {
            hash ^= u64::from(byte);
            hash.wrapping_mul(0x100000001b3)
        });
    Ok(format!("{hash:016x}"))
}
