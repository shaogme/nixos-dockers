use serde::{Deserialize, Serialize};

use crate::{action::Action, error::ModelError};

pub const MAX_HANDOFF_PREFIX_ITEMS: usize = 64;
pub const MAX_HANDOFF_PREFIX_ITEM_BYTES: usize = 64 * 1024;
pub const MAX_HANDOFF_PREFIX_TOTAL_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HandoffConfig {
    runtime: String,
    #[serde(default)]
    exec_prefix: Vec<String>,
    /// Prefix used for an explicit initial command when the backend starts.
    #[serde(default)]
    initial_exec_prefix: Vec<String>,
    #[serde(default)]
    shell_prefix: Vec<String>,
    /// Keep the handoff runtime as a privileged service supervisor.
    #[serde(default)]
    root_service: bool,
    ssh_daemon: Option<String>,
    login_shell: Option<String>,
}

impl HandoffConfig {
    pub fn new(runtime: impl Into<String>) -> Self {
        Self {
            runtime: runtime.into(),
            exec_prefix: Vec::new(),
            initial_exec_prefix: Vec::new(),
            shell_prefix: Vec::new(),
            root_service: false,
            ssh_daemon: None,
            login_shell: None,
        }
    }

    pub fn runtime(&self) -> &str {
        &self.runtime
    }

    pub fn set_runtime(&mut self, runtime: impl Into<String>) {
        self.runtime = runtime.into();
    }

    pub fn exec_prefix(&self) -> &[String] {
        &self.exec_prefix
    }

    pub fn set_exec_prefix(&mut self, exec_prefix: Vec<String>) {
        self.exec_prefix = exec_prefix;
    }

    pub fn initial_exec_prefix(&self) -> &[String] {
        &self.initial_exec_prefix
    }

    pub fn set_initial_exec_prefix(&mut self, initial_exec_prefix: Vec<String>) {
        self.initial_exec_prefix = initial_exec_prefix;
    }

    pub fn shell_prefix(&self) -> &[String] {
        &self.shell_prefix
    }

    pub fn set_shell_prefix(&mut self, shell_prefix: Vec<String>) {
        self.shell_prefix = shell_prefix;
    }

    pub fn root_service(&self) -> bool {
        self.root_service
    }

    pub fn set_root_service(&mut self, root_service: bool) {
        self.root_service = root_service;
    }

    pub fn ssh_daemon(&self) -> &Option<String> {
        &self.ssh_daemon
    }

    pub fn set_ssh_daemon(&mut self, ssh_daemon: Option<String>) {
        self.ssh_daemon = ssh_daemon;
    }

    pub fn login_shell(&self) -> &Option<String> {
        &self.login_shell
    }

    pub fn set_login_shell(&mut self, login_shell: Option<String>) {
        self.login_shell = login_shell;
    }

    pub(super) fn validate(&self) -> Result<(), ModelError> {
        Action::validate_executable("bootstrap.handoff.runtime", &self.runtime)?;
        Self::validate_prefix("exec_prefix", &self.exec_prefix)?;
        Self::validate_prefix("initial_exec_prefix", &self.initial_exec_prefix)?;
        Self::validate_prefix("shell_prefix", &self.shell_prefix)?;
        if let Some(path) = &self.ssh_daemon {
            Action::validate_executable("bootstrap.handoff.ssh_daemon", path)?;
        }
        if let Some(path) = &self.login_shell {
            Action::validate_executable("bootstrap.handoff.login_shell", path)?;
        }
        Ok(())
    }

    fn validate_prefix(field: &str, prefix: &[String]) -> Result<(), ModelError> {
        let location = format!("bootstrap.handoff.{field}");
        if prefix.len() > MAX_HANDOFF_PREFIX_ITEMS {
            return Err(ModelError::Invalid {
                location,
                message: format!(
                    "contains {} arguments; maximum is {MAX_HANDOFF_PREFIX_ITEMS}",
                    prefix.len()
                ),
            });
        }
        let mut total_bytes = 0_usize;
        for (index, argument) in prefix.iter().enumerate() {
            Action::validate_argv_value(&format!("bootstrap.handoff.{field}[{index}]"), argument)?;
            if argument.len() > MAX_HANDOFF_PREFIX_ITEM_BYTES {
                return Err(ModelError::Invalid {
                    location: format!("bootstrap.handoff.{field}[{index}]"),
                    message: format!(
                        "is {} bytes; maximum is {MAX_HANDOFF_PREFIX_ITEM_BYTES} bytes",
                        argument.len()
                    ),
                });
            }
            total_bytes =
                total_bytes
                    .checked_add(argument.len())
                    .ok_or_else(|| ModelError::Invalid {
                        location: location.clone(),
                        message: format!(
                            "total argument bytes exceed {MAX_HANDOFF_PREFIX_TOTAL_BYTES}"
                        ),
                    })?;
        }
        if total_bytes > MAX_HANDOFF_PREFIX_TOTAL_BYTES {
            return Err(ModelError::Invalid {
            location,
            message: format!(
                "contains {total_bytes} argument bytes; maximum is {MAX_HANDOFF_PREFIX_TOTAL_BYTES} bytes"
            ),
        });
        }
        Ok(())
    }
}
