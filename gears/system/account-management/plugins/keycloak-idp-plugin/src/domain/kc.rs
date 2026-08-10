//! Keycloak admin client + token cache (DESIGN §4.4).

pub mod client;
pub mod factory;
pub mod token_cache;
pub mod transport;

pub(crate) use client::KcAdminClient;
