//! Usage Collector SDK, generation 2 — the reworked metering contract.
//!
//! The crate root carries generation 1 unchanged for existing consumers; this
//! module is the surface the reworked specification defines. The two share
//! `serde_helpers` alone; everything else is independent.
//!
//! - [`UsageCollectorClientV2`] — consumer SDK trait, obtained from `ClientHub`.
//! - [`UsageCollectorPluginV2`] — storage plugin SPI trait.
//! - [`UsageCollectorPluginSpecV2`] — GTS plugin spec for discovery/binding.

pub mod aggregation;
pub mod api;
pub mod error;
pub mod feed;
pub mod filter;
pub mod gts;
pub mod id;
pub mod keyset;
pub mod models;
pub mod plugin_api;
pub mod quantity;
pub mod reason;
pub mod reconciliation;
pub mod stored;
pub mod time_range;

pub use aggregation::{
    AggregationBucket, AggregationDimension, AggregationFold, AggregationResult,
    MAX_AGGREGATION_BUCKETS,
};
pub use api::UsageCollectorClientV2;
/// The carrier of [`QuantitySummary::Accrued`]; see [`crate::bigdecimal`].
pub use bigdecimal::BigDecimal;
pub use error::{
    AlreadyInvalidatedArgs, ConflictOutcome, CursorField, IngestionQuotaExceededArgs,
    UsageCollectorError, UsageCollectorPluginError,
};
pub use feed::{
    FeedPage, FeedPosition, FeedPositionInvalid, FeedStart, FeedSubscription,
    FeedSubscriptionInvalid, MAX_FEED_POSITION_BYTES, SubscribedMeters, SubscribedMetersInvalid,
};
pub use filter::{
    KEYSET_SAFE_RECORD_FIELDS, MetadataFilter, PUBLISHED_FILTER_FIELDS, UsageRecordFilterField,
    UsageRecordQuery, is_keyset_safe_record_field,
};
pub use gts::{USAGE_RECORD_RESOURCE, UsageCollectorPluginSpecV2};
pub use id::{
    PeriodBoundInvalid, USAGE_RECORD_ID_NAMESPACE, UsageRecordIdInvalid, canonical_period_bound,
    derive_usage_record_id,
};
pub use keyset::{Keyset, KeysetInvalid, MAX_KEYSET_BYTES, MAX_PAGE_SIZE, RecordPage};
pub use models::{
    BACKFILL_ROUTE_PATH, CreateUsageRecord, EntryType, IdempotencyKey, Invalidation, MetadataKey,
    MeterTypeId, RECORD_ID_FIELD, ReasonCode, RecordOrigin, ResourceRef, SubjectRef,
    USAGE_RECORD_BASE_TYPE, UsageRecord, WINDOW_END_FIELD, WINDOW_START_FIELD,
};
pub use plugin_api::{CONSISTENCY_FLOOR_STATEMENT, UsageCollectorPluginV2};
pub use quantity::{MAX_QUANTITY_SIGNIFICANT_DIGITS, UsageQuantity};
pub use reason::{ConflictReason, NotFoundReason, ValidationReason};
pub use reconciliation::{ObservedQuantity, QuantitySummary, ReconciliationMetadata};
pub use stored::{MeterRecordMismatch, MeterRef, MeteredRecord, StoredUsageRecord};
pub use time_range::TimeRange;
