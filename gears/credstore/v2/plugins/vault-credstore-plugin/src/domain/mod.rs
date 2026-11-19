// Created: 2026-09-23 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! SDK adapter and KV v2 logic for the Vault / `OpenBao` backend.
//!
//! The HTTP exchange is abstracted behind the [`transport::VaultTransport`]
//! port; its `reqwest`-backed implementation lives in [`crate::infra`].

mod client;
pub mod retry;
pub mod service;
pub mod transport;
mod wire;

pub use service::Service;
