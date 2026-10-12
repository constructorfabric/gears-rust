//! Graph Storage gear.
//!
//! A stateless gateway over a pluggable store: the gear holds no graph state
//! of its own, and every byte it serves comes from a
//! [`graph_storage_sdk::plugin_api::GraphStoreV1`] implementation behind the
//! port. The built-in `PostgreSQL` store and engine live in `infra` for
//! packaging convenience, not as a privilege.
//!
//! The public API is defined in `graph-storage-sdk` and re-exported here.

// === PUBLIC API (from SDK) ===
pub use graph_storage_sdk::{GraphStorageClientV1, models, plugin_api};

// === MODULE DEFINITION ===
pub mod gear;
pub use gear::GraphStorage;

// === INTERNAL MODULES ===
#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod infra;
