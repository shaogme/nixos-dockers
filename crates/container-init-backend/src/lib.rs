//! container-init 的长驻单实例后端。

mod client;
mod server;

pub use client::{
    BackendClient, BackendClientError, CommitResult, PrepareOptions, PreparedHandoff,
};
pub use server::{BackendClaim, BackendLease, BackendPaths, BackendRunError, BackendStartOptions};
