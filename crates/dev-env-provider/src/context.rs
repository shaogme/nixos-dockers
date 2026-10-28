use dev_env_model::ValueTree;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::metadata,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Runtime facts made available to provider argv templates and conditions.
/// The map is the environment explicitly selected by the materializer; the
/// provider runtime does not inspect or serialize a second environment source.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProviderContext {
    workspace: PathBuf,
    cwd: PathBuf,
    shell: String,
    #[serde(default)]
    environment: BTreeMap<String, String>,
    #[serde(default)]
    config: ValueTree,
    #[serde(default)]
    workspace_writable: bool,
    #[serde(default)]
    workspace_config_present: bool,
    #[serde(default)]
    user_id: u32,
    #[serde(default)]
    config_fingerprint: [u8; 32],
    #[serde(default)]
    provider_enabled: bool,
}

impl ProviderContext {
    pub fn new(
        workspace: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
        shell: impl Into<String>,
        environment: BTreeMap<String, String>,
    ) -> Self {
        let workspace = workspace.into();
        Self {
            workspace_writable: directory_looks_writable(&workspace),
            workspace_config_present: false,
            workspace,
            cwd: cwd.into(),
            shell: shell.into(),
            environment,
            config: ValueTree::default(),
            user_id: 0,
            config_fingerprint: [0; 32],
            provider_enabled: true,
        }
    }

    pub fn with_config(mut self, config: ValueTree) -> Self {
        self.config = config;
        self
    }

    pub fn with_workspace_writable(mut self, writable: bool) -> Self {
        self.workspace_writable = writable;
        self
    }

    pub fn with_workspace_config_present(mut self, present: bool) -> Self {
        self.workspace_config_present = present;
        self
    }

    pub fn with_user_id(mut self, user_id: u32) -> Self {
        self.user_id = user_id;
        self
    }

    pub fn with_config_fingerprint(mut self, fingerprint: [u8; 32]) -> Self {
        self.config_fingerprint = fingerprint;
        self
    }

    pub fn with_provider_enabled(mut self, enabled: bool) -> Self {
        self.provider_enabled = enabled;
        self
    }

    pub fn config_value(&self, path: &str) -> Option<&ValueTree> {
        let mut value = &self.config;
        for component in path.split('.') {
            value = match value {
                ValueTree::Map(values) => values.get(component)?,
                _ => return None,
            };
        }
        Some(value)
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn shell(&self) -> &str {
        &self.shell
    }

    pub fn shell_mut(&mut self) -> &mut String {
        &mut self.shell
    }

    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    pub fn environment_mut(&mut self) -> &mut BTreeMap<String, String> {
        &mut self.environment
    }

    pub fn config(&self) -> &ValueTree {
        &self.config
    }

    pub fn workspace_writable(&self) -> bool {
        self.workspace_writable
    }

    pub fn workspace_config_present(&self) -> bool {
        self.workspace_config_present
    }

    pub fn user_id(&self) -> u32 {
        self.user_id
    }

    pub fn config_fingerprint(&self) -> [u8; 32] {
        self.config_fingerprint
    }

    pub fn provider_enabled(&self) -> bool {
        self.provider_enabled
    }

    pub fn namespace_value(&self, namespace: &str, path: &str) -> Option<&ValueTree> {
        if let Some(value) = self.config_value(&format!("{namespace}.{path}")) {
            return Some(value);
        }
        if namespace == "features" {
            return self.config_value(path);
        }
        None
    }

    pub fn template_value(&self, name: &str) -> Option<String> {
        match name {
            "workspace" => Some(self.workspace.display().to_string()),
            "cwd" => Some(self.cwd.display().to_string()),
            "shell" => Some(self.shell.clone()),
            "user-id" => Some(self.user_id.to_string()),
            "workspace-config-present" => Some(self.workspace_config_present.to_string()),
            "workspace-writable" => Some(self.workspace_writable.to_string()),
            _ => None,
        }
    }

    pub(crate) fn set_workspace_config_present(&mut self, present: bool) {
        self.workspace_config_present = present;
    }
}

fn directory_looks_writable(path: &Path) -> bool {
    if let Ok(metadata) = metadata(path) {
        if metadata.permissions().readonly() {
            return false;
        }
        #[cfg(unix)]
        {
            return metadata.permissions().mode() & 0o222 != 0;
        }
        #[cfg(not(unix))]
        {
            return true;
        }
    }
    false
}
