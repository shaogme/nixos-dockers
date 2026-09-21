use crate::error::LoaderError;
use dev_env_model::ProfileDocument;

/// A typed document plus the unprojected TOML tree. The tree lets the merger
/// distinguish an omitted field from a serde default such as `merge = strict`.
#[derive(Clone, Debug)]
pub(crate) struct RawProfile {
    pub(crate) document: ProfileDocument,
    pub(crate) root: toml::Value,
}

pub(crate) fn parse_profile(contents: &str, location: String) -> Result<RawProfile, LoaderError> {
    let root: toml::Value = toml::from_str(contents).map_err(|source| LoaderError::Parse {
        location: location.clone(),
        source,
    })?;
    // A profile may also carry the independent Bootstrap DSL. Project that
    // namespace out before deserializing the environment document; the
    // container-init loader performs the inverse projection for bootstrap.
    // Keeping this at the loader boundary lets both runtimes consume the
    // same profile file without making either model depend on the other.
    let mut environment_root = root.clone();
    if let Some(table) = environment_root.as_table_mut() {
        table.remove("bootstrap");
    }
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
    Ok(RawProfile { document, root })
}

pub(crate) fn table<'a>(value: &'a toml::Value, key: &str) -> Option<&'a toml::value::Table> {
    value.as_table()?.get(key)?.as_table()
}
