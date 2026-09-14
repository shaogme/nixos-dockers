use crate::config::LoadedConfig;
use bootstrap_model::{ActionKind, PlanPhase, RunAs};
use container_init_core::{
    CoreError, IdentityResolver, PosixSystem, ResolvedIdentity, RuntimeContext, SshCapability,
};
use container_init_posix::is_writable;
use serde::Serialize;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub message: String,
    pub error: Option<CoreError>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorHandoff {
    pub runtime: String,
    pub runtime_executable: bool,
    pub exec_prefix: Vec<String>,
    pub shell_prefix: Vec<String>,
    pub ssh_daemon: Option<String>,
    pub login_shell: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub profile: String,
    pub profile_chain: Vec<String>,
    pub workspace: String,
    pub identity: Option<ResolvedIdentity>,
    pub handoff: DoctorHandoff,
    pub action_count: usize,
    pub checks: Vec<DoctorCheck>,
    pub ok: bool,
}

impl DoctorReport {
    pub fn failed_checks(&self) -> impl Iterator<Item = &DoctorCheck> {
        self.checks
            .iter()
            .filter(|check| check.status == DoctorStatus::Fail)
    }
}

pub fn inspect(loaded: &LoadedConfig, context: &RuntimeContext) -> DoctorReport {
    let config = loaded.config();
    let mut checks = Vec::new();
    let workspace = context.cwd();
    check_workspace(workspace, &mut checks);

    let identity = match IdentityResolver::new().resolve(config, context) {
        Ok(identity) => {
            checks.push(DoctorCheck {
                name: "identity".to_owned(),
                status: DoctorStatus::Pass,
                message: format!(
                    "target {}:{} ({}) with HOME {}",
                    identity.uid,
                    identity.gid,
                    identity.user,
                    identity.home.display()
                ),
                error: None,
            });
            Some(identity)
        }
        Err(error) => {
            checks.push(DoctorCheck {
                name: "identity".to_owned(),
                status: DoctorStatus::Fail,
                message: "identity resolution failed".to_owned(),
                error: Some(error),
            });
            None
        }
    };

    let runtime = Path::new(&config.handoff.runtime);
    let runtime_executable = executable_file(runtime);
    checks.push(DoctorCheck {
        name: "handoff.runtime".to_owned(),
        status: if runtime_executable {
            DoctorStatus::Pass
        } else {
            DoctorStatus::Fail
        },
        message: if runtime_executable {
            format!("{} is executable", runtime.display())
        } else {
            format!("{} is not an executable file", runtime.display())
        },
        error: None,
    });

    if let Some(path) = &config.handoff.login_shell {
        check_executable("handoff.login_shell", path, &mut checks);
    }
    if let Some(path) = &config.handoff.ssh_daemon {
        check_executable("handoff.ssh_daemon", path, &mut checks);
    }

    let mut action_status = DoctorStatus::Pass;
    for action in loaded.plan().actions() {
        if action.kind == ActionKind::ServiceSshPrepare {
            let keygen = loaded
                .config()
                .actions
                .iter()
                .find(|candidate| candidate.id == action.id)
                .and_then(|candidate| candidate.ssh_keygen.as_deref())
                .map(SshCapability::new)
                .unwrap_or_default();
            let available = keygen.available();
            if !available {
                action_status = DoctorStatus::Fail;
            }
            checks.push(DoctorCheck {
                name: format!("action.{}", action.id),
                status: if available {
                    DoctorStatus::Pass
                } else {
                    DoctorStatus::Fail
                },
                message: if available {
                    format!(
                        "SSH preparation capability {} is available",
                        keygen.keygen().display()
                    )
                } else {
                    format!(
                        "SSH preparation requires an executable ssh-keygen at {}",
                        keygen.keygen().display()
                    )
                },
                error: None,
            });
        }
        if (action.phase == PlanPhase::Root || action.run_as == RunAs::Root) && !is_effective_root()
        {
            action_status = DoctorStatus::Fail;
            checks.push(DoctorCheck {
                name: format!("action.{}", action.id),
                status: DoctorStatus::Fail,
                message: "action requires effective root".to_owned(),
                error: None,
            });
        }
        if action.kind == ActionKind::ProcessSetUserShell {
            if let Some(shell) = loaded
                .config()
                .actions
                .iter()
                .find(|candidate| candidate.id == action.id)
                .and_then(|candidate| candidate.shell.as_deref())
            {
                if !executable_file(Path::new(shell)) {
                    action_status = DoctorStatus::Fail;
                    checks.push(DoctorCheck {
                        name: format!("action.{}", action.id),
                        status: DoctorStatus::Fail,
                        message: format!("login shell {shell} is not executable"),
                        error: None,
                    });
                }
            }
        }
    }
    if loaded.plan().actions().is_empty() {
        checks.push(DoctorCheck {
            name: "actions".to_owned(),
            status: DoctorStatus::Warn,
            message: "profile declares no bootstrap actions".to_owned(),
            error: None,
        });
    } else if action_status == DoctorStatus::Pass {
        checks.push(DoctorCheck {
            name: "actions".to_owned(),
            status: DoctorStatus::Pass,
            message: format!("{} validated actions", loaded.plan().actions().len()),
            error: None,
        });
    }

    let handoff = DoctorHandoff {
        runtime: config.handoff.runtime.clone(),
        runtime_executable,
        exec_prefix: config.handoff.exec_prefix.clone(),
        shell_prefix: config.handoff.shell_prefix.clone(),
        ssh_daemon: config.handoff.ssh_daemon.clone(),
        login_shell: config.handoff.login_shell.clone(),
    };
    let ok = checks
        .iter()
        .all(|check| check.status != DoctorStatus::Fail);
    DoctorReport {
        profile: loaded.profile().to_owned(),
        profile_chain: loaded
            .profile_chain()
            .map(|profile| profile.id.clone())
            .collect(),
        workspace: workspace.to_string_lossy().into_owned(),
        identity,
        handoff,
        action_count: loaded.plan().actions().len(),
        checks,
        ok,
    }
}

fn check_workspace(path: &Path, checks: &mut Vec<DoctorCheck>) {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => checks.push(DoctorCheck {
            name: "workspace".to_owned(),
            status: if is_writable(path) {
                DoctorStatus::Pass
            } else {
                DoctorStatus::Warn
            },
            message: if is_writable(path) {
                format!("{} exists and is writable", path.display())
            } else {
                format!("{} exists but is not writable", path.display())
            },
            error: None,
        }),
        Ok(_) => checks.push(DoctorCheck {
            name: "workspace".to_owned(),
            status: DoctorStatus::Fail,
            message: format!("{} is not a directory", path.display()),
            error: None,
        }),
        Err(error) => checks.push(DoctorCheck {
            name: "workspace".to_owned(),
            status: DoctorStatus::Fail,
            message: format!("{} cannot be inspected", path.display()),
            error: Some(CoreError::Io {
                action: Some("doctor.workspace".to_owned()),
                path: Some(path.to_path_buf()),
                source: error,
            }),
        }),
    }
}

fn check_executable(name: &str, path: &str, checks: &mut Vec<DoctorCheck>) {
    let executable = executable_file(Path::new(path));
    checks.push(DoctorCheck {
        name: name.to_owned(),
        status: if executable {
            DoctorStatus::Pass
        } else {
            DoctorStatus::Fail
        },
        message: if executable {
            format!("{path} is executable")
        } else {
            format!("{path} is not an executable file")
        },
        error: None,
    });
}

fn executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn is_effective_root() -> bool {
    #[cfg(unix)]
    {
        PosixSystem::new().current_ids().0 == 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}
