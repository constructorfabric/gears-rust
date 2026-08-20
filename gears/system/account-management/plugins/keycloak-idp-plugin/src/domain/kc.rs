//! Keycloak admin client + token cache (DESIGN "Component Model").

pub mod client;
pub mod factory;
pub mod token_cache;
pub mod transport;

pub use client::KcAdminClient;
