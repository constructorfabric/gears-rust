// @cpt-begin:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-0
//! Wire contract for time-range parameters used in this module's query types:
//!
//! - `DateTime<Utc>` is the only accepted wire representation for `from`,
//!   `to`, and `time_range` fields. RFC 3339 strings carrying a non-zero
//!   offset (offset-aware datetimes) MUST be rejected by the gateway with
//!   `400 Bad Request` before this struct is constructed.
//! - `page_size`: when absent in the incoming request, the gateway MUST
//!   substitute `DEFAULT_PAGE_SIZE` before constructing [`RawQuery`].
// @cpt-end:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-0

// @cpt-begin:cpt-cf-usage-collector-algo-query-api-sdk-types:p2:inst-sdk-6
//!
//! Pagination contract: this crate reuses [`modkit_odata::CursorV1`] and
//! [`modkit_odata::Page`] / [`modkit_odata::PageInfo`] as the cursor and
//! paginated-result types. [`CursorV1`] is an *exclusive lower-bound* keyset
//! cursor whose key encodes `(timestamp, id)`; plugins MUST return records
//! ascending by `(timestamp, id)` strictly greater than the supplied cursor.
//!
//! A cursor whose timestamp falls outside the request's `time_range`
//! SHOULD be rejected by the gateway with `400 Bad Request`. Cursors are
//! stable across retries on an unchanged record set; callers MUST tolerate
//! short pages caused by interleaved deletions because the keyset boundary
//! is preserved even when some referenced rows have been removed.
// @cpt-end:cpt-cf-usage-collector-algo-query-api-sdk-types:p2:inst-sdk-6

use chrono::{DateTime, Utc};
use modkit_odata::CursorV1;
use modkit_security::AccessScope;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Kind of numeric usage observation (gauge vs counter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    Gauge,
    Counter,
}

/// A single allowed metric definition returned by [`crate::UsageCollectorClientV1::get_module_config`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AllowedMetric {
    /// Metric name.
    pub name: String,
    /// Gauge vs counter semantics for this metric.
    pub kind: UsageKind,
}

/// Per-module configuration returned by [`crate::UsageCollectorClientV1::get_module_config`].
///
/// Extensible: future fields may include rate limit config, sampling policies, etc.
///
/// # Fields
///
/// - `allowed_metrics` — metrics this module is allowed to emit.
/// - `max_metadata_bytes` — maximum serialized metadata size in bytes that the
///   emitter will accept on a [`UsageRecord`]. A value of `0` means metadata is
///   **disabled**: any non-`None` metadata payload is rejected by the emitter.
///   The upper bound on this value is enforced by the collector (not by this
///   SDK type); the SDK trusts whatever value the collector sends.
///
/// # Wire compatibility
///
/// `max_metadata_bytes` is a **required** serde field with no `#[serde(default)]`.
/// Payloads from an older collector that omit the field intentionally fail to
/// decode — the in-repo collector and emitter ship together, so this wire-break
/// surfaces a version-skew rather than silently falling back to an unspecified
/// default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ModuleConfig {
    /// Metrics this module is allowed to emit.
    pub allowed_metrics: Vec<AllowedMetric>,
    /// Maximum serialized metadata size in bytes enforced by the emitter on
    /// outgoing [`UsageRecord`]s. `0` disables metadata entirely; the upper
    /// bound is enforced by the collector (not by this SDK type).
    pub max_metadata_bytes: u32,
}

/// Identity of the subject (user or service) performing the metered action.
///
/// Encodes the real invariant: an id is always required, the type is optional.
/// The "type without id" payload is therefore structurally unrepresentable.
///
/// # Wire shape
///
/// Serializes as a nested object:
///
/// ```json
/// { "id": "...", "type": "user" }
/// ```
///
/// The `r#type` raw identifier maps to the plain JSON key `"type"` because
/// serde strips the `r#` prefix automatically; no `#[serde(rename)]` is needed.
///
/// `#[serde(deny_unknown_fields)]` is applied so that nested typos such as
/// `{ "subject": { "idd": "..." } }` fail loudly at decode time rather than
/// silently dropping authorization-relevant context.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Subject {
    /// Subject identifier.
    pub id: Uuid,
    /// Logical type of the subject (e.g. GTS id or domain name).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub r#type: Option<String>,
}

impl Subject {
    /// Construct a [`Subject`] from an id only (no type).
    #[must_use]
    pub fn new(id: Uuid) -> Self {
        Self { id, r#type: None }
    }

    /// Construct a [`Subject`] from an id and a type.
    #[must_use]
    pub fn with_type(id: Uuid, ty: impl Into<String>) -> Self {
        Self {
            id,
            r#type: Some(ty.into()),
        }
    }
}

/// A single usage record submitted to the collector.
///
/// # Field visibility
///
/// All fields are public by design so that emitters, storage plugins, tests,
/// and serde can construct records directly without going through a builder.
/// This is a deliberate trade-off: the SDK keeps the construction surface
/// minimal, and the **emitter is the validation gateway** (see below). Source
/// modules SHOULD NOT construct [`UsageRecord`] directly in production code —
/// use the `usage-emitter` crate, which performs the validation listed below
/// before forwarding to the collector.
///
/// # Validation contract
///
/// This type is an unvalidated data carrier. It does NOT enforce:
///
/// - finite `value` — NaN / ±∞ are accepted by the Rust type. JSON has no
///   representation for non-finite floats, and `serde_json` with default
///   settings encodes them as JSON `null` rather than erroring. The encoded
///   payload then fails to deserialize back into [`UsageRecord`] (or is
///   rejected by the storage backend's schema) — so the operational signal
///   is a *decode-time* error at the next hop, not a clean emit failure.
///   Emitters MUST filter non-finite values before submission; do not rely
///   on the serializer to surface them.
/// - `metadata` size — enforced by the emitter using
///   [`ModuleConfig::max_metadata_bytes`] returned by
///   [`crate::UsageCollectorClientV1::get_module_config`]; a value of `0`
///   disables metadata entirely. Not enforced at this SDK layer.
/// - `idempotency_key` length or format.
///
/// Validation of these invariants is the **emitter's** responsibility — see
/// the `usage-emitter` crate. Storage plugins MAY perform additional defensive
/// checks but should not rely on this type to enforce them.
///
/// # Wire strictness
///
/// `#[serde(deny_unknown_fields)]` is applied: this is an authorization-relevant
/// payload, and silently dropping a typo such as `subject_idd` would downgrade
/// PDP scoping without a visible error. Adding a field to this struct is
/// therefore a wire-breaking change for any older deserializer — bump the
/// type's GTS version when extending the surface.
///
/// For emission from source modules, always use the `usage-emitter` crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageRecord {
    /// Name of the module that emitted this record.
    pub module: String,
    /// Tenant that owns this usage observation.
    pub tenant_id: Uuid,
    /// Metric name for this observation.
    pub metric: String,
    /// Gauge vs counter semantics.
    pub kind: UsageKind,
    /// Numeric value for this usage observation.
    ///
    /// NaN / ±∞ are accepted by this type. JSON has no encoding for non-finite
    /// floats: `serde_json` silently emits them as `null`, which then fails to
    /// round-trip back into `f64`. Emitters MUST filter non-finite values
    /// before submission — see the type-level "Validation contract" section.
    pub value: f64,
    /// Identifier of the metered resource instance.
    pub resource_id: Uuid,
    /// Logical type of the metered resource (e.g. GTS id or domain name).
    pub resource_type: String,
    /// Subject (user or service) performing the metered action.
    /// `None` when no subject context is available; PDP validation is skipped in that case.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub subject: Option<Subject>,
    /// Idempotency key for at-least-once delivery.
    ///
    /// Length and format constraints (typically a UUID v4 string) are
    /// enforced by the emitter, not by this type.
    pub idempotency_key: String,
    /// Timestamp of the observation.
    pub timestamp: DateTime<Utc>,
    /// Optional caller-supplied metadata. The serialized-size limit is
    /// enforced by the emitter using
    /// [`ModuleConfig::max_metadata_bytes`] returned by
    /// [`crate::UsageCollectorClientV1::get_module_config`]; a value of `0`
    /// disables metadata entirely. Not enforced at this SDK layer.
    /// Absent when not provided; serializes as absent JSON field, not `null`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub metadata: Option<serde_json::Value>,
}

// @cpt-begin:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-1
/// Aggregation function applied over matching usage records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AggregationFn {
    Sum,
    Count,
    Min,
    Max,
    Avg,
}
// @cpt-end:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-1

// @cpt-begin:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-2
/// Time granularity for time-bucket grouping in aggregation queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BucketSize {
    Minute,
    Hour,
    Day,
    Week,
    Month,
}
// @cpt-end:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-2

// @cpt-begin:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-3
/// Dimension by which aggregation results may be grouped.
///
/// `TimeBucket` carries the bucket granularity inline; the other variants are
/// unit variants. Externally tagged enum on the wire: `TimeBucket(Day)` becomes
/// `{"time_bucket":"day"}`; the unit variants become bare strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupByDimension {
    TimeBucket(BucketSize),
    UsageType,
    Subject,
    Resource,
    Source,
}
// @cpt-end:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-3

// @cpt-begin:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-4
/// Parameters for an aggregated usage query delegated to the storage plugin.
///
/// The `scope` field is the PDP-compiled [`AccessScope`] the gateway constructs
/// from the request's authorization context; the storage plugin contract takes
/// no separate `SecurityContext`. `scope` represents the row-level access
/// constraints the plugin MUST apply to the result set.
///
/// All present optional filters AND the `scope` are combined conjunctively
/// (AND semantics) when the plugin compiles the storage query.
///
/// This is a Rust-side SDK contract type, not a serde-encoded wire DTO — its
/// JSON-facing equivalent is the REST handler's query-string DTO, and any
/// out-of-process plugin transports `AggregationQuery` (including `scope`) via
/// the SDK's gRPC client adapter and its proto schema, not via serde.
///
/// This SDK type is the plugin-facing contract. Gateway-internal code paths
/// build a pre-auth request type and lift it into [`AggregationQuery`] after
/// PDP authorization; that lifting helper lives in the gateway crate, not in
/// the SDK, so plugins and remote SDK consumers only see the scoped form.
#[derive(Debug, Clone)]
pub struct AggregationQuery {
    /// Access scope compiled from PDP constraints.
    pub scope: AccessScope,
    /// Mandatory time range (from, to).
    pub time_range: (DateTime<Utc>, DateTime<Utc>),
    /// Aggregation function to apply.
    pub function: AggregationFn,
    /// Dimensions to group results by.
    pub group_by: Vec<GroupByDimension>,
    /// Required when `group_by` contains `TimeBucket`.
    pub bucket_size: Option<BucketSize>,
    /// Optional filter: usage type name.
    pub usage_type: Option<String>,
    /// Optional filter: resource UUID.
    pub resource_id: Option<Uuid>,
    /// Optional filter: resource type.
    pub resource_type: Option<String>,
    /// Optional filter: subject UUID.
    pub subject_id: Option<Uuid>,
    /// Optional filter: subject type.
    pub subject_type: Option<String>,
    /// Optional filter: source module name.
    pub source: Option<String>,
}
// @cpt-end:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-4

// @cpt-begin:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-5
/// A single row in an aggregation result set.
///
/// Each `Option` field is populated only when the corresponding
/// [`GroupByDimension`] was requested; otherwise it is absent (not `null`)
/// in the JSON wire form.
///
/// `PartialEq` is intentionally gated to `#[cfg(test)]`: `value: f64` can be
/// `NaN` in production for `Avg` over an empty bucket, and `NaN != NaN` would
/// make any equality comparison on production data silently false. Round-trip
/// tests in this crate only ever compare finite values, so they keep the
/// derive for assertion convenience; downstream code that needs to compare
/// rows in production should compare structurally with explicit epsilon-on-
/// `value` instead.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[cfg_attr(test, derive(PartialEq))]
pub struct AggregationResult {
    /// Aggregation function that produced [`AggregationResult::value`].
    pub function: AggregationFn,
    /// Numeric aggregation output.
    pub value: f64,
    /// Bucket start when `group_by` includes [`GroupByDimension::TimeBucket`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bucket_start: Option<DateTime<Utc>>,
    /// Usage type when `group_by` includes [`GroupByDimension::UsageType`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub usage_type: Option<String>,
    /// Subject UUID when `group_by` includes [`GroupByDimension::Subject`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub subject_id: Option<Uuid>,
    /// Subject type when `group_by` includes [`GroupByDimension::Subject`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub subject_type: Option<String>,
    /// Resource UUID when `group_by` includes [`GroupByDimension::Resource`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resource_id: Option<Uuid>,
    /// Resource type when `group_by` includes [`GroupByDimension::Resource`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub resource_type: Option<String>,
    /// Source module when `group_by` includes [`GroupByDimension::Source`].
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source: Option<String>,
}
// @cpt-end:cpt-cf-usage-collector-algo-query-api-sdk-types:p1:inst-sdk-5

// @cpt-begin:cpt-cf-usage-collector-algo-query-api-sdk-types:p2:inst-sdk-7
/// Parameters for a raw (unaggregated) usage record query.
///
/// The `scope` field is the PDP-compiled [`AccessScope`] the gateway constructs
/// from the request's authorization context (see `inst-raw-7`); it represents
/// the row-level access constraints the plugin MUST apply to the returned
/// records. The `cursor`, when present, is an exclusive lower-bound keyset
/// cursor produced by [`CursorV1`]; the plugin returns records ascending by
/// `(timestamp, id)` strictly greater than the cursor key. `page_size` MUST be
/// validated by the gateway to lie in `[1, MAX_PAGE_SIZE]`; an absent
/// `page_size` MUST default to `DEFAULT_PAGE_SIZE` before this struct is
/// constructed.
///
/// Source-level filtering is intentionally absent from [`RawQuery`];
/// [`AggregationQuery::source`] supports source filtering, raw-query source
/// filtering is deferred to a future feature revision.
///
/// Like [`AggregationQuery`], this is a Rust-side SDK contract type, not a
/// serde-encoded wire DTO: out-of-process plugins transport `RawQuery`
/// (including `scope`) via the SDK's gRPC client adapter and its proto schema,
/// not via serde. The gateway builds a pre-auth request internally and lifts
/// it into this form after the PDP call.
#[derive(Debug, Clone)]
pub struct RawQuery {
    /// Access scope compiled from PDP constraints.
    pub scope: AccessScope,
    /// Mandatory time range (from, to).
    pub time_range: (DateTime<Utc>, DateTime<Utc>),
    /// Optional filter: usage type name.
    pub usage_type: Option<String>,
    /// Optional filter: resource UUID.
    pub resource_id: Option<Uuid>,
    /// Optional filter: resource type.
    pub resource_type: Option<String>,
    /// Optional filter: subject type.
    pub subject_type: Option<String>,
    /// Optional filter: subject UUID.
    pub subject_id: Option<Uuid>,
    /// Pagination cursor (exclusive lower-bound keyset cursor; ascending by
    /// `(timestamp, id)`). `None` for the first page.
    pub cursor: Option<CursorV1>,
    /// Records per page. Validated in `[1, MAX_PAGE_SIZE]` by the gateway.
    pub page_size: u32,
}
// @cpt-end:cpt-cf-usage-collector-algo-query-api-sdk-types:p2:inst-sdk-7

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "models_tests.rs"]
mod models_tests;
