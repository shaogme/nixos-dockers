use dev_env_model::ModelError;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Facts for one materialization session.
///
/// The process environment is supplied explicitly so callers can decide which
/// environment source is visible to the runtime and tests do not depend on
/// the host process.  `shell = None` selects `config.shell.default`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeContext {
    pub workspace: PathBuf,
    pub cwd: PathBuf,
    pub shell: Option<String>,
    pub process_environment: BTreeMap<String, String>,
    pub user_id: u32,
    pub workspace_writable: Option<bool>,
    pub workspace_config_present: bool,
}

/// Shorter name for callers that use the terminology from the design docs.
pub type MaterializeContext = RuntimeContext;

impl RuntimeContext {
    pub fn new(
        workspace: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
        shell: impl Into<String>,
        process_environment: BTreeMap<String, String>,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            cwd: cwd.into(),
            shell: Some(shell.into()),
            process_environment,
            user_id: 0,
            workspace_writable: None,
            workspace_config_present: false,
        }
    }

    pub fn without_shell(
        workspace: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
        process_environment: BTreeMap<String, String>,
    ) -> Self {
        let mut context = Self::new(workspace, cwd, "", process_environment);
        context.shell = None;
        context
    }

    pub fn current(
        workspace: impl Into<PathBuf>,
        shell: impl Into<String>,
    ) -> Result<Self, ContextError> {
        let cwd = std::env::current_dir().map_err(ContextError::CurrentDirectory)?;
        // Container runtimes may inject metadata such as `container=...`.
        // The environment DSL intentionally models portable, uppercase shell
        // names, so discard host metadata that cannot be represented there.
        let process_environment = std::env::vars()
            .filter(|(name, _)| is_environment_name(name))
            .collect();
        Ok(Self::new(workspace, cwd, shell, process_environment))
    }

    pub fn with_shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = Some(shell.into());
        self
    }

    pub fn with_user_id(mut self, user_id: u32) -> Self {
        self.user_id = user_id;
        self
    }

    pub fn with_workspace_writable(mut self, writable: bool) -> Self {
        self.workspace_writable = Some(writable);
        self
    }

    pub fn with_workspace_config_present(mut self, present: bool) -> Self {
        self.workspace_config_present = present;
        self
    }

    pub fn validate(&self) -> Result<(), ContextError> {
        validate_absolute("workspace", &self.workspace)?;
        validate_absolute("cwd", &self.cwd)?;
        if !self.cwd.starts_with(&self.workspace) {
            return Err(ContextError::CwdOutsideWorkspace {
                cwd: self.cwd.clone(),
                workspace: self.workspace.clone(),
            });
        }
        for (name, value) in &self.process_environment {
            let candidate = dev_env_model::EnvValue::public(value.clone());
            candidate
                .validate(name)
                .map_err(ContextError::InvalidEnvironment)?;
        }
        if self.shell.as_deref().is_some_and(str::is_empty) {
            return Err(ContextError::EmptyShell);
        }
        Ok(())
    }

    pub(crate) fn writable(&self) -> bool {
        self.workspace_writable
            .unwrap_or_else(|| directory_is_writable(&self.workspace))
    }
}

fn is_environment_name(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(characters.next(), Some(character) if character.is_ascii_uppercase() || character == '_')
        && characters.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextPathError {
    Empty,
    Relative,
    Nul,
}

impl fmt::Display for ContextPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "path may not be empty",
            Self::Relative => "path must be absolute",
            Self::Nul => "path may not contain NUL",
        })
    }
}

impl std::error::Error for ContextPathError {}

#[derive(Debug)]
pub enum ContextError {
    CurrentDirectory(std::io::Error),
    InvalidPath {
        name: &'static str,
        path: PathBuf,
        reason: ContextPathError,
    },
    CwdOutsideWorkspace {
        cwd: PathBuf,
        workspace: PathBuf,
    },
    EmptyShell,
    UnknownShell {
        shell: String,
    },
    InvalidEnvironment(ModelError),
}

impl fmt::Display for ContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory(source) => {
                write!(formatter, "could not determine current directory: {source}")
            }
            Self::InvalidPath { name, path, reason } => {
                write!(formatter, "runtime {name} {}: {reason}", path.display())
            }
            Self::CwdOutsideWorkspace { cwd, workspace } => write!(
                formatter,
                "runtime cwd {} is outside workspace {}",
                cwd.display(),
                workspace.display()
            ),
            Self::EmptyShell => formatter.write_str("runtime shell may not be empty"),
            Self::UnknownShell { shell } => {
                write!(formatter, "runtime shell {shell:?} is not configured")
            }
            Self::InvalidEnvironment(source) => {
                write!(formatter, "runtime environment is invalid: {source}")
            }
        }
    }
}

impl std::error::Error for ContextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CurrentDirectory(source) => Some(source),
            Self::InvalidPath { reason, .. } => Some(reason),
            Self::InvalidEnvironment(source) => Some(source),
            Self::CwdOutsideWorkspace { .. } | Self::EmptyShell | Self::UnknownShell { .. } => None,
        }
    }
}

fn validate_absolute(name: &'static str, path: &Path) -> Result<(), ContextError> {
    let reason = if path.as_os_str().is_empty() {
        Some(ContextPathError::Empty)
    } else if contains_nul(path) {
        Some(ContextPathError::Nul)
    } else if !path.is_absolute() {
        Some(ContextPathError::Relative)
    } else {
        None
    };
    match reason {
        Some(reason) => Err(ContextError::InvalidPath {
            name,
            path: path.to_path_buf(),
            reason,
        }),
        None => Ok(()),
    }
}

fn contains_nul(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().contains(&0)
    }
    #[cfg(not(unix))]
    {
        path.as_os_str().to_string_lossy().contains('\0')
    }
}

fn directory_is_writable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if metadata.permissions().readonly() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o222 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
