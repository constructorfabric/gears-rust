//! SDK adapter and KV v2 logic for the Vault / `OpenBao` backend.
//!
//! The HTTP exchange is abstracted behind the [`transport::VaultTransport`]
//! port; its `reqwest`-backed implementation lives in [`crate::infra`].

mod client;
pub mod service;
pub mod transport;
mod wire;

pub use service::Service;
