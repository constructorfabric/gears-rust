//! Usage Collector SDK — public contract surfaces for the `usage-collector` gear:
//!
//! - [`UsageCollectorClientV1`] — consumer SDK trait, obtained from `ClientHub`.
//! - [`UsageCollectorPluginV1`] — storage plugin SPI trait.
//! - [`UsageCollectorPluginSpecV1`] — GTS plugin spec for discovery/binding.
//! - Domain models: [`UsageType`], [`UsageTypeGtsId`], [`UsageRecord`],
//!   [`AggregationResult`], [`ResourceRef`], [`SubjectRef`], and the
//!   aggregation surface ([`AggregationOp`], [`AggregationDimension`],
//!   [`AggregationSpec`], [`AggregationBucket`]).
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
pub mod id;
pub mod models;
pub mod plugin_api;
pub mod reason;
pub mod serde_helpers;

/// Generation 2 of the contract — the reworked metering surface. See
/// [`v2`]'s own module documentation. The items re-exported at this crate's
/// root remain generation 1, unchanged, for existing consumers.
pub mod v2;

/// The `bigdecimal` this crate's contract surfaces are typed against,
/// re-exported so a plugin filling [`v2::QuantitySummary::Accrued`] need not
/// declare the dependency and keep its version in step by hand. A version
/// that drifts does not fail to resolve — it compiles two distinct
/// `BigDecimal` types and fails at the call site.
pub use bigdecimal;

pub use api::UsageCollectorClientV1;
pub use error::{UsageCollectorError, UsageCollectorPluginError};
pub use gts::{USAGE_RECORD_RESOURCE, USAGE_TYPE_RESOURCE, UsageCollectorPluginSpecV1};
pub use id::{USAGE_RECORD_ID_NAMESPACE, created_at_micros, derive_usage_record_id};
pub use models::{
    AggregationBucket, AggregationDimension, AggregationOp, AggregationResult, AggregationSpec,
    CreateUsageRecord, IdempotencyKey, MAX_AGGREGATION_BUCKETS, MetadataFilter, MetadataKey,
    ResourceRef, SubjectRef, UsageKind, UsageRecord, UsageRecordFilterField, UsageRecordQuery,
    UsageRecordStatus, UsageType, UsageTypeFilterField, UsageTypeGtsId, UsageTypeQuery,
    is_keyset_safe_record_field, is_keyset_safe_type_field,
};
pub use plugin_api::UsageCollectorPluginV1;
pub use reason::{ConflictReason, ValidationReason};
