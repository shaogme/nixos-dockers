use dev_env_model::ProviderConfig;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum DetectionError {
    ReadWorkspace {
        path: PathBuf,
        source: io::Error,
    },
    LocateExecutable {
        executable: String,
        source: io::Error,
    },
}

impl std::fmt::Display for DetectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadWorkspace { path, source } => {
                write!(formatter, "could not inspect {}: {source}", path.display())
            }
            Self::LocateExecutable { executable, source } => {
                write!(formatter, "could not locate {executable:?}: {source}")
            }
        }
    }
}

impl std::error::Error for DetectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadWorkspace { source, .. } | Self::LocateExecutable { source, .. } => {
                Some(source)
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetectionResult {
    pub executable: Option<PathBuf>,
    pub matched_files: Vec<PathBuf>,
    pub applicable: bool,
}

pub trait ExecutableLocator: Send + Sync {
    fn locate(
        &self,
        executable: &str,
        environment: &BTreeMap<String, String>,
    ) -> Result<Option<PathBuf>, io::Error>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemExecutableLocator;

impl ExecutableLocator for SystemExecutableLocator {
    fn locate(
        &self,
        executable: &str,
        environment: &BTreeMap<String, String>,
    ) -> Result<Option<PathBuf>, io::Error> {
        let executable_path = Path::new(executable);
        if executable_path.components().count() > 1 {
            return Ok(is_executable(executable_path).then(|| executable_path.to_path_buf()));
        }
        let path = environment
            .get("PATH")
            .map(OsString::from)
            .or_else(|| std::env::var_os("PATH"));
        let Some(path) = path else {
            return Ok(None);
        };
        for directory in std::env::split_paths(&path) {
            let directory = if directory.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                directory
            };
            let candidate = directory.join(executable);
            if is_executable(&candidate) {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }
}

pub fn detect(
    config: &ProviderConfig,
    workspace: &Path,
    environment: &BTreeMap<String, String>,
    locator: &dyn ExecutableLocator,
) -> Result<DetectionResult, DetectionError> {
    let matched_files = detect_files(workspace, &config.detect_files)?;
    // A file-gated provider is simply not applicable outside a matching
    // workspace.  This is distinct from an applicable provider whose binary
    // is missing and must follow `missing = error|warn|ignore`.
    if !config.detect_files.is_empty() && matched_files.is_empty() {
        return Ok(DetectionResult {
            executable: None,
            matched_files,
            applicable: false,
        });
    }
    let executable = locator
        .locate(&config.executable, environment)
        .map_err(|source| DetectionError::LocateExecutable {
            executable: config.executable.clone(),
            source,
        })?;
    Ok(DetectionResult {
        applicable: executable.is_some(),
        executable,
        matched_files,
    })
}

fn detect_files(workspace: &Path, patterns: &[String]) -> Result<Vec<PathBuf>, DetectionError> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    if !workspace.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    let mut candidates = Vec::new();
    for pattern in patterns {
        if !has_glob_magic(pattern) {
            let candidate = workspace.join(pattern);
            if candidate.is_file() {
                candidates.push(candidate);
            }
        }
    }
    if candidates.len() == patterns.len() {
        candidates.sort();
        candidates.dedup();
        return Ok(candidates);
    }

    walk_workspace(workspace, &mut paths)?;
    paths.sort();
    paths.dedup();
    Ok(paths
        .into_iter()
        .filter(|path| {
            let Ok(relative) = path.strip_prefix(workspace) else {
                return false;
            };
            let relative = relative
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            patterns
                .iter()
                .any(|pattern| glob_matches(pattern, &relative))
        })
        .collect())
}

fn walk_workspace(directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), DetectionError> {
    let entries = std::fs::read_dir(directory).map_err(|source| DetectionError::ReadWorkspace {
        path: directory.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| DetectionError::ReadWorkspace {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|source| DetectionError::ReadWorkspace {
                path: path.clone(),
                source,
            })?;
        if metadata.is_dir() {
            walk_workspace(&path, output)?;
        } else if metadata.is_file() {
            output.push(path);
        }
    }
    Ok(())
}

fn has_glob_magic(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?')
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.split('/').collect::<Vec<_>>();
    let path = path.split('/').collect::<Vec<_>>();
    glob_components(&pattern, &path)
}

fn glob_components(pattern: &[&str], path: &[&str]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            glob_components(&pattern[1..], path)
                || (!path.is_empty() && glob_components(pattern, &path[1..]))
        }
        (Some(component), Some(path_component)) => {
            component_matches(component, path_component)
                && glob_components(&pattern[1..], &path[1..])
        }
        _ => false,
    }
}

fn component_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    let mut table = vec![vec![None; value.len() + 1]; pattern.len() + 1];
    fn visit(
        pattern: &[char],
        value: &[char],
        i: usize,
        j: usize,
        table: &mut [Vec<Option<bool>>],
    ) -> bool {
        if let Some(result) = table[i][j] {
            return result;
        }
        let result = if i == pattern.len() {
            j == value.len()
        } else if pattern[i] == '*' {
            visit(pattern, value, i + 1, j, table)
                || (j < value.len() && visit(pattern, value, i, j + 1, table))
        } else {
            j < value.len()
                && (pattern[i] == '?' || pattern[i] == value[j])
                && visit(pattern, value, i + 1, j + 1, table)
        };
        table[i][j] = Some(result);
        result
    }
    visit(&pattern, &value, 0, 0, &mut table)
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
