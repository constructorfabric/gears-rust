//! Domain layer for the static `AuthZ` resolver plugin.

mod client;
pub mod service;

pub use service::{GrantValue, PropertyGrant, Service};
