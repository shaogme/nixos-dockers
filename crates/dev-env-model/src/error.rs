use serde::Serialize;
use std::error::Error;
use std::fmt;

use crate::condition::ConditionError;
use crate::input::InputError;
use crate::path::PathRenderError;
use crate::provider::ProviderError;
use crate::shell::{ShellArgError, ShellEnvParseError};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelErrorReason {
    Empty,
    Nul,
    Newline,
    InvalidMapKey,
    InvalidIdentifier,
    InvalidPath,
    AbsolutePathRequired,
    PathTraversal,
    ShellMetacharacter,
    Duplicate,
    Missing,
    Unsupported,
    MustBeScalar,
    MustBeNonEmpty,
    MustBeExplicit,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum ModelError {
    UnsupportedSchema {
        found: u32,
        expected: u32,
    },
    InvalidProfileId {
        id: String,
    },
    InvalidProfileReference {
        profile: String,
        reference: String,
    },
    DuplicateProfileId {
        id: String,
    },
    MissingParentProfile {
        profile: String,
        parent: String,
    },
    ProfileCycle {
        profiles: Vec<String>,
    },
    UnknownDefaultProfile {
        id: String,
    },
    InvalidValue {
        location: String,
        reason: ModelErrorReason,
    },
    InvalidEnvironmentName {
        location: String,
        name: String,
    },
    InvalidEnvironmentValue {
        location: String,
        reason: ModelErrorReason,
    },
    InvalidPath {
        location: String,
        reason: ModelErrorReason,
    },
    InvalidShell {
        shell: String,
        reason: ShellArgError,
    },
    MissingDefaultShell {
        shell: String,
    },
    DuplicateShell {
        shell: String,
    },
    InvalidProvider {
        provider: String,
        reason: ProviderError,
    },
    MissingProviderDependency {
        provider: String,
        dependency: String,
    },
    ProviderDependencyCycle {
        providers: Vec<String>,
    },
    InvalidInput {
        name: String,
        reason: InputError,
    },
    UnknownInput {
        name: String,
    },
    InvalidCondition {
        location: String,
        reason: ConditionError,
    },
    InvalidShellEnv {
        provider: String,
        reason: ShellEnvParseError,
    },
    InvalidPathRender {
        template: String,
        reason: PathRenderError,
    },
    InvalidOverride {
        path: String,
        reason: ModelErrorReason,
    },
    WorkspaceOverrideNotAllowed {
        path: String,
    },
    CliOverrideNotAllowed {
        path: String,
    },
    PreferChildPolicyNotTrusted,
}

impl fmt::Display for ModelErrorReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Empty => "may not be empty",
            Self::Nul => "may not contain NUL",
            Self::Newline => "may not contain a newline",
            Self::InvalidMapKey => "contains an invalid map key",
            Self::InvalidIdentifier => "is not a valid identifier",
            Self::InvalidPath => "is not a valid path",
            Self::AbsolutePathRequired => "must be an absolute path",
            Self::PathTraversal => "may not contain '..' path traversal",
            Self::ShellMetacharacter => "may not contain shell metacharacters",
            Self::Duplicate => "contains a duplicate value",
            Self::Missing => "is missing",
            Self::Unsupported => "is unsupported",
            Self::MustBeScalar => "must be a scalar value",
            Self::MustBeNonEmpty => "must contain at least one value",
            Self::MustBeExplicit => "must use an explicit operation",
        };
        formatter.write_str(text)
    }
}

impl fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchema { found, expected } => {
                write!(formatter, "unsupported schema {found}; expected {expected}")
            }
            Self::InvalidProfileId { id } => write!(formatter, "invalid profile id {id:?}"),
            Self::InvalidProfileReference { profile, reference } => write!(
                formatter,
                "profile {profile:?} has invalid extends reference {reference:?}"
            ),
            Self::DuplicateProfileId { id } => write!(formatter, "duplicate profile id {id:?}"),
            Self::MissingParentProfile { profile, parent } => write!(
                formatter,
                "profile {profile:?} extends missing profile {parent:?}"
            ),
            Self::ProfileCycle { profiles } => {
                write!(
                    formatter,
                    "profile inheritance cycle: {}",
                    profiles.join(" -> ")
                )
            }
            Self::UnknownDefaultProfile { id } => {
                write!(formatter, "default profile {id:?} does not exist")
            }
            Self::InvalidValue { location, reason } => write!(formatter, "{location}: {reason}"),
            Self::InvalidEnvironmentName { location, name } => {
                write!(formatter, "{location}: invalid environment name {name:?}")
            }
            Self::InvalidEnvironmentValue { location, reason } => {
                write!(formatter, "{location}: invalid environment value: {reason}")
            }
            Self::InvalidPath { location, reason } => write!(formatter, "{location}: {reason}"),
            Self::InvalidShell { shell, reason } => {
                write!(formatter, "shell {shell:?}: {reason}")
            }
            Self::MissingDefaultShell { shell } => {
                write!(formatter, "default shell {shell:?} is not configured")
            }
            Self::DuplicateShell { shell } => write!(formatter, "duplicate shell {shell:?}"),
            Self::InvalidProvider { provider, reason } => {
                write!(formatter, "provider {provider:?}: {reason}")
            }
            Self::MissingProviderDependency {
                provider,
                dependency,
            } => write!(
                formatter,
                "provider {provider:?} depends on missing provider {dependency:?}"
            ),
            Self::ProviderDependencyCycle { providers } => write!(
                formatter,
                "provider dependency cycle: {}",
                providers.join(" -> ")
            ),
            Self::InvalidInput { name, reason } => write!(formatter, "input {name:?}: {reason}"),
            Self::UnknownInput { name } => write!(formatter, "input {name:?} is not declared"),
            Self::InvalidCondition { location, reason } => {
                write!(formatter, "{location}: {reason}")
            }
            Self::InvalidShellEnv { provider, reason } => {
                write!(formatter, "provider {provider:?} shellenv: {reason}")
            }
            Self::InvalidPathRender { template, reason } => {
                write!(formatter, "path template {template:?}: {reason}")
            }
            Self::InvalidOverride { path, reason } => {
                write!(formatter, "override {path:?}: {reason}")
            }
            Self::WorkspaceOverrideNotAllowed { path } => {
                write!(formatter, "workspace may not override {path:?}")
            }
            Self::CliOverrideNotAllowed { path } => {
                write!(formatter, "CLI may not override {path:?}")
            }
            Self::PreferChildPolicyNotTrusted => {
                formatter.write_str("prefer-child merge policy requires a trusted source")
            }
        }
    }
}

impl Error for ModelError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidShell { reason, .. } => Some(reason),
            Self::InvalidProvider { reason, .. } => Some(reason),
            Self::InvalidInput { reason, .. } => Some(reason),
            Self::InvalidCondition { reason, .. } => Some(reason),
            Self::InvalidShellEnv { reason, .. } => Some(reason),
            Self::InvalidPathRender { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

impl From<ConditionError> for ModelError {
    fn from(reason: ConditionError) -> Self {
        Self::InvalidCondition {
            location: "condition".to_owned(),
            reason,
        }
    }
}

impl From<PathRenderError> for ModelError {
    fn from(reason: PathRenderError) -> Self {
        Self::InvalidPathRender {
            template: "<path>".to_owned(),
            reason,
        }
    }
}
