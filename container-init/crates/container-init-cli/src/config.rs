use crate::args::CliOptions;
use crate::error::CliError;
use bootstrap_loader::{LoadedBootstrap, ProfileLoader, ProfileSource};
use bootstrap_model::{Plan, SourceKind};
use container_init_core::RuntimeContext;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_PROFILES_DIR: &str = "/etc/dev-env/profiles.d";
pub const DEFAULT_PROFILE_FILE: &str = "/etc/dev-env/default-profile";

pub struct LoadedConfig {
    profile: String,
    bootstrap: LoadedBootstrap,
    plan: Plan,
}

impl LoadedConfig {
    pub fn profile(&self) -> &str {
        &self.profile
    }

    pub fn bootstrap(&self) -> &LoadedBootstrap {
        &self.bootstrap
    }

    pub fn config(&self) -> &bootstrap_model::BootstrapConfig {
        self.bootstrap.config()
    }

    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    pub fn profile_chain(&self) -> impl Iterator<Item = &bootstrap_loader::LoadedProfile> {
        self.bootstrap.profile_chain().iter()
    }
}

pub fn load(options: &CliOptions) -> Result<LoadedConfig, CliError> {
    let profiles_dir = options
        .profiles_dir
        .clone()
        .or_else(|| env_path("CONTAINER_INIT_PROFILE_DIR"))
        .or_else(|| env_path("CONTAINER_INIT_PROFILES_DIR"))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PROFILES_DIR));
    let admin_profiles_dir = options
        .admin_profiles_dir
        .clone()
        .or_else(|| env_path("CONTAINER_INIT_ADMIN_PROFILES_DIR"));

    let mut loader = ProfileLoader::new();
    load_sources(&mut loader, &profiles_dir, SourceKind::ImageProfile)?;
    if let Some(directory) = admin_profiles_dir {
        load_sources(&mut loader, &directory, SourceKind::AdminProfile)?;
    }

    let profile = if let Some(profile) = options.profile.clone() {
        profile
    } else if let Ok(profile) = env::var("CONTAINER_INIT_PROFILE") {
        profile
    } else if let Ok(profile) = env::var("CONTAINER_INIT_PROFILE_ID") {
        profile
    } else {
        let default_file = options
            .default_profile_file
            .clone()
            .or_else(|| env_path("CONTAINER_INIT_DEFAULT_PROFILE"))
            .or_else(|| env_path("CONTAINER_INIT_DEFAULT_PROFILE_FILE"))
            .unwrap_or_else(|| PathBuf::from(DEFAULT_PROFILE_FILE));
        read_default_profile(default_file)?
    };

    let bootstrap = loader.load(&profile)?;
    for (name, _) in &options.inputs {
        let declared = bootstrap.config().inputs.contains_key(name)
            || bootstrap
                .config()
                .inputs
                .values()
                .any(|input| input.aliases.iter().any(|alias| alias == name));
        if !declared {
            return Err(CliError::Configuration(format!(
                "runtime input {name:?} is not declared by profile {profile:?}"
            )));
        }
    }
    let plan = bootstrap.build_plan().map_err(CliError::Model)?;
    Ok(LoadedConfig {
        profile,
        bootstrap,
        plan,
    })
}

pub fn runtime_context(
    options: &CliOptions,
    change_directory: bool,
) -> Result<RuntimeContext, CliError> {
    let workspace = workspace_path(options)?;
    if change_directory {
        env::set_current_dir(&workspace).map_err(|source| {
            CliError::io("change working directory", Some(workspace.clone()), source)
        })?;
    }
    let mut context = RuntimeContext::new(&workspace).with_environment(env::vars());
    for (name, value) in &options.inputs {
        context = context.with_cli_input(name.clone(), value.clone());
    }
    Ok(context)
}

pub fn workspace_path(options: &CliOptions) -> Result<PathBuf, CliError> {
    let path = options
        .workspace
        .clone()
        .or_else(|| env_path("CONTAINER_INIT_WORKSPACE"))
        .or_else(|| env_path("WORKSPACE"))
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(env::current_dir()
            .map_err(|source| CliError::io("read current directory", None, source))?
            .join(path))
    }
}

fn load_sources(
    loader: &mut ProfileLoader,
    path: &Path,
    source: SourceKind,
) -> Result<(), CliError> {
    if path.is_file() {
        loader.add_profile(ProfileSource::from_file(path, source)?)?;
        return Ok(());
    }
    let mut entries = fs::read_dir(path)
        .map_err(|source_error| {
            CliError::io(
                "read profile directory",
                Some(path.to_path_buf()),
                source_error,
            )
        })?
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|source_error| {
                CliError::io(
                    "read profile directory entry",
                    Some(path.to_path_buf()),
                    source_error,
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();
    for entry in entries {
        if entry.extension().and_then(|extension| extension.to_str()) == Some("toml") {
            loader.add_profile(ProfileSource::from_file(&entry, source.clone())?)?;
        }
    }
    Ok(())
}

fn read_default_profile(path: PathBuf) -> Result<String, CliError> {
    let contents = fs::read_to_string(&path)
        .map_err(|source| CliError::io("read default profile", Some(path.clone()), source))?;
    let profile = contents.trim();
    if profile.is_empty() || profile.chars().any(char::is_whitespace) {
        return Err(CliError::Configuration(format!(
            "default profile file {} must contain one profile id",
            path.display()
        )));
    }
    Ok(profile.to_owned())
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}
