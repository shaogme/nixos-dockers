use crate::error::CoreError;
use crate::identity::ResolvedIdentity;
use bootstrap_model::HandoffConfig;
use serde::Serialize;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HandoffCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl HandoffCommand {
    pub(crate) fn from_config(
        handoff: &HandoffConfig,
        command: &[String],
    ) -> Result<Self, CoreError> {
        let mut args = if command.is_empty() {
            handoff.shell_prefix.clone()
        } else {
            handoff.exec_prefix.clone()
        };
        for argument in command {
            if argument.is_empty() || argument.contains('\0') {
                return Err(CoreError::Invalid {
                    location: "handoff.command".to_owned(),
                    message: "command arguments must be non-empty and contain no NUL".to_owned(),
                });
            }
            args.push(argument.clone());
        }
        Ok(Self {
            program: PathBuf::from(&handoff.runtime),
            args,
        })
    }

    pub fn argv(&self) -> Vec<String> {
        let mut argv = Vec::with_capacity(self.args.len() + 1);
        argv.push(self.program.to_string_lossy().into_owned());
        argv.extend(self.args.iter().cloned());
        argv
    }

    /// Replace the current process with the configured runtime.
    pub fn exec(&self) -> Result<(), CoreError> {
        self.exec_with_identity(None)
    }

    /// Replace the current process with the configured runtime and expose the
    /// resolved target identity through the conventional login environment.
    ///
    /// `container-init` changes credentials in-place before handoff. The
    /// inherited environment still contains the launcher identity, however,
    /// so runtimes that resolve user configuration through HOME would otherwise
    /// continue looking under the old user's home directory.
    pub fn exec_with_identity(&self, identity: Option<&ResolvedIdentity>) -> Result<(), CoreError> {
        let mut command = std::process::Command::new(&self.program);
        command.args(&self.args);
        if let Some(identity) = identity {
            command
                .env("HOME", &identity.home)
                .env("USER", &identity.user)
                .env("LOGNAME", &identity.user);
        }
        let source = command.exec();
        Err(CoreError::Handoff {
            program: self.program.clone(),
            args: self.args.clone(),
            source,
        })
    }

    /// Replace the current process with a root-owned service handoff. The
    /// daemon keeps root credentials, but receives a canonical root login
    /// environment instead of the target development user's environment.
    pub fn exec_as_root_service(&self) -> Result<(), CoreError> {
        let mut command = std::process::Command::new(&self.program);
        command
            .args(&self.args)
            .env("HOME", "/root")
            .env("USER", "root")
            .env("LOGNAME", "root");
        let source = command.exec();
        Err(CoreError::Handoff {
            program: self.program.clone(),
            args: self.args.clone(),
            source,
        })
    }
}
