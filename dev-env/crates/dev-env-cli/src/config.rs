use crate::args::CliOptions;
use crate::error::{CliError, ConfigurationError, IoOperation};
use dev_env_core::RuntimeContext;
use dev_env_loader::{LoadedConfig as LoaderConfig, ProfileLoader, ProfileSource, SourceKind};
use dev_env_model::WorkspaceSearch;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

pub const DEFAULT_PROFILES_DIR: &str = "/etc/dev-env/profiles.d";
pub const DEFAULT_PROFILE_FILE: &str = "/etc/dev-env/default-profile";
pub const DEFAULT_ADMIN_CONFIG: &str = "/etc/dev-env/config.toml";

pub struct LoadedConfig {
    profile: String,
    loaded: LoaderConfig,
    workspace: PathBuf,
    cwd: PathBuf,
    workspace_config_present: bool,
    user_id: u32,
}

impl LoadedConfig {
    pub fn profile(&self) -> &str {
        &self.profile
    }

    pub fn config(&self) -> &dev_env_model::ResolvedConfig {
        self.loaded.config()
    }

    pub fn profile_chain(&self) -> &[dev_env_loader::LoadedProfile] {
        self.loaded.profile_chain()
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn workspace_config_present(&self) -> bool {
        self.workspace_config_present
    }

    pub fn user_id(&self) -> u32 {
        self.user_id
    }
}

pub fn load(options: &CliOptions) -> Result<LoadedConfig, CliError> {
    let mut loader = ProfileLoader::new();
    let profiles_dir = options
        .profiles_dir
        .clone()
        .or_else(|| env_path("DEVENV_PROFILES_DIR"))
        .or_else(|| env_path("DEVENV_PROFILE_DIR"))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PROFILES_DIR));
    load_sources(&mut loader, &profiles_dir, SourceKind::ImageProfile)?;

    if let Some(admin_profiles_dir) = options
        .admin_profiles_dir
        .clone()
        .or_else(|| env_path("DEVENV_ADMIN_PROFILES_DIR"))
    {
        load_sources(&mut loader, &admin_profiles_dir, SourceKind::AdminProfile)?;
    }

    let profile = select_profile(options)?;
    // Resolve the profile once before workspace overlays.  The profile root
    // is the only safe starting point from which untrusted workspace files
    // may be discovered.
    let base = loader.load(&profile)?;
    let original_cwd = current_directory()?;
    let cwd_source = options.cwd.clone().or_else(|| env_path("DEVENV_CWD"));
    let cwd = resolve_path(cwd_source.as_deref(), &original_cwd);
    let workspace_source = options
        .workspace
        .clone()
        .or_else(|| env_path("DEVENV_WORKSPACE"))
        .unwrap_or_else(|| PathBuf::from(&base.config().workspace.root));
    let workspace = resolve_path(Some(&workspace_source), &original_cwd);
    if !cwd.starts_with(&workspace) {
        return Err(ConfigurationError::CwdOutsideWorkspace { cwd, workspace }.into());
    }

    let mut overlays = Vec::new();
    let admin_config =
        env_path("DEVENV_ADMIN_CONFIG").unwrap_or_else(|| PathBuf::from(DEFAULT_ADMIN_CONFIG));
    if let Some(source) = optional_source(&admin_config, SourceKind::AdminProfile, "admin-config")?
    {
        overlays.push(source);
    }
    if let Some(path) = user_config_path() {
        if let Some(source) = optional_source(&path, SourceKind::UserOverlay, "user-config")? {
            overlays.push(source);
        }
    }

    let explicit_config = options.config.clone().or_else(|| env_path("DEVENV_CONFIG"));
    let has_explicit_config = explicit_config.is_some();
    let (workspace_overlays, workspace_config_present) = collect_workspace_overlays(
        &workspace,
        &cwd,
        base.config().workspace.search,
        explicit_config.as_deref(),
    )?;
    overlays.extend(workspace_overlays);

    if let Some(path) = explicit_config {
        overlays.push(required_source(
            &path,
            SourceKind::WorkspaceOverlay,
            "explicit-config",
        )?);
    }

    let ambient = ambient_environment();
    let patches = options.patches.clone();
    let loaded =
        loader.load_with_overlays_runtime_and_cli(&profile, overlays.iter(), ambient, patches)?;

    Ok(LoadedConfig {
        profile,
        loaded,
        workspace,
        cwd,
        workspace_config_present: workspace_config_present || has_explicit_config,
        user_id: options.user_id.unwrap_or_else(effective_user_id),
    })
}

pub fn runtime_context(
    loaded: &LoadedConfig,
    shell: Option<&str>,
) -> Result<RuntimeContext, CliError> {
    let process_environment = ambient_environment();
    let context = match shell {
        Some(shell) => RuntimeContext::new(
            loaded.workspace.clone(),
            loaded.cwd.clone(),
            shell.to_owned(),
            process_environment,
        ),
        None => RuntimeContext::without_shell(
            loaded.workspace.clone(),
            loaded.cwd.clone(),
            process_environment,
        ),
    };
    Ok(context
        .with_user_id(loaded.user_id())
        .with_workspace_config_present(loaded.workspace_config_present()))
}

fn select_profile(options: &CliOptions) -> Result<String, CliError> {
    if let Some(profile) = &options.profile {
        return Ok(profile.clone());
    }
    if let Some(profile) = env_string("DEVENV_PROFILE").or_else(|| env_string("DEVENV_PROFILE_ID"))
    {
        return Ok(profile);
    }
    let path = options
        .default_profile_file
        .clone()
        .or_else(|| env_path("DEVENV_DEFAULT_PROFILE"))
        .or_else(|| env_path("DEVENV_DEFAULT_PROFILE_FILE"))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PROFILE_FILE));
    let contents = fs::read_to_string(&path).map_err(|source| {
        CliError::io(IoOperation::ReadDefaultProfile, Some(path.clone()), source)
    })?;
    let values = contents.split_whitespace().collect::<Vec<_>>();
    let profile = values.first().copied().unwrap_or_default();
    if profile.is_empty() {
        return Err(ConfigurationError::InvalidDefaultProfile {
            path,
            reason: crate::error::DefaultProfileReason::Empty,
        }
        .into());
    }
    if values.len() != 1 {
        return Err(ConfigurationError::InvalidDefaultProfile {
            path,
            reason: crate::error::DefaultProfileReason::MultipleLines,
        }
        .into());
    }
    Ok(profile.to_owned())
}

fn load_sources(
    loader: &mut ProfileLoader,
    path: &Path,
    source: SourceKind,
) -> Result<(), CliError> {
    let metadata = fs::metadata(path).map_err(|source| {
        CliError::io(
            IoOperation::ReadProfileDirectory,
            Some(path.to_path_buf()),
            source,
        )
    })?;
    if metadata.is_file() {
        loader.add_profile(ProfileSource::from_file(path, source)?)?;
        return Ok(());
    }
    let mut entries = fs::read_dir(path)
        .map_err(|source| {
            CliError::io(
                IoOperation::ReadProfileDirectory,
                Some(path.to_path_buf()),
                source,
            )
        })?
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|source| {
                CliError::io(
                    IoOperation::ReadProfileDirectory,
                    Some(path.to_path_buf()),
                    source,
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();
    for entry in entries {
        if entry.extension().and_then(|extension| extension.to_str()) == Some("toml") {
            loader.add_profile(ProfileSource::from_file(&entry, source)?)?;
        }
    }
    Ok(())
}

fn collect_workspace_overlays(
    workspace: &Path,
    cwd: &Path,
    search: WorkspaceSearch,
    explicit_config: Option<&Path>,
) -> Result<(Vec<ProfileSource>, bool), CliError> {
    let mut directories = vec![workspace.to_path_buf()];
    if search == WorkspaceSearch::Upward {
        let mut parents = cwd
            .ancestors()
            .take_while(|directory| *directory != workspace)
            .map(Path::to_path_buf)
            .collect::<Vec<_>>();
        parents.reverse();
        directories.extend(parents);
    }

    let mut overlays = Vec::new();
    let mut present = false;
    for directory in directories {
        let conventional = directory.join(".dev-env.toml");
        let directory_config = directory.join(".dev-env").join("config.toml");
        let conventional_exists = is_file(&conventional)?;
        let directory_config_exists = is_file(&directory_config)?;
        if conventional_exists && directory_config_exists && explicit_config.is_none() {
            return Err(ConfigurationError::AmbiguousWorkspaceConfig {
                directory,
                conventional,
                directory_config,
            }
            .into());
        }
        let config = match (conventional_exists, directory_config_exists) {
            (true, true) => explicit_config.and_then(|path| {
                if path == conventional {
                    Some(conventional.clone())
                } else if path == directory_config {
                    Some(directory_config.clone())
                } else {
                    None
                }
            }),
            (true, false) => Some(conventional),
            (false, true) => Some(directory_config),
            (false, false) => None,
        };
        if let Some(path) = config {
            overlays.push(required_source(
                &path,
                SourceKind::WorkspaceOverlay,
                "workspace-config",
            )?);
            present = true;
        }
        let local = directory.join(".dev-env.local.toml");
        if is_file(&local)? {
            overlays.push(required_source(
                &local,
                SourceKind::WorkspaceOverlay,
                "workspace-local",
            )?);
            present = true;
        }
    }
    Ok((overlays, present))
}

fn optional_source(
    path: &Path,
    source: SourceKind,
    id: &str,
) -> Result<Option<ProfileSource>, CliError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            Ok(Some(ProfileSource::from_overlay_file(path, source, id)?))
        }
        Ok(_) => Err(CliError::io(
            IoOperation::ReadOverlay,
            Some(path.to_path_buf()),
            io::Error::new(io::ErrorKind::InvalidInput, "overlay path is not a file"),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source_error) => Err(CliError::io(
            IoOperation::ReadOverlay,
            Some(path.to_path_buf()),
            source_error,
        )),
    }
}

fn required_source(path: &Path, source: SourceKind, id: &str) -> Result<ProfileSource, CliError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            Ok(ProfileSource::from_overlay_file(path, source, id)?)
        }
        Ok(_) => Err(CliError::io(
            IoOperation::ReadOverlay,
            Some(path.to_path_buf()),
            io::Error::new(io::ErrorKind::InvalidInput, "overlay path is not a file"),
        )),
        Err(source_error) => Err(CliError::io(
            IoOperation::ReadOverlay,
            Some(path.to_path_buf()),
            source_error,
        )),
    }
}

fn is_file(path: &Path) -> Result<bool, CliError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(CliError::io(
            IoOperation::ReadOverlay,
            Some(path.to_path_buf()),
            source,
        )),
    }
}

fn user_config_path() -> Option<PathBuf> {
    let root = env_path("XDG_CONFIG_HOME")
        .or_else(|| env_path("HOME").map(|home| home.join(".config")))?;
    Some(root.join("dev-env").join("config.toml"))
}

fn current_directory() -> Result<PathBuf, CliError> {
    env::current_dir()
        .map_err(|source| CliError::io(IoOperation::ReadCurrentDirectory, None, source))
}

fn resolve_path(path: Option<&Path>, base: &Path) -> PathBuf {
    let path = path.unwrap_or(base);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    normalize_path(&absolute)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                result.push(component.as_os_str())
            }
        }
    }
    result
}

pub(crate) fn ambient_environment() -> BTreeMap<String, String> {
    env::vars_os()
        // Docker and Podman commonly add lowercase metadata variables such as
        // `container`.  They are process metadata, not portable DSL inputs,
        // and cannot be represented by dev-env's uppercase environment model.
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            if !is_environment_name(&name) {
                return None;
            }
            Some((name, value.into_string().ok()?))
        })
        .collect()
}

fn is_environment_name(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(characters.next(), Some(character) if character.is_ascii_uppercase() || character == '_')
        && characters.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn env_string(name: &str) -> Option<String> {
    env::var_os(name)
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
}

pub(crate) fn effective_user_id() -> u32 {
    #[cfg(unix)]
    {
        // libc is used only for this process fact; providers still receive it
        // through the typed RuntimeContext rather than reading it themselves.
        unsafe { libc::geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::{ambient_environment, is_environment_name};

    #[test]
    fn ambient_environment_excludes_nonportable_metadata_names() {
        let environment = ambient_environment();

        assert!(!environment.contains_key("container"));
        assert!(environment.keys().all(|name| is_environment_name(name)));
    }
}
