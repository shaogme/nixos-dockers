use container_init_posix::WorkspaceObservation;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::io;
use std::path::{Path, PathBuf};

/// Runtime data supplied by the CLI or the process environment.
///
/// Only names declared by the Bootstrap DSL are consumed as typed bootstrap
/// inputs. The rest of the ambient environment is available to condition
/// evaluation only when explicitly requested by a condition.
#[derive(Clone, Debug, Default)]
pub struct RuntimeContext {
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    cli_inputs: BTreeMap<String, String>,
    features: BTreeSet<String>,
    workspace_observation: Option<WorkspaceObservation>,
}

impl RuntimeContext {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            ..Self::default()
        }
    }

    pub fn current() -> io::Result<Self> {
        Ok(Self::new(env::current_dir()?).with_environment(env::vars()))
    }

    pub fn with_environment<I, K, V>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.env = values
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect();
        self
    }

    pub fn with_cli_input(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.cli_inputs.insert(name.into(), value.into());
        self
    }

    pub fn with_feature(mut self, feature: impl Into<String>) -> Self {
        self.features.insert(feature.into());
        self
    }

    pub fn with_workspace_observation(mut self, observation: WorkspaceObservation) -> Self {
        self.workspace_observation = Some(observation);
        self
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.env
    }

    pub fn cli_inputs(&self) -> &BTreeMap<String, String> {
        &self.cli_inputs
    }

    pub fn features(&self) -> &BTreeSet<String> {
        &self.features
    }

    pub fn env(&self, name: &str) -> Option<&str> {
        self.env.get(name).map(String::as_str)
    }

    pub fn cli_input(&self, name: &str) -> Option<&str> {
        self.cli_inputs.get(name).map(String::as_str)
    }

    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.contains(feature)
    }

    pub fn workspace_observation(&self) -> Option<&WorkspaceObservation> {
        self.workspace_observation.as_ref()
    }
}
