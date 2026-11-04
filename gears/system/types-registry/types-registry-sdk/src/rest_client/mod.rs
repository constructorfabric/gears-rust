//! The REST transport (`rest-client`): both clients, their directory-resolving wrappers and
//! their private wire DTOs.
//!
//! The semantic models stay transport-free; only this module knows the wire.

mod client;
mod platform_client;
mod resolving;
mod transport;
mod wire;

pub use client::{TENANT_BASE_PATH, TypesRegistryApiRestClient};
pub use platform_client::{PLATFORM_BASE_PATH, PlatformTypesRegistryApiRestClient};
pub use resolving::{
    PlatformTypesRegistryApiRestResolvingClient, TypesRegistryApiRestResolvingClient,
};
