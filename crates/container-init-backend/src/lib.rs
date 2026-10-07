//! The long-lived, single-instance container-init backend.

mod client;
mod instance;
mod protocol;
mod server;
mod socket;

pub use client::{BackendClient, BackendClientError, PreparedHandoff};
pub use instance::{InstanceClaim, InstanceLock};
pub use protocol::*;
pub use server::{BackendClaim, BackendLease, BackendPaths, BackendRunError};
pub use socket::{
    bind_socket, cleanup_stale_socket, ensure_socket_directory, peer_credentials,
    validate_socket_path,
};
