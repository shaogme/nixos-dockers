use serde::{Deserialize, Serialize};

use crate::{action::Action, condition::PathTemplate, error::ModelError, input::BootstrapInput};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct IdentityConfig {
    default_user: Option<String>,
    default_uid: Option<u32>,
    default_gid: Option<u32>,
    default_home: Option<String>,
    auto_mapping: bool,
    run_as_root_input: Option<String>,
    uid_input: Option<String>,
    gid_input: Option<String>,
    home_input: Option<String>,
}

impl IdentityConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn default_user(&self) -> &Option<String> {
        &self.default_user
    }

    pub fn set_default_user(&mut self, default_user: Option<String>) {
        self.default_user = default_user;
    }

    pub fn default_uid(&self) -> Option<u32> {
        self.default_uid
    }

    pub fn set_default_uid(&mut self, default_uid: Option<u32>) {
        self.default_uid = default_uid;
    }

    pub fn default_gid(&self) -> Option<u32> {
        self.default_gid
    }

    pub fn set_default_gid(&mut self, default_gid: Option<u32>) {
        self.default_gid = default_gid;
    }

    pub fn default_home(&self) -> &Option<String> {
        &self.default_home
    }

    pub fn set_default_home(&mut self, default_home: Option<String>) {
        self.default_home = default_home;
    }

    pub fn auto_mapping(&self) -> bool {
        self.auto_mapping
    }

    pub fn set_auto_mapping(&mut self, auto_mapping: bool) {
        self.auto_mapping = auto_mapping;
    }

    pub fn run_as_root_input(&self) -> &Option<String> {
        &self.run_as_root_input
    }

    pub fn set_run_as_root_input(&mut self, run_as_root_input: Option<String>) {
        self.run_as_root_input = run_as_root_input;
    }

    pub fn uid_input(&self) -> &Option<String> {
        &self.uid_input
    }

    pub fn set_uid_input(&mut self, uid_input: Option<String>) {
        self.uid_input = uid_input;
    }

    pub fn gid_input(&self) -> &Option<String> {
        &self.gid_input
    }

    pub fn set_gid_input(&mut self, gid_input: Option<String>) {
        self.gid_input = gid_input;
    }

    pub fn home_input(&self) -> &Option<String> {
        &self.home_input
    }

    pub fn set_home_input(&mut self, home_input: Option<String>) {
        self.home_input = home_input;
    }

    pub(super) fn validate(&self) -> Result<(), ModelError> {
        if let Some(user) = &self.default_user {
            Action::validate_user_name("bootstrap.identity.default_user", user)?;
        }
        if let Some(home) = &self.default_home {
            PathTemplate::validate_template("bootstrap.identity.default_home", home)?;
        }
        for (field, value) in [
            ("run_as_root_input", &self.run_as_root_input),
            ("uid_input", &self.uid_input),
            ("gid_input", &self.gid_input),
            ("home_input", &self.home_input),
        ] {
            if let Some(name) = value {
                if !BootstrapInput::is_env_name(name) {
                    return Err(ModelError::Invalid {
                        location: format!("bootstrap.identity.{field}"),
                        message: format!("{name:?} is not a valid environment input name"),
                    });
                }
            }
        }
        Ok(())
    }
}
