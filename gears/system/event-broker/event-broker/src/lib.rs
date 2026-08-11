//! Event Broker module: `Ingest`/`Delivery`/`Dispatcher` composite, wired
//! per deployment mode by [`module::EventBrokerModule`].
//!
//! See `docs/DESIGN.md` (module tree, §3.8 Deployment Topology, §4.1
//! Deployment Modes) and `docs/ADR/0007-service-decomposition.md`.
//!
//! A `test_support` module for shared test fixtures (`DESIGN.md:614`) joins
//! this tree once #4346/#4347 need shared test doubles.

pub mod api;
pub mod config;
pub mod domain;
pub mod infra;
pub mod module;

pub use config::{DeploymentMode, EventBrokerConfig};
pub use module::EventBrokerModule;
