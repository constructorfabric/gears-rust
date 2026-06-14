//! Usage Collector SDK — public contract surfaces for the `usage-collector` gear:
//!
//! - [`UsageCollectorClientV1`] — consumer SDK trait, obtained from `ClientHub`.
//! - [`UsageCollectorPluginV1`] — storage plugin SPI trait.
//! - [`UsageCollectorPluginSpecV1`] — GTS plugin spec for discovery/binding.
//! - Domain models: [`UsageType`], [`UsageTypeGtsId`], [`UsageRecord`],
//!   [`AggregationResult`], [`ResourceRef`], [`SubjectRef`], [`TimeWindow`]
//!   (validated `[from, to)`), and the aggregation surface ([`AggregationOp`],
//!   [`AggregationDimension`], [`AggregationSpec`], [`AggregationBucket`]).
//!   List pagination uses [`toolkit_odata::ODataQuery`]
//!   / [`toolkit_odata::Page`]. The filterable-field schema for
//!   `list_usage_records` is declared by [`UsageRecordQuery`] (macro-derived
//!   via `ODataFilterable`); dynamic metadata-key filtering rides a typed
//!   [`MetadataFilter`] side channel.
//! - [`UsageCollectorError`] / [`UsageCollectorPluginError`] — flat error envelopes.
//!   This crate does NOT depend on `toolkit-canonical-errors`; the host crate
//!   owns the lift to RFC-9457 `Problem` on the REST surface.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod api;
pub mod error;
pub mod gts;
pub mod models;
pub mod plugin_api;

pub use api::UsageCollectorClientV1;
pub use error::{UsageCollectorError, UsageCollectorPluginError};
pub use gts::UsageCollectorPluginSpecV1;
pub use models::{
    AggregationBucket, AggregationDimension, AggregationOp, AggregationResult, AggregationSpec,
    IdempotencyKey, MetadataFilter, MetadataKey, ResourceRef, SubjectRef, TimeWindow, UsageKind,
    UsageRecord, UsageRecordFilterField, UsageRecordQuery, UsageRecordStatus, UsageType,
    UsageTypeGtsId,
};
pub use plugin_api::UsageCollectorPluginV1;
