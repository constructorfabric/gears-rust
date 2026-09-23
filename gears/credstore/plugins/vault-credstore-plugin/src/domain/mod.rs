//! HTTP client implementation and SDK adapter for the Vault / `OpenBao` KV v2
//! backend.

mod client;
pub mod service;
mod wire;

pub use service::Service;
