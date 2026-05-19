//! Usage Collector SDK
//!
//! Transport-agnostic contracts for the usage-collector module family.
//!
//! ## What this crate provides
//!
//! - [`UsageCollectorClientV1`] — ingest trait implemented by gateway/remote client modules
//!   (passed by constructor argument to the emitter, never via `ClientHub`).
//! - [`UsageCollectorPluginClientV1`] — storage-plugin trait implemented by backend plugins.
//! - [`UsageRecord`], [`UsageKind`], [`ModuleConfig`], [`AllowedMetric`] — ingest-side models.
//! - [`UsageRecordError`] — resource-scoped error builder for usage record operations.
//! - [`UsageCollectorPluginSpecV1`] — GTS schema for storage plugin registration.

// @cpt-dod:cpt-cf-usage-collector-dod-sdk-and-ingest-core-sdk-crate:p1

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod api;
pub mod authz;
pub mod error;
pub mod gts;
pub mod models;
pub mod plugin_api;

pub use api::UsageCollectorClientV1;
pub use error::{ModuleConfigError, UsageCollectorError, UsageRecordError};
pub use gts::UsageCollectorPluginSpecV1;
pub use models::{AllowedMetric, ModuleConfig, Subject, UsageKind, UsageRecord};
pub use plugin_api::UsageCollectorPluginClientV1;
