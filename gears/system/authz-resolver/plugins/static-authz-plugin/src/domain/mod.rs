// Updated: 2026-10-06 by Constructor Tech
//! Domain layer for the static `AuthZ` resolver plugin.

mod client;
pub mod service;

pub use service::{GrantValue, PropertyGrant, Service};
