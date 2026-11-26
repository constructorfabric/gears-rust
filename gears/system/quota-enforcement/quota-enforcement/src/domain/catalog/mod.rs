//! The metric catalogue (`features/projection-contracts.md`, ADR-0012): one
//! entry per metric type QE has read, served or unavailable, and the cache of
//! registered subject types.
//!
//! - [`model`]: an entry and the checks that build it.
//! - [`metric_catalog`]: the catalogue, filled on first use.

pub mod metric_catalog;
pub mod model;

pub use metric_catalog::{MetricCatalog, UnknownMemory};
pub use model::{CatalogEntry, MetricMode, ServedMetric};
