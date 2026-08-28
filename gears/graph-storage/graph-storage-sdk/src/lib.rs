//! Graph Storage SDK
//!
//! Public contract of the graph-storage gear:
//!
//! - [`GraphStorageClientV1`] — the `ClientHub` trait other gears consume;
//! - transport-agnostic models (no serde derives, no HTTP, no DB types);
//! - the three plugin contracts an external team implements against
//!   ([`plugin_api::GraphStoreV1`], [`plugin_api::GraphEngineV1`],
//!   [`plugin_api::EmbeddingProviderV1`]);
//! - GTS identifiers owned by the gear.
//!
//! Consumers obtain the client from `ClientHub`:
//! ```ignore
//! let client = hub.get::<dyn `GraphStorageClientV1`>()?;
//! ```

#![forbid(unsafe_code)]

pub mod client;
/// Executable plugin contracts for implementors; see [`contract`].
#[cfg(feature = "test-support")]
pub mod contract;
pub mod gts;
pub mod models;
pub mod plugin_api;

pub use client::GraphStorageClientV1;
pub use models::*;
