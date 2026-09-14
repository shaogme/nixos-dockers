use dev_env_model::ValueTree;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Runtime facts made available to provider argv templates and conditions.
/// The map is the environment explicitly selected by the materializer; the
/// provider runtime does not inspect or serialize a second environment source.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProviderContext {
    pub workspace: PathBuf,
    pub cwd: PathBuf,
    pub shell: String,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub config: ValueTree,
    #[serde(default)]
    pub workspace_writable: bool,
    #[serde(default)]
    pub workspace_config_present: bool,
    #[serde(default)]
    pub user_id: u32,
    #[serde(default)]
    pub config_fingerprint: [u8; 32],
    #[serde(default)]
    pub provider_enabled: bool,
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
    if let Ok(metadata) = std::fs::metadata(path) {
        if metadata.permissions().readonly() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            return metadata.permissions().mode() & 0o222 != 0;
        }
        #[cfg(not(unix))]
        {
            return true;
        }
    }
    false
}
