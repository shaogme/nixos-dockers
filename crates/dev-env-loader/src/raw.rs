use crate::error::LoaderError;
use dev_env_model::{OverrideSpec, ProfileDocument};
use std::collections::BTreeMap;
use toml::{value::Table, Value};

/// A typed environment document paired with the unprojected TOML tree.
///
/// The tree preserves omitted fields and the independent bootstrap namespace,
/// neither of which is represented by the environment model alone.
#[derive(Clone, Debug)]
pub(crate) struct RawProfile {
    document: ProfileDocument,
    root: Value,
}

impl RawProfile {
    pub(crate) fn parse(contents: &str, location: String) -> Result<Self, LoaderError> {
        let root: Value = contents.parse().map_err(|source| LoaderError::Parse {
            location: location.clone(),
            source,
        })?;
        let document = Self::parse_environment_document(&root, location)?;
        Ok(Self { document, root })
    }

    fn parse_environment_document(
        root: &Value,
        location: String,
    ) -> Result<ProfileDocument, LoaderError> {
        let environment_root = Self::environment_projection(root);
        let document: ProfileDocument =
            environment_root
                .try_into()
                .map_err(|source| LoaderError::Parse {
                    location: location.clone(),
                    source,
                })?;
        document.validate().map_err(|source| LoaderError::Model {
            location: Some(location),
            source,
        })?;
        Ok(document)
    }

    fn environment_projection(root: &Value) -> Value {
        let mut environment_root = root.clone();
        if let Some(table) = environment_root.as_table_mut() {
            table.remove("bootstrap");
        }
        environment_root
    }

    pub(crate) fn id(&self) -> &str {
        self.document.id()
    }

    pub(crate) fn parents(&self) -> &[String] {
        self.document.extends()
    }

    pub(crate) fn overrides(&self) -> &BTreeMap<String, OverrideSpec> {
        self.document.overrides()
    }

    pub(crate) fn table(&self, key: &str) -> Option<&Table> {
        self.root.as_table()?.get(key)?.as_table()
    }

    pub(crate) fn has_section(&self, key: &str) -> bool {
        self.root
            .as_table()
            .is_some_and(|table| table.contains_key(key))
    }

    pub(crate) fn protected_override_path(&self) -> Option<&str> {
        self.overrides()
            .keys()
            .find(|path| Self::is_protected_path(path))
            .map(String::as_str)
    }

    fn is_protected_path(path: &str) -> bool {
        path == "policy"
            || path.starts_with("policy.")
            || path == "inputs"
            || path.starts_with("inputs.")
    }
}
