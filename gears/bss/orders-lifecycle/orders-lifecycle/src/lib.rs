//! Orders Lifecycle wiring scaffold. No business HTTP routes are enabled.
#![forbid(unsafe_code)]
// D-184: `AccessScope::allow_all` is a disallowed method (crate-local clippy.toml); the only
// permitted use is the read-only maintenance `DiscoveryScope` constructor.
#![deny(clippy::disallowed_methods)]
pub mod api;
pub mod authz;
pub mod config;
mod domain;
pub mod gear;
pub mod gts;
mod infra;
pub use bss_orders_lifecycle_sdk as sdk;
pub use gear::BssOrdersLifecycleGear;
