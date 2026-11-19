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

#[cfg(test)]
pub(crate) mod test_ext {
    /// Test-only predicates on the plugin error.
    pub trait PluginErrorExt {
        fn is_unavailable(&self) -> bool;
        fn is_credentials_rejected(&self) -> bool;
    }

    impl PluginErrorExt for credstore_sdk::CredStorePluginError {
        fn is_unavailable(&self) -> bool {
            matches!(self, Self::Unavailable { .. })
        }
        fn is_credentials_rejected(&self) -> bool {
            matches!(self, Self::CredentialsRejected { .. })
        }
    }
}
