use crate::{
    CoreError, HandoffCommand, PosixSystem, ResolvedIdentity, ResourceLockManager, SshCapability,
};
use container_init_bootstrap_model::{ActionKind, Origin, PlanPhase};
use serde::Serialize;
use serde_json::to_vec_pretty;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::id,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Default)]
pub struct ExecutionOptions {
    receipt_path: Option<PathBuf>,
    posix: PosixSystem,
    ssh: Option<SshCapability>,
    resource_locks: ResourceLockManager,
    preserve_current_process_in_cgroup: bool,
}

impl ExecutionOptions {
    pub fn with_receipt_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.receipt_path = Some(path.into());
        self
    }

    pub fn without_receipt_path(mut self) -> Self {
        self.receipt_path = None;
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

    pub fn with_resource_locks(mut self, manager: ResourceLockManager) -> Self {
        self.resource_locks = manager;
        self
    }

    pub fn preserve_current_process_in_cgroup(mut self) -> Self {
        self.preserve_current_process_in_cgroup = true;
        self
    }

    pub(crate) fn receipt_path(&self) -> Option<&Path> {
        self.receipt_path.as_deref()
    }

    pub(crate) fn posix(&self) -> &PosixSystem {
        &self.posix
    }

    pub(crate) fn ssh(&self) -> Option<&SshCapability> {
        self.ssh.as_ref()
    }

    pub(crate) fn resource_locks(&self) -> &ResourceLockManager {
        &self.resource_locks
    }

    pub(crate) fn preserves_current_process_in_cgroup(&self) -> bool {
        self.preserve_current_process_in_cgroup
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
    pub(crate) fn succeeded(&self) -> bool {
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
    identity: ResolvedIdentity,
    outcomes: Vec<ActionOutcome>,
    handoff: Option<HandoffCommand>,
    root_service_handoff: bool,
    warnings: Vec<String>,
}

pub struct PreparedExec {
    identity: ResolvedIdentity,
    handoff: HandoffCommand,
    root_service_handoff: bool,
}

impl PreparedExec {
    pub(crate) fn new(
        identity: ResolvedIdentity,
        handoff: HandoffCommand,
        root_service_handoff: bool,
    ) -> Self {
        Self {
            identity,
            handoff,
            root_service_handoff,
        }
    }

    pub fn identity(&self) -> &ResolvedIdentity {
        &self.identity
    }

    pub fn handoff(&self) -> &HandoffCommand {
        &self.handoff
    }

    pub fn root_service_handoff(&self) -> bool {
        self.root_service_handoff
    }
}

impl ExecutionReport {
    pub(crate) fn new(
        identity: ResolvedIdentity,
        outcomes: Vec<ActionOutcome>,
        handoff: Option<HandoffCommand>,
        root_service_handoff: bool,
        warnings: Vec<String>,
    ) -> Self {
        Self {
            identity,
            outcomes,
            handoff,
            root_service_handoff,
            warnings,
        }
    }

    pub fn succeeded(&self) -> bool {
        self.outcomes
            .iter()
            .all(|outcome| outcome.status.succeeded())
    }

    pub fn outcome(&self, id: &str) -> Option<&ActionOutcome> {
        self.outcomes.iter().find(|outcome| outcome.id == id)
    }

    pub fn identity(&self) -> &ResolvedIdentity {
        &self.identity
    }

    pub fn outcomes(&self) -> &[ActionOutcome] {
        &self.outcomes
    }

    pub fn handoff(&self) -> Option<&HandoffCommand> {
        self.handoff.as_ref()
    }

    pub fn root_service_handoff(&self) -> bool {
        self.root_service_handoff
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

pub(crate) struct ExecutionReportWriter;

impl ExecutionReportWriter {
    pub(crate) fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), CoreError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|source| CoreError::io(Some("receipt"), Some(parent.to_path_buf()), source))?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = parent.join(format!(
            ".{}-container-init-{stamp}-{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("receipt"),
            id()
        ));
        let encoded = to_vec_pretty(value).map_err(|source| CoreError::Serialization {
            operation: "serialize receipt".to_owned(),
            source,
        })?;
        let result = (|| -> io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&encoded)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            Ok(())
        })();
        if let Err(source) = result {
            let _ = fs::remove_file(&temporary);
            return Err(CoreError::io(
                Some("receipt"),
                Some(path.to_path_buf()),
                source,
            ));
        }
        Ok(())
    }
}
