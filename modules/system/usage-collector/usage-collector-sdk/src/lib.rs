//! Usage Collector SDK
//!
//! Transport-agnostic contracts for the usage-collector module family.
//!
//! ## What this crate provides
//!
//! - [`UsageCollectorClientV1`] — ingest trait implemented by gateway/remote client modules
//!   (passed by constructor argument to the emitter, never via `ClientHub`).
//! - [`UsageCollectorPluginClientV1`] — storage-plugin trait implemented by backend plugins.
//!   In addition to ingest, this trait exposes `query_aggregated` and `query_raw`
//!   for the Feature 3 query API.
//! - [`UsageRecord`], [`UsageKind`], [`ModuleConfig`], [`AllowedMetric`] — ingest-side models.
//! - [`AggregationFn`], [`BucketSize`], [`GroupByDimension`], [`AggregationQuery`],
//!   [`AggregationResult`], [`RawQuery`] — query-side models.
//! - [`CursorV1`], [`Page`], [`PageInfo`] — re-exported from `modkit-odata` for
//!   convenience; the query API uses the canonical `OData` keyset cursor and
//!   paginated-result types.
//! - [`UsageCollectorError`] — top-level error returned by ingest and query
//!   operations; alias of `modkit_canonical_errors::CanonicalError`.
//! - [`UsageRecordError`] / [`ModuleConfigError`] — resource-scoped error builders.
//! - [`UsageCollectorPluginSpecV1`] — GTS schema for storage plugin registration.

// @cpt-dod:cpt-cf-usage-collector-dod-sdk-and-ingest-core-sdk-crate:p1

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod api;
pub mod authz;
pub mod cursor_filter;
pub mod error;
pub mod gts;
pub mod models;
pub mod plugin_api;

pub use api::UsageCollectorClientV1;
pub use cursor_filter::{RawQueryFilters, raw_query_effective_order, raw_query_filter_hash};
pub use error::{ModuleConfigError, UsageCollectorError, UsageRecordError};
pub use gts::UsageCollectorPluginSpecV1;
pub use models::{
    AggregationFn, AggregationQuery, AggregationResult, AllowedMetric, BucketSize,
    GroupByDimension, ModuleConfig, RawQuery, Subject, UsageKind, UsageRecord,
};
pub use modkit_odata::{CursorV1, Page, PageInfo, SortDir};
pub use plugin_api::UsageCollectorPluginClientV1;
