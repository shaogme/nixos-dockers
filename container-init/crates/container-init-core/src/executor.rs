use crate::cgroup;
use crate::condition::ConditionContext;
use crate::context::RuntimeContext;
use crate::error::CoreError;
use crate::filesystem;
use crate::handoff::HandoffCommand;
use crate::identity::{IdentityResolver, ResolvedIdentity, ResolvedInputs, WorkspaceStatus};
use crate::lock::BootstrapLock;
use crate::receipt;
use crate::ssh::{self, SshCapability};
use bootstrap_model::{
    Action, ActionKind, BootstrapConfig, FailurePolicy, Origin, Plan, PlanPhase, PlannedAction,
    RunAs,
};
use container_init_posix::{ActionChange, PosixIdentity, PosixSystem};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Actions can be executed through this alias by callers that do not need to
/// distinguish the plan-oriented name.
pub type ActionExecutor = PlanExecutor;

#[derive(Clone, Debug, Default)]
pub struct ExecutionOptions {
    pub lock_path: Option<PathBuf>,
    pub lock_timeout: Option<Duration>,
    pub receipt_path: Option<PathBuf>,
    pub posix: PosixSystem,
    pub ssh: Option<SshCapability>,
}

impl ExecutionOptions {
    pub fn with_lock_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.lock_path = Some(path.into());
        self
    }

    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = Some(timeout);
        self
    }

    pub fn with_receipt_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.receipt_path = Some(path.into());
        self
    }

    pub fn with_posix(mut self, posix: PosixSystem) -> Self {
        self.posix = posix;
        self
    }

    pub fn with_ssh(mut self, capability: SshCapability) -> Self {
        self.ssh = Some(capability);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Succeeded,
    SkippedCondition,
    SkippedDependency,
    FailedWarn,
    FailedIgnore,
}

impl ActionStatus {
    fn succeeded(&self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

#[derive(Debug, Serialize)]
pub struct ActionOutcome {
    pub id: String,
    pub kind: ActionKind,
    pub phase: PlanPhase,
    pub origin: Origin,
    pub status: ActionStatus,
    pub path: Option<PathBuf>,
    pub message: String,
    pub error: Option<CoreError>,
}

#[derive(Debug, Serialize)]
pub struct ExecutionReport {
    pub identity: ResolvedIdentity,
    pub outcomes: Vec<ActionOutcome>,
    pub handoff: Option<HandoffCommand>,
    pub root_service_handoff: bool,
    pub warnings: Vec<String>,
}

impl ExecutionReport {
    pub fn succeeded(&self) -> bool {
        self.outcomes
            .iter()
            .all(|outcome| outcome.status.succeeded())
    }

    pub fn outcome(&self, id: &str) -> Option<&ActionOutcome> {
        self.outcomes.iter().find(|outcome| outcome.id == id)
    }
}

pub struct PlanExecutor {
    config: BootstrapConfig,
    context: RuntimeContext,
    options: ExecutionOptions,
}

impl PlanExecutor {
    pub fn new(config: BootstrapConfig, context: RuntimeContext) -> Self {
        Self {
            config,
            context,
            options: ExecutionOptions::default(),
        }
    }

    pub fn with_options(mut self, options: ExecutionOptions) -> Self {
        self.options = options;
        self
    }

    pub fn config(&self) -> &BootstrapConfig {
        &self.config
    }

    pub fn context(&self) -> &RuntimeContext {
        &self.context
    }

    pub fn resolve_identity(&self) -> Result<ResolvedIdentity, CoreError> {
        IdentityResolver::with_posix(self.options.posix.clone())
            .resolve(&self.config, &self.context)
    }

    /// Execute every action in a pre-built static plan. The command is only
    /// appended to the handoff argv; this method never replaces the current
    /// process. Use execute_and_handoff for the production run path.
    pub fn execute(&self, plan: &Plan, command: &[String]) -> Result<ExecutionReport, CoreError> {
        self.validate_plan(plan)?;
        let _lock = self.acquire_lock()?;
        let resolver = IdentityResolver::with_posix(self.options.posix.clone());
        let identity = resolver.resolve(&self.config, &self.context)?;
        let root_service_handoff = self.is_root_service_handoff(command);
        let warnings = identity_warnings(&self.config, &identity);
        let inputs = resolver.resolve_inputs(&self.config, &self.context)?;
        let values = interpolation_values(&self.config, &self.context, &identity, &inputs);
        let condition_context = ConditionContext::new(
            &identity,
            &inputs,
            &self.context,
            Path::new(&self.config.workspace_root),
        );
        let actions = self
            .config
            .actions
            .iter()
            .map(|action| (action.id.as_str(), action))
            .collect::<BTreeMap<_, _>>();
        let mut completed = BTreeMap::<String, ActionStatus>::new();
        let mut outcomes = Vec::with_capacity(plan.actions().len());
        let mut handoff = None;

        for planned in plan.actions() {
            let action = actions
                .get(planned.id.as_str())
                .ok_or_else(|| CoreError::Invalid {
                    location: format!("bootstrap.plan.{}", planned.id),
                    message: "plan references an action absent from the configuration".to_owned(),
                })?;
            if planned.kind != action.kind {
                return Err(CoreError::Invalid {
                    location: format!("bootstrap.plan.{}", planned.id),
                    message: "plan action kind does not match the configuration".to_owned(),
                });
            }

            if planned.depends_on.iter().any(|dependency| {
                !completed
                    .get(dependency)
                    .is_some_and(ActionStatus::succeeded)
            }) {
                let outcome = skipped_outcome(
                    planned,
                    ActionStatus::SkippedDependency,
                    "dependency did not complete successfully",
                    None,
                );
                completed.insert(planned.id.clone(), outcome.status.clone());
                outcomes.push(outcome);
                continue;
            }

            let condition = action.condition().map_err(CoreError::Model)?;
            if !condition_context
                .evaluate(&condition)
                .map_err(|error| annotate_action_error(error, action))?
            {
                let outcome = skipped_outcome(
                    planned,
                    ActionStatus::SkippedCondition,
                    "condition evaluated to false",
                    None,
                );
                completed.insert(planned.id.clone(), outcome.status.clone());
                outcomes.push(outcome);
                continue;
            }

            // A configured SSH daemon is a root-owned service handoff. Keep
            // the bootstrap process privileged for that one command; the
            // login shell still receives the resolved identity later.
            if action.kind == ActionKind::ProcessDropPrivileges && root_service_handoff {
                let outcome = ActionOutcome {
                    id: planned.id.clone(),
                    kind: planned.kind,
                    phase: planned.phase,
                    origin: planned.origin.clone(),
                    status: ActionStatus::Succeeded,
                    path: action_path(action, &values)?,
                    message: "privilege drop skipped for root service handoff".to_owned(),
                    error: None,
                };
                completed.insert(planned.id.clone(), ActionStatus::Succeeded);
                outcomes.push(outcome);
                continue;
            }

            if identity.uid == 0
                && matches!(
                    action.kind,
                    ActionKind::IdentityMapUser
                        | ActionKind::ProcessSetUserShell
                        | ActionKind::ProcessDropPrivileges
                )
            {
                let outcome = ActionOutcome {
                    id: planned.id.clone(),
                    kind: planned.kind,
                    phase: planned.phase,
                    origin: planned.origin.clone(),
                    status: ActionStatus::Succeeded,
                    path: action_path(action, &values)?,
                    message: "ordinary target identity action skipped for root".to_owned(),
                    error: None,
                };
                completed.insert(planned.id.clone(), ActionStatus::Succeeded);
                outcomes.push(outcome);
                continue;
            }

            self.check_run_as(action.run_as, &identity, planned)
                .map_err(|error| annotate_action_error(error, action))?;
            match self.execute_action(action, &identity, &values, command) {
                Ok((change, prepared_handoff)) => {
                    if let Some(command) = prepared_handoff {
                        handoff = Some(command);
                    }
                    let outcome = ActionOutcome {
                        id: planned.id.clone(),
                        kind: planned.kind,
                        phase: planned.phase,
                        origin: planned.origin.clone(),
                        status: ActionStatus::Succeeded,
                        path: action_path(action, &values)?,
                        message: change.message().to_owned(),
                        error: None,
                    };
                    completed.insert(planned.id.clone(), ActionStatus::Succeeded);
                    outcomes.push(outcome);
                }
                Err(error) => {
                    let error = annotate_action_error(error, action);
                    if action.failure == FailurePolicy::Error {
                        return Err(error);
                    }
                    let status = match action.failure {
                        FailurePolicy::Warn => ActionStatus::FailedWarn,
                        FailurePolicy::Ignore => ActionStatus::FailedIgnore,
                        FailurePolicy::Error => unreachable!(),
                    };
                    let outcome = ActionOutcome {
                        id: planned.id.clone(),
                        kind: planned.kind,
                        phase: planned.phase,
                        origin: planned.origin.clone(),
                        status: status.clone(),
                        path: action_path(action, &values)?,
                        message: "action failed according to its failure policy".to_owned(),
                        error: Some(error),
                    };
                    completed.insert(planned.id.clone(), status);
                    outcomes.push(outcome);
                }
            }
        }

        let report = ExecutionReport {
            identity,
            outcomes,
            handoff,
            root_service_handoff,
            warnings,
        };
        self.write_receipt(&report)?;
        Ok(report)
    }

    pub fn execute_plan(&self, plan: &Plan) -> Result<ExecutionReport, CoreError> {
        self.execute(plan, &[])
    }

    pub fn build_handoff_command(&self, command: &[String]) -> Result<HandoffCommand, CoreError> {
        HandoffCommand::from_config(&self.config.handoff, command)
    }

    /// Execute the plan and replace this process with the configured runtime.
    pub fn execute_and_handoff(&self, plan: &Plan, command: &[String]) -> Result<(), CoreError> {
        let report = self.execute(plan, command)?;
        let handoff = report
            .handoff
            .unwrap_or(self.build_handoff_command(command)?);
        if report.root_service_handoff {
            handoff.exec_as_root_service()
        } else {
            handoff.exec_with_identity(Some(&report.identity))
        }
    }

    pub fn prepare_exec(
        &self,
        command: &[String],
    ) -> Result<(ResolvedIdentity, HandoffCommand, bool), CoreError> {
        self.config.validate().map_err(CoreError::Model)?;
        let resolver = IdentityResolver::with_posix(self.options.posix.clone());
        let identity = resolver.resolve(&self.config, &self.context)?;
        let root_service_handoff = self.is_root_service_handoff(command);
        let handoff = self.build_handoff_command(command)?;
        Ok((identity, handoff, root_service_handoff))
    }

    pub fn exec_prepared(
        &self,
        identity: &ResolvedIdentity,
        handoff: HandoffCommand,
        root_service_handoff: bool,
    ) -> Result<(), CoreError> {
        if !root_service_handoff && identity.uid != 0 && !identity.run_as_root {
            let posix = posix_identity(identity);
            self.options
                .posix
                .drop_privileges(&posix)
                .map_err(|error| CoreError::from_posix("drop-privileges", None, error))?;
        }
        if root_service_handoff {
            handoff.exec_as_root_service()
        } else {
            handoff.exec_with_identity(Some(identity))
        }
    }

    /// Transition privileges to the resolved identity and replace this process with
    /// the configured handoff command, without executing any bootstrap actions or
    /// acquiring the bootstrap lock.
    pub fn exec_and_handoff(&self, command: &[String]) -> Result<(), CoreError> {
        let (identity, handoff, root_service_handoff) = self.prepare_exec(command)?;
        self.exec_prepared(&identity, handoff, root_service_handoff)
    }

    fn validate_plan(&self, plan: &Plan) -> Result<(), CoreError> {
        self.config.validate().map_err(CoreError::Model)?;
        let expected = self.config.build_plan().map_err(CoreError::Model)?;
        if expected != *plan {
            return Err(CoreError::Invalid {
                location: "bootstrap.plan".to_owned(),
                message: "plan was not produced from the supplied configuration".to_owned(),
            });
        }
        Ok(())
    }

    fn acquire_lock(&self) -> Result<Option<BootstrapLock>, CoreError> {
        self.options
            .lock_path
            .as_ref()
            .map(|path| match self.options.lock_timeout {
                Some(timeout) => BootstrapLock::acquire_with_timeout(path.clone(), timeout),
                None => BootstrapLock::acquire(path.clone()),
            })
            .transpose()
    }

    fn check_run_as(
        &self,
        run_as: RunAs,
        identity: &ResolvedIdentity,
        planned: &PlannedAction,
    ) -> Result<(), CoreError> {
        let (uid, _) = self.options.posix.current_ids();
        match run_as {
            RunAs::Root if uid != 0 => Err(CoreError::Permission {
                action: Some(planned.id.clone()),
                message: "action requires effective root".to_owned(),
            }),
            RunAs::Target if uid != 0 && uid != identity.uid => Err(CoreError::Permission {
                action: Some(planned.id.clone()),
                message: format!(
                    "action requires the target UID {} or effective root",
                    identity.uid
                ),
            }),
            _ => Ok(()),
        }
    }

    fn is_root_service_handoff(&self, command: &[String]) -> bool {
        self.options.posix.current_ids().0 == 0
            && self
                .config
                .handoff
                .ssh_daemon
                .as_deref()
                .is_some_and(|daemon| command.first().is_some_and(|candidate| candidate == daemon))
    }

    fn execute_action(
        &self,
        action: &Action,
        identity: &ResolvedIdentity,
        values: &BTreeMap<String, String>,
        command: &[String],
    ) -> Result<(ActionChange, Option<HandoffCommand>), CoreError> {
        let action_id = action.id.as_str();
        if action.kind == ActionKind::HandoffExec {
            return Ok((
                ActionChange::new("handoff command prepared"),
                Some(HandoffCommand::from_config(&self.config.handoff, command)?),
            ));
        }
        let path = |field: &str, value: Option<&String>| {
            value
                .ok_or_else(|| CoreError::Invalid {
                    location: format!("bootstrap.actions.{action_id}.{field}"),
                    message: "field is required".to_owned(),
                })
                .and_then(|value| filesystem::render_path(action_id, field, value, values))
        };
        let result = match action.kind {
            ActionKind::IdentityResolve => Ok(ActionChange::new("identity resolved")),
            ActionKind::IdentityMapUser => self
                .options
                .posix
                .map_user(&posix_identity(identity))
                .map_err(|error| {
                    CoreError::from_posix(
                        action_id,
                        Some(self.options.posix.passwd_path().to_path_buf()),
                        error,
                    )
                }),
            ActionKind::IdentityEnsureHome => {
                let home = action
                    .path
                    .as_ref()
                    .map(|value| filesystem::render_path(action_id, "path", value, values))
                    .transpose()?
                    .unwrap_or_else(|| identity.home.clone());
                filesystem::ensure_dir(
                    action_id,
                    &home,
                    action.mode.as_deref().or(Some("0755")),
                    action.owner.as_deref().or(Some("identity.target")),
                    identity,
                    &self.options.posix,
                )
            }
            ActionKind::FilesystemEnsureDir => filesystem::ensure_dir(
                action_id,
                &path("path", action.path.as_ref())?,
                action.mode.as_deref(),
                action.owner.as_deref(),
                identity,
                &self.options.posix,
            ),
            ActionKind::FilesystemEnsureFile => filesystem::ensure_file(
                action_id,
                &path("path", action.path.as_ref())?,
                action.content.as_deref(),
                action.mode.as_deref(),
                action.owner.as_deref(),
                identity,
                &self.options.posix,
            ),
            ActionKind::FilesystemEnsureSymlink => filesystem::ensure_symlink(
                action_id,
                &path("link", action.link.as_ref())?,
                &path("target", action.target.as_ref())?,
                action.parent_mode.as_deref(),
                action.owner.as_deref(),
                identity,
                &self.options.posix,
            ),
            ActionKind::FilesystemChown => filesystem::chown(
                action_id,
                &path("path", action.path.as_ref())?,
                action.owner.as_deref().expect("validated owner"),
                action.recursive,
                identity,
                &self.options.posix,
            ),
            ActionKind::FilesystemChmod => filesystem::chmod(
                action_id,
                &path("path", action.path.as_ref())?,
                action.mode.as_deref().expect("validated mode"),
                action.recursive,
            ),
            ActionKind::ProcessSetUserShell => {
                let user = action.user.as_deref() == Some("identity.target");
                let user = if user {
                    identity.user.as_str()
                } else {
                    action.user.as_deref().expect("validated user")
                };
                let shell = filesystem::render_path(
                    action_id,
                    "shell",
                    action.shell.as_deref().expect("validated shell"),
                    values,
                )?;
                self.options
                    .posix
                    .set_user_shell(user, &shell)
                    .map_err(|error| {
                        CoreError::from_posix(
                            action_id,
                            Some(self.options.posix.passwd_path().to_path_buf()),
                            error,
                        )
                    })
            }
            ActionKind::ProcessDropPrivileges => self
                .options
                .posix
                .drop_privileges(&posix_identity(identity))
                .map_err(|error| CoreError::from_posix(action_id, None, error)),
            ActionKind::ServiceSshPrepare => {
                let capability = self.options.ssh.as_ref().ok_or_else(|| {
                    CoreError::action(
                        action_id,
                        None,
                        "SSH preparation requires an enabled service capability",
                    )
                })?;
                ssh::prepare(action, identity, values, &self.options.posix, capability)
            }
            ActionKind::CgroupV2Init => {
                cgroup::init_v2(action, identity, values, &self.options.posix)
            }
            ActionKind::HandoffExec => unreachable!("handled before the action match"),
        };
        result.map(|change| (change, None))
    }

    fn write_receipt(&self, report: &ExecutionReport) -> Result<(), CoreError> {
        if let Some(path) = &self.options.receipt_path {
            receipt::write(path, report)?;
        }
        Ok(())
    }

    pub fn runtime_dir(&self) -> PathBuf {
        std::env::var_os("CONTAINER_INIT_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                #[cfg(unix)]
                if self.options.posix.current_ids().0 == 0 {
                    PathBuf::from("/run/container-init")
                } else {
                    Path::new(&self.config.workspace_root).join(".container-init")
                }
                #[cfg(not(unix))]
                Path::new(&self.config.workspace_root).join(".container-init")
            })
    }

    pub fn is_reconciled(&self, identity: &ResolvedIdentity) -> bool {
        let has_ensure_home = self
            .config
            .actions
            .iter()
            .any(|action| action.kind == ActionKind::IdentityEnsureHome);
        if has_ensure_home {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                match std::fs::metadata(&identity.home) {
                    Ok(meta) => {
                        if meta.uid() != identity.uid || meta.gid() != identity.gid {
                            return false;
                        }
                    }
                    Err(_) => {
                        return false;
                    }
                }
            }
        }

        true
    }
}

fn posix_identity(identity: &ResolvedIdentity) -> PosixIdentity {
    PosixIdentity::new(
        identity.uid,
        identity.gid,
        identity.user.clone(),
        identity.home.clone(),
    )
}

fn identity_warnings(config: &BootstrapConfig, identity: &ResolvedIdentity) -> Vec<String> {
    if config.identity.auto_mapping && identity.workspace != WorkspaceStatus::Mounted {
        vec![format!(
            "workspace auto-mapping was not used ({:?})",
            identity.workspace
        )]
    } else {
        Vec::new()
    }
}

fn interpolation_values(
    config: &BootstrapConfig,
    context: &RuntimeContext,
    identity: &ResolvedIdentity,
    inputs: &ResolvedInputs,
) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    values.insert(
        "bootstrap.workspace_root".to_owned(),
        config.workspace_root.clone(),
    );
    values.insert("identity.uid".to_owned(), identity.uid.to_string());
    values.insert("identity.gid".to_owned(), identity.gid.to_string());
    values.insert("identity.user".to_owned(), identity.user.clone());
    values.insert(
        "identity.home".to_owned(),
        identity.home.to_string_lossy().into_owned(),
    );
    values.insert("identity.target".to_owned(), identity.user.clone());
    values.insert(
        "context.cwd".to_owned(),
        context.cwd().to_string_lossy().into_owned(),
    );
    values.insert("context.os".to_owned(), std::env::consts::OS.to_owned());
    values.insert("context.arch".to_owned(), std::env::consts::ARCH.to_owned());
    for (name, input) in inputs.iter() {
        values.insert(format!("input.{name}"), input.raw.clone());
    }
    for (name, value) in context.environment() {
        values.insert(format!("env.{name}"), value.clone());
    }
    values
}

fn action_path(
    action: &Action,
    values: &BTreeMap<String, String>,
) -> Result<Option<PathBuf>, CoreError> {
    let field = match action.kind {
        ActionKind::FilesystemEnsureSymlink => action.link.as_ref(),
        _ => action.path.as_ref(),
    };
    field
        .map(|value| filesystem::render_path(&action.id, "path", value, values))
        .transpose()
}

fn skipped_outcome(
    planned: &PlannedAction,
    status: ActionStatus,
    message: &str,
    path: Option<PathBuf>,
) -> ActionOutcome {
    ActionOutcome {
        id: planned.id.clone(),
        kind: planned.kind,
        phase: planned.phase,
        origin: planned.origin.clone(),
        status,
        path,
        message: message.to_owned(),
        error: None,
    }
}

fn annotate_action_error(error: CoreError, action: &Action) -> CoreError {
    CoreError::Annotated {
        action: action.id.clone(),
        origin: action.origin.clone(),
        source: Box::new(error),
    }
}
