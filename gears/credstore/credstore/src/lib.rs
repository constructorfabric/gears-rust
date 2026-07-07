//! credstore — `CredStore` module: tenant-scoped secrets over pluggable backends.
pub mod api;
pub mod client;
pub mod config;
pub mod domain;
pub mod gear;
pub mod infra;

pub use gear::CredStoreGear;
