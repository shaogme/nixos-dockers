//! container-init 的长驻单实例后端。

mod client;
mod instance;
mod server;
mod socket;

pub use client::{BackendClient, BackendClientError, CommitResult, PreparedHandoff};
pub use instance::{InstanceClaim, InstanceLock};
pub use server::{BackendClaim, BackendLease, BackendPaths, BackendRunError, BackendStartOptions};
pub use socket::BackendSocket;
