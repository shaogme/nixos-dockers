use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    #[default]
    Public,
    Sensitive,
    Secret,
}

/// The source of one declaration.  The payload is data, not a rendered
/// error, so callers can still inspect and format it as JSON or text.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum SourceId {
    Profile(String),
    File(String),
    Environment(String),
    Cli,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    #[default]
    Profile,
    Admin,
    User,
    Workspace,
    Runtime,
    Cli,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Origin {
    pub source: SourceId,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub layer: Layer,
    pub reason: Option<String>,
}

impl Origin {
    pub fn profile(id: impl Into<String>) -> Self {
        Self {
            source: SourceId::Profile(id.into()),
            line: None,
            column: None,
            layer: Layer::Profile,
            reason: None,
        }
    }

    pub fn file(path: impl Into<String>, layer: Layer) -> Self {
        Self {
            source: SourceId::File(path.into()),
            line: None,
            column: None,
            layer,
            reason: None,
        }
    }

    pub fn environment(name: impl Into<String>) -> Self {
        Self {
            source: SourceId::Environment(name.into()),
            line: None,
            column: None,
            layer: Layer::Runtime,
            reason: None,
        }
    }

    pub fn cli() -> Self {
        Self {
            source: SourceId::Cli,
            line: None,
            column: None,
            layer: Layer::Cli,
            reason: None,
        }
    }
}

impl Default for Origin {
    fn default() -> Self {
        Self {
            source: SourceId::Unknown,
            line: None,
            column: None,
            layer: Layer::Profile,
            reason: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConfigValue<T> {
    pub value: T,
    pub origin: Origin,
    pub sensitivity: Sensitivity,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProvenanceIndex {
    entries: BTreeMap<String, ProvenanceEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProvenanceEntry {
    pub path: String,
    pub origins: Vec<Origin>,
    pub sensitivity: Sensitivity,
}

impl ProvenanceIndex {
    pub fn insert(&mut self, path: impl Into<String>, origin: Origin, sensitivity: Sensitivity) {
        let path = path.into();
        let entry = self
            .entries
            .entry(path.clone())
            .or_insert_with(|| ProvenanceEntry {
                path,
                origins: Vec::new(),
                sensitivity,
            });
        entry.sensitivity = entry.sensitivity.max(sensitivity);
        entry.origins.push(origin);
    }

    pub fn get(&self, path: &str) -> Option<&ProvenanceEntry> {
        self.entries.get(path)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ProvenanceEntry)> {
        self.entries
            .iter()
            .map(|(path, entry)| (path.as_str(), entry))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
