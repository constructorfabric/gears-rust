//! Event Broker module: `Ingest`/`Delivery`/`Dispatcher` composite, wired
//! per deployment mode by [`module::EventBrokerModule`].
//!
//! See `docs/DESIGN.md` (module tree, §3.8 Deployment Topology, §4.1
//! Deployment Modes) and `docs/ADR/0007-service-decomposition.md`.

pub mod api;
pub mod config;
pub mod domain;
pub mod infra;
pub mod module;

// Exposed to other crates under `test-utils` (the SDK's integration tests use
// the harness); `#[cfg(test)]` alone still covers this crate's own unit tests.
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support;

pub use config::{DeploymentMode, EventBrokerConfig};
pub use module::EventBrokerModule;
