//! Pure model for the `dev-env` environment DSL.
//!
//! This crate intentionally does not read files, inspect the process, execute
//! providers, or launch shells.  It contains the serializable schema, typed
//! input and expression parsers, provenance types, and static invariants used
//! by the loader and materializer crates.

mod condition;
mod config;
mod environment;
mod error;
mod input;
mod materialized;
mod path;
mod provenance;
mod provider;
mod shell;
mod validation;
mod value;

pub use condition::{Condition, ConditionError, ConditionValue};
pub use config::{
    ConfigLayer, EnvironmentLayer, MergePolicy, OverrideOperation, OverrideSpec, PolicyConfig,
    ProfileDocument, ProfileSet, ResolvedConfig, ShellSelection, UnknownInputPolicy,
    UntrustedWorkspacePolicy, WorkspaceConfig, WorkspaceSearch,
};
pub use environment::{
    ConditionalEnvironmentVariable, ConfiguredValuePrecedence, EnvironmentConfig, EnvironmentPath,
    PathMode,
};
pub use error::{ModelError, ModelErrorReason};
pub use input::{InputError, InputSpec, InputType, InputValue, ParsedInput};
pub use materialized::{EnvValue, MaterializedEnv, ProviderReceipt};
pub use path::{PathRenderError, PathTemplate};
pub use provenance::{
    ConfigValue, Layer, Origin, ProvenanceEntry, ProvenanceIndex, Sensitivity, SourceId,
};
pub use provider::{
    FailurePolicy, MissingProviderPolicy, PrepareStep, ProviderConfig, ShellEnvConfig,
    ShellEnvFormat,
};
pub use shell::{ShellArgError, ShellConfig, ShellEnvEntry, ShellEnvParseError, ShellKind};
pub use value::ValueTree;

pub type Value = ValueTree;
pub type Profile = ProfileDocument;
pub type PathConfig = EnvironmentPath;

pub const DEV_ENV_SCHEMA_V1: u32 = 1;
