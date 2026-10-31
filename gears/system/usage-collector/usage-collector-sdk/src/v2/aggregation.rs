//! The aggregate read path's declared fold, grouping dimensions and result
//! shape.
//!
//! The fold is never a request parameter — it is resolved from the queried
//! meter's GTS type declaration — so the surface a caller controls is the
//! grouping alone.

use bigdecimal::BigDecimal;
use serde::{Deserialize, Serialize};

use crate::v2::error::UsageCollectorError;
use crate::v2::models::MetadataKey;

// Declared aggregation fold

/// The single aggregation a meter declares, read from its GTS type
/// declaration's `x-gts-traits.aggregation_fold`.
///
/// Never a request parameter: the aggregate path serves the declared fold and
/// no other. The set is closed — adding a fold is additive, removing one is
/// breaking. `#[non_exhaustive]` is what makes the first half true: without
/// it a new variant breaks every out-of-crate `match`, and plugins match on
/// this to build their SQL. A plugin meets an unknown fold the way
/// [`crate::v2::FeedStart`] names for an unknown start mode — a wildcard arm
/// answering `Internal`.
///
/// `SUM` is the only fold yielding a chargeable period quantity. A meter whose
/// consumption is naturally a level is pre-integrated at the emitter into an
/// accrued quantity and declared `SUM`; this gear integrates on no path.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::VariantArray,
)]
#[serde(rename_all = "UPPERCASE")]
// Spells every variant as serde does, so `FromStr` parses the wire shape
// without a `serde_json::Value` per call. Case-sensitive: the trait schema's
// enum is upper case, so accepting another casing would admit a declaration
// `types-registry` rejects.
#[strum(
    serialize_all = "UPPERCASE",
    parse_err_ty = UsageCollectorError,
    parse_err_fn = UsageCollectorError::invalid_aggregation_fold
)]
#[non_exhaustive]
pub enum AggregationFold {
    /// Total of the selected quantities.
    Sum,
    /// Count of selected entries. Under this fold a quantity means nothing:
    /// one record is one event.
    Count,
    /// Greatest selected quantity.
    Max,
    /// Least selected quantity.
    Min,
    /// The quantity of the entry with the greatest `window_end`, ties broken
    /// by the greatest `accepted_at`, then by the greatest `id` in byte
    /// order. `id` is unique, so the order is total, and every one of those
    /// keys compares across tenants and types. A plugin owes this exact order;
    /// a backend-local sequence or insertion order is not it.
    Latest,
}

impl AggregationFold {
    /// Every variant, so a consumer walking the closed set cannot fall behind
    /// the enum. Derived by `strum::VariantArray`, so a fold added later lands
    /// here on its own.
    ///
    /// What `#[non_exhaustive]` costs an out-of-crate `match` — the compile
    /// error on a fold added later — this gives back at run time: a test
    /// iterating this slice fails on the new fold instead of silently covering
    /// one less.
    pub const ALL: &'static [Self] = <Self as strum::VariantArray>::VARIANTS;

    /// The wire spelling, identical to the trait-schema enum member.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

/// Dimension to group an aggregation by.
///
/// Each variant is a column or JSON-key facet of the underlying record
/// stream. `Metadata(MetadataKey)` carries a single declared metadata key
/// (validated against the queried usage type's `metadata_fields`).
///
/// `#[non_exhaustive]` for [`AggregationFold`]'s reason: plugins match on this
/// to build their `GROUP BY`, so a dimension added later must not break every
/// out-of-crate match.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(strum::EnumCount))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AggregationDimension {
    /// Group by owning tenant.
    TenantId,
    /// Group by `resource_ref.resource_id`.
    ResourceId,
    /// Group by `resource_ref.resource_type`.
    ResourceType,
    /// Group by `subject_ref.subject_id` (rows without a subject are
    /// excluded from the grouping).
    SubjectId,
    /// Group by `subject_ref.subject_type` (rows without a `subject_type`
    /// are excluded from the grouping).
    SubjectType,
    /// Group by the value of a single declared metadata key.
    Metadata(MetadataKey),
}

impl AggregationDimension {
    /// Every variant that carries no payload, for [`AggregationFold::ALL`]'s
    /// reason. [`Self::Metadata`] is the sixth and is absent here because it
    /// carries a key a caller has to supply.
    ///
    /// Hand-listed, because `strum::VariantArray` refuses a data-carrying
    /// variant; a test pins the length to the enum's variant count instead.
    pub const ALL_UNIT_VARIANTS: &'static [Self] = &[
        Self::TenantId,
        Self::ResourceId,
        Self::ResourceType,
        Self::SubjectId,
        Self::SubjectType,
    ];
}

/// Single aggregated bucket. One entry per element in the query's
/// `group_by` dimensions, in the same order; empty when `group_by` was
/// empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregationBucket {
    /// Dimension key values in `group_by` order. Each entry is the string
    /// form of the corresponding [`AggregationDimension`]:
    ///
    /// - [`AggregationDimension::TenantId`] — `Uuid::to_string()`, the
    ///   canonical lowercase hyphenated form
    ///   (`01234567-89ab-cdef-0123-456789abcdef`).
    /// - [`AggregationDimension::ResourceId`],
    ///   [`AggregationDimension::ResourceType`],
    ///   [`AggregationDimension::SubjectId`],
    ///   [`AggregationDimension::SubjectType`] — the record's corresponding
    ///   identifier or type string verbatim.
    /// - [`AggregationDimension::Metadata`] — the metadata value at the
    ///   declared key, which is already a `String` (or string-coercible)
    ///   per the resolved meter declaration's closed-shape metadata rule.
    ///
    /// Plugins own this string-form contract at bucket-construction time;
    /// the SDK does not transform values at the boundary. Empty when
    /// `group_by` was empty (the no-grouping case yields a single bucket).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key: Vec<String>,
    /// Aggregation result for the bucket, carried as an arbitrary-precision
    /// [`bigdecimal::BigDecimal`] so every fold is exact at any magnitude:
    /// Postgres `NUMERIC` is unbounded, and a wide `SUM` can exceed
    /// [`rust_decimal::Decimal`]'s ~7.9×10²⁸ ceiling.
    ///
    /// `None` when no rows matched the bucket, except under `SUM` and
    /// `COUNT`, which are defined over an empty selection and answer
    /// `Some(0)`.
    ///
    /// Wire-encoded as a JSON string (never a float) for the same round-trip
    /// reason as [`UsageRecord::quantity`](crate::v2::UsageRecord::quantity), via
    /// [`crate::serde_helpers::bigdecimal_str_option`].
    /// No `serde(default)`: `AggregationBucketV2` lists `value` in
    /// `required`, and `None` here is a real answer (the bucket matched no
    /// rows) rather than an absent field, so an omitted key is refused
    /// instead of read as an empty bucket.
    #[serde(with = "crate::serde_helpers::bigdecimal_str_option")]
    pub value: Option<BigDecimal>,
}

/// Aggregated-query result.
///
/// A single bucket with an empty `key` represents the no-grouping case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AggregationResult {
    /// Result buckets in plugin-emitted order.
    pub buckets: Vec<AggregationBucket>,
}

/// Maximum number of buckets a single [`AggregationResult`] may carry.
///
/// This caps the distinct *groups* an aggregate produces, which a
/// high-cardinality [`AggregationDimension::Metadata`] key could otherwise
/// leave unbounded. The storage plugin MUST bound its own result (e.g. append
/// `LIMIT MAX_AGGREGATION_BUCKETS + 1`); the gateway rejects a result
/// exceeding this cap with a `400`
/// ([`crate::v2::reason::AGGREGATION_RESULT_TOO_LARGE`]). Declared on the wire as
/// `AggregatedQueryResult.buckets`' `maxItems`.
pub const MAX_AGGREGATION_BUCKETS: usize = 100_000;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "aggregation_tests.rs"]
mod aggregation_tests;
