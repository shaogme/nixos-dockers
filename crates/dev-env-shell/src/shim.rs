use crate::{
    command::CommandLine,
    error::{ShimError, ShimPathError},
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// A compatibility shim keeps the real shell path and original argv
/// separate.  It does not add shell syntax or re-parse the arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shim {
    real: PathBuf,
    configured_command: Option<PathBuf>,
}

impl Shim {
    pub fn new(real: impl Into<PathBuf>) -> Self {
        Self {
            real: real.into(),
            configured_command: None,
        }
    }

    pub fn with_configured_command(mut self, configured: impl Into<PathBuf>) -> Self {
        self.configured_command = Some(configured.into());
        self
    }

    pub fn real(&self) -> &Path {
        &self.real
    }

    pub(crate) fn validate_configured_command(&self, configured: &Path) -> Result<(), ShimError> {
        validate_real_path(&self.real)?;
        if self.real == configured {
            return Err(ShimError::Recursive {
                real: self.real.clone(),
                configured: configured.to_path_buf(),
            });
        }
        Ok(())
    }

    pub fn build(&self, args: &[OsString]) -> Result<CommandLine, ShimError> {
        validate_real_path(&self.real)?;
        if let Some(configured) = &self.configured_command {
            self.validate_configured_command(configured)?;
        }
        CommandLine::try_new(self.real.clone(), args.iter().cloned())
            .map_err(|source| ShimError::CommandLine { source })
    }
}

fn validate_real_path(path: &Path) -> Result<(), ShimError> {
    let reason = if path.as_os_str().is_empty() {
        Some(ShimPathError::Empty)
    } else if contains_nul(path) {
        Some(ShimPathError::Nul)
    } else if !path.is_absolute() {
        Some(ShimPathError::Relative)
    } else {
        None
    };
    match reason {
        Some(reason) => Err(ShimError::InvalidRealPath {
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
