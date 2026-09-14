//! Loading and merging for the container-init Bootstrap DSL.
//!
//! The crate stops at a validated [`bootstrap_model::BootstrapConfig`]. It
//! does not inspect the host, execute actions, invoke a shell, or know about
//! any provider.

mod error;
mod loader;
mod merge;
mod raw;
mod source;

pub use error::LoaderError;
pub use loader::{BootstrapLoader, ProfileLoader};
pub use source::{LoadedBootstrap, LoadedProfile, ProfileSource};
