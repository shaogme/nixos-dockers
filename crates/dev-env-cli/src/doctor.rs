use crate::config::LoadedConfig;
use dev_env_model::MissingProviderPolicy;
use dev_env_provider::{ExecutableLocator, SystemExecutableLocator};
use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorIoKind {
    PermissionDenied,
    NotFound,
    NotDirectory,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "issue_kind")]
pub enum DoctorIssue {
    WorkspaceIo {
        kind: DoctorIoKind,
    },
    WorkspaceNotDirectory,
    WorkspaceNotWritable,
    ShellNotExecutable {
        shell: String,
        command: String,
    },
    ProviderNotFound {
        provider: String,
        executable: String,
    },
    ProviderLookup {
        provider: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub message: String,
    pub issue: Option<DoctorIssue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub profile: String,
    pub profile_chain: Vec<String>,
    pub workspace: String,
    pub cwd: String,
    pub config_fingerprint: String,
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

pub fn inspect(loaded: &LoadedConfig) -> DoctorReport {
    let mut checks = Vec::new();
    check_workspace(loaded.workspace(), &mut checks);

    for (id, shell) in &loaded.config().shells {
        let path = Path::new(&shell.command);
        if executable_file(path) {
            checks.push(DoctorCheck {
                name: format!("shell.{id}"),
                status: DoctorStatus::Pass,
                message: format!("{} is executable", path.display()),
                issue: None,
            });
        } else {
            checks.push(DoctorCheck {
                name: format!("shell.{id}"),
                status: DoctorStatus::Fail,
                message: format!("{} is not an executable file", path.display()),
                issue: Some(DoctorIssue::ShellNotExecutable {
                    shell: id.clone(),
                    command: shell.command.clone(),
                }),
            });
        }
    }

    let locator = SystemExecutableLocator;
    let environment = crate::config::ambient_environment();
    for (id, provider) in &loaded.config().providers {
        if !provider.detect_files.is_empty() {
            checks.push(DoctorCheck {
                name: format!("provider.{id}"),
                status: DoctorStatus::Warn,
                message: "provider applicability depends on workspace files".to_owned(),
                issue: None,
            });
            continue;
        }
        match locator.locate(&provider.executable, &environment) {
            Ok(Some(path)) => checks.push(DoctorCheck {
                name: format!("provider.{id}"),
                status: DoctorStatus::Pass,
                message: format!("{} resolves to {}", provider.executable, path.display()),
                issue: None,
            }),
            Ok(None) => {
                let (status, issue) = match provider.missing {
                    MissingProviderPolicy::Error => (
                        DoctorStatus::Fail,
                        Some(DoctorIssue::ProviderNotFound {
                            provider: id.clone(),
                            executable: provider.executable.clone(),
                        }),
                    ),
                    MissingProviderPolicy::Warn => (
                        DoctorStatus::Warn,
                        Some(DoctorIssue::ProviderNotFound {
                            provider: id.clone(),
                            executable: provider.executable.clone(),
                        }),
                    ),
                    MissingProviderPolicy::Ignore => (DoctorStatus::Pass, None),
                };
                checks.push(DoctorCheck {
                    name: format!("provider.{id}"),
                    status,
                    message: format!("provider executable {} was not found", provider.executable),
                    issue,
                });
            }
            Err(_) => checks.push(DoctorCheck {
                name: format!("provider.{id}"),
                status: DoctorStatus::Fail,
                message: "provider executable lookup failed".to_owned(),
                issue: Some(DoctorIssue::ProviderLookup {
                    provider: id.clone(),
                }),
            }),
        }
    }

    let config_fingerprint = serde_json::to_vec(loaded.config())
        .map(|value| dev_env_provider::fingerprint_bytes(&value))
        .map(hex_digest)
        .unwrap_or_else(|_| "<unavailable>".to_owned());
    let ok = checks
        .iter()
        .all(|check| check.status != DoctorStatus::Fail);
    DoctorReport {
        profile: loaded.profile().to_owned(),
        profile_chain: loaded
            .profile_chain()
            .iter()
            .map(|profile| profile.id.clone())
            .collect(),
        workspace: loaded.workspace().display().to_string(),
        cwd: loaded.cwd().display().to_string(),
        config_fingerprint,
        checks,
        ok,
    }
}

fn check_workspace(path: &Path, checks: &mut Vec<DoctorCheck>) {
    match fs::metadata(path) {
        Ok(metadata) if !metadata.is_dir() => checks.push(DoctorCheck {
            name: "workspace".to_owned(),
            status: DoctorStatus::Fail,
            message: format!("{} is not a directory", path.display()),
            issue: Some(DoctorIssue::WorkspaceNotDirectory),
        }),
        Ok(metadata) if !writable(&metadata) => checks.push(DoctorCheck {
            name: "workspace".to_owned(),
            status: DoctorStatus::Warn,
            message: format!("{} exists but may not be writable", path.display()),
            issue: Some(DoctorIssue::WorkspaceNotWritable),
        }),
        Ok(_) => checks.push(DoctorCheck {
            name: "workspace".to_owned(),
            status: DoctorStatus::Pass,
            message: format!("{} exists and is writable", path.display()),
            issue: None,
        }),
        Err(error) => checks.push(DoctorCheck {
            name: "workspace".to_owned(),
            status: DoctorStatus::Fail,
            message: format!("{} cannot be inspected", path.display()),
            issue: Some(DoctorIssue::WorkspaceIo {
                kind: io_kind(error.kind()),
            }),
        }),
    }
}

fn executable_file(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                !metadata.permissions().readonly()
            }
        }
        _ => false,
    }
}

fn writable(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        (unsafe { libc::geteuid() == 0 }) || metadata.permissions().mode() & 0o222 != 0
    }
    #[cfg(not(unix))]
    {
        !metadata.permissions().readonly()
    }
}

fn io_kind(kind: std::io::ErrorKind) -> DoctorIoKind {
    match kind {
        std::io::ErrorKind::PermissionDenied => DoctorIoKind::PermissionDenied,
        std::io::ErrorKind::NotFound => DoctorIoKind::NotFound,
        std::io::ErrorKind::NotADirectory => DoctorIoKind::NotDirectory,
        _ => DoctorIoKind::Other,
    }
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
