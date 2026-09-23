//! The long-lived, single-instance container-init backend.

mod client;
mod instance;
mod protocol;
mod server;
mod socket;

pub use client::{BackendClient, BackendClientError};
pub use instance::{InstanceClaim, InstanceLock};
pub use protocol::{
    read_message, validate_message, write_message, BackendError, BackendState, BackendStatus,
    ClientMessage, ClientRequest, HelloInfo, PeerCredentials, PreparedHandoff, ProtocolError,
    ReceiptSummary, ServerMessage, ServerResponse, MAX_FRAME_BYTES, PROTOCOL_VERSION,
};
pub use server::{BackendClaim, BackendLease, BackendPaths, BackendRunError};
pub use socket::{
    bind_socket, cleanup_stale_socket, ensure_socket_directory, peer_credentials,
    validate_socket_path,
};
