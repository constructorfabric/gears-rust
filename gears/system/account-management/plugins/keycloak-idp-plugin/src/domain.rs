//! Domain layer for the VHP `IdP` Plugin.
//!
//! Contains the plugin-owned system
//! [`SecurityContext`](toolkit_security::SecurityContext) ([`system_actor`])
//! and the ctx-baked-in `CredStore` seams ([`credstore`]).

pub mod audit;
pub mod credstore;
pub mod error;
pub mod kc;
pub mod metadata_codec;
pub mod metrics;
pub mod ports;
pub mod service_principal_facade;
pub mod system_actor;
pub mod tenant_facade;
pub mod user_facade;

#[cfg(test)]
pub(crate) mod test_support;
