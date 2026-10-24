//! The `$filter` and `$orderby` schema both query read paths are held to, and
//! the metadata-filter parameter that travels beside them.

use serde::{Deserialize, Serialize};
use toolkit_odata_macros::ODataFilterable;
use uuid::Uuid;

use crate::v2::error::UsageCollectorError;
use crate::v2::models::{MetadataKey, RECORD_ID_FIELD, WINDOW_END_FIELD, WINDOW_START_FIELD};

// Filter surface for `list_usage_records`
//
// `UsageRecordQuery` declares the filterable-field schema for the OData
// surface of `list_usage_records`. The struct is never constructed at
// runtime; it exists solely to feed `#[derive(ODataFilterable)]`, which
// generates `UsageRecordQueryFilterField` and its
// `toolkit_odata::filter::FilterField` impl. Plugin implementations supply
// a `FieldToColumn<UsageRecordFilterField>` mapper next to their entity
// definition; the SDK does not encode storage-layer column mapping.
//
// `gts_type_id` is intentionally absent from this schema. It travels as a
// typed parameter on both read surfaces, and omitting it here makes
// `parse_odata_filter::<UsageRecordFilterField>` reject any
// `gts_type_id`-touching predicate as `FilterError::UnknownField`. That
// covers the wire path only: an in-process caller builds an `ast::Expr` by
// hand, so the host crate's runtime `reject_unpublished_filter_fields` guard
// reserves `gts_type_id` too, and needs to.
//
// The covered-period bounds, `id` and `accepted_at` ARE on the schema, and
// none of them is `$filter`-reachable: this schema is a superset of
// `$filter`'s admissible surface rather than a definition of it, and it is
// what gives every `KEYSET_SAFE_RECORD_FIELDS` name a column mapping.
// `PUBLISHED_FILTER_FIELDS` defines the filterable set and
// `reject_unpublished_filter_fields` enforces it.
//
// Nested attribution composites (`resource_ref`, `subject_ref`) are
// flattened to their leaf identifiers (`resource_id`, `resource_type`,
// `subject_id`, `subject_type`) so filtering goes through the macro-derived
// path rather than a hand-rolled slash-path `FilterField` impl.
//
// `entry_type` is declared `String` on the filter wire (`"record"` /
// `"invalidation"`). The SDK stores no such attribute, so a plugin wanting the
// field filterable holds a column of its own — written or derived, its choice
// — and returns it from `FieldToColumn::map_field`. `map_value` cannot carry
// it: that hook rewrites a value and can change neither column nor operator.
//
// `metadata` filtering does not flow through OData — see `MetadataFilter`
// below, supplied as a separate parameter on `list_usage_records`.

/// Filterable-field schema for `list_usage_records`'s `ODataQuery` argument.
///
/// Never constructed at runtime — see the file-level comment above. It needs
/// no `dead_code` suppression: being `pub` and re-exported, it never fires.
#[derive(ODataFilterable)]
pub struct UsageRecordQuery {
    /// `usage_records.id` (record primary key). On the filter surface so the
    /// gateway can use it as the canonical cursor tiebreaker
    /// (`(window_end, id)`) and a plugin has a column mapping for it;
    /// **not** filterable — it is off [`PUBLISHED_FILTER_FIELDS`], so
    /// `reject_unpublished_filter_fields` rejects any predicate naming it.
    #[odata(filter(kind = "Uuid"))]
    pub id: Uuid,
    /// `usage_records.window_start` — the inclusive start of the covered
    /// period. Here so a plugin has a column mapping for it and a caller can
    /// order by it; **not** filterable, because the read range travels as a
    /// typed parameter.
    #[odata(filter(kind = "DateTimeUtc"))]
    pub window_start: time::OffsetDateTime,
    /// `usage_records.window_end` — the exclusive end of the covered period,
    /// and the column every read path selects on (see
    /// [`crate::v2::TimeRange::contains_window_end`]). Every raw-path page order
    /// names it, so selection and sorting share a column. Reserved on the
    /// `$filter` surface for the same reason as [`Self::window_start`].
    #[odata(filter(kind = "DateTimeUtc"))]
    pub window_end: time::OffsetDateTime,
    /// `usage_records.tenant_id` (owning tenant). Supports `eq` and `in`.
    #[odata(filter(kind = "Uuid"))]
    pub tenant_id: Uuid,
    /// `usage_records.resource_ref.resource_id`, flattened for the filter
    /// surface.
    #[odata(filter(kind = "String"))]
    pub resource_id: String,
    /// `usage_records.resource_ref.resource_type`, flattened for the filter
    /// surface.
    #[odata(filter(kind = "String"))]
    pub resource_type: String,
    /// `usage_records.subject_ref.subject_id`, flattened for the filter
    /// surface.
    #[odata(filter(kind = "String"))]
    pub subject_id: String,
    /// `usage_records.subject_ref.subject_type`, flattened for the filter
    /// surface.
    #[odata(filter(kind = "String"))]
    pub subject_type: String,
    /// The entry an invalidation withdraws, or absent on an ordinary
    /// record. Filterable (`eq` / `in`) so a consumer folding entries
    /// itself can find a withdrawn pair; **not** an order key, because it
    /// is domain-optional — see [`is_keyset_safe_record_field`].
    #[odata(filter(kind = "Uuid"))]
    pub invalidates: Uuid,
    /// The derived `record` / `invalidation` discriminator. On the filter
    /// surface because the wire contract lists it there; a plugin backs it
    /// with a column of its own and returns that from
    /// `FieldToColumn::map_field` (see the file-level comment above).
    /// **Not** an order key — see [`is_keyset_safe_record_field`].
    #[odata(filter(kind = "String"))]
    pub entry_type: String,
    /// `usage_records.origin` — which ingestion path admitted the entry
    /// (`live` / `backfill`). On the filter surface so a consumer can separate
    /// imported history from current consumption.
    ///
    /// Declared `String` on the filter wire like [`Self::entry_type`], but
    /// unlike it a stored attribute rather than a function of an optional one,
    /// so it **is** a sound order key. See [`is_keyset_safe_record_field`].
    #[odata(filter(kind = "String"))]
    pub origin: String,
    /// `usage_records.accepted_at` — the gear-assigned acceptance instant.
    /// Here so a plugin resolves the admissible `$orderby` key of the same name
    /// through the one field-to-column map; **not** filterable, being off
    /// [`PUBLISHED_FILTER_FIELDS`].
    #[odata(filter(kind = "DateTimeUtc"))]
    pub accepted_at: time::OffsetDateTime,
}

pub use UsageRecordQueryFilterField as UsageRecordFilterField;

/// The `$filter` field set the public contract publishes, and the whole of
/// it — the closed set the gear's `reject_unpublished_filter_fields` guard
/// admits.
///
/// Fixed, not resolved per request, and a strict subset of
/// [`UsageRecordFilterField`]'s variants, which are wider: membership in that
/// schema says nothing about `$filter` admissibility (see the file-level
/// comment above `UsageRecordQuery`), and this constant is what says it.
///
/// Matching against it is case-insensitive, not exact: see the gear's
/// `is_published_filter_field`, which has to agree with
/// `toolkit_odata::FilterField::from_name`, the resolver `$filter` identifiers
/// actually go through.
pub const PUBLISHED_FILTER_FIELDS: &[&str] = &[
    "tenant_id",
    "resource_id",
    "resource_type",
    "subject_id",
    "subject_type",
    "entry_type",
    "origin",
    "invalidates",
];

/// The record attributes every entry carries in its own right, and
/// therefore sound as keyset-pagination ordering keys — the closed set
/// [`is_keyset_safe_record_field`] tests against.
///
/// Exported because it is the admissible `$orderby` vocabulary, so a `400`
/// refusing a caller's order can name the whole closed set.
///
/// Matching is **exact**, unlike [`PUBLISHED_FILTER_FIELDS`] just above: an
/// `$orderby` key never passes through
/// `toolkit_odata::filter::FilterField::from_name` — the plugin hands the
/// caller's string straight to its field-to-column map, an exact `match` — so
/// folding case here would admit a key the storage layer then fails to
/// resolve.
pub const KEYSET_SAFE_RECORD_FIELDS: &[&str] = &[
    RECORD_ID_FIELD,
    WINDOW_START_FIELD,
    WINDOW_END_FIELD,
    "tenant_id",
    "resource_id",
    "resource_type",
    "origin",
    "accepted_at",
];

/// Record filter fields sound to use as a keyset-pagination ordering key.
///
/// The storage plugin's keyset continuation is a row-value tuple comparison
/// (`(c1, c2, …) > ($…)`). In SQL three-valued logic a tuple whose leading
/// column is NULL compares as NULL, so every NULL-keyed row is silently
/// dropped from the paged result — and a page ending on such a row cannot
/// encode a `next_cursor` at all (a 500). A keyset key therefore has to be an
/// attribute this SDK guarantees present on every entry. Two kinds of field
/// are not, and both are on the filterable schema:
///
/// - **Domain-optional attributes.** `subject_id` / `subject_type` come from
///   `subject_ref: Option<SubjectRef>`, and the `invalidates` filter field
///   reads the target inside [`UsageRecord::invalidation`], absent on every
///   ordinary measurement.
/// - **Attributes derived from an optional one.** `entry_type` has a value on
///   every entry and is still not keyset-safe: it is
///   [`UsageRecord::entry_type`], a function of `invalidates` that partitions
///   entries on that field's *absence*. The SDK carries no such attribute of
///   its own and obliges no plugin to materialize one.
///
/// Derivation, not presence, is what separates the second bullet from the safe
/// fields: [`UsageRecord::origin`] and [`UsageRecord::accepted_at`] also have
/// a value on every entry, but each is a *field* of the record, so the SDK's
/// shape obliges every plugin persisting a [`UsageRecord`] to store it
/// non-null.
///
/// Enforcement is the **gateway's alone**: it refuses a caller `$orderby` on a
/// non-keyset-safe field with a `400` and guarantees the order slot the Plugin
/// SPI documents on every surface — see
/// [`crate::v2::UsageCollectorPluginV2::list_usage_records`], normative for the
/// plugin side. The allowlist is fail-closed: a newly added field is unsafe
/// until it is classified there.
///
/// Every name here has a [`UsageRecordFilterField`] variant, so a plugin
/// resolves all of them through the one field-to-column map it already needs
/// for `$filter` — see [`crate::v2::UsageCollectorPluginV2::list_usage_records`],
/// which obliges it to.
#[must_use]
pub fn is_keyset_safe_record_field(name: &str) -> bool {
    KEYSET_SAFE_RECORD_FIELDS.contains(&name)
}

/// Equality-set filter applied to a single [`UsageRecord::metadata`] key.
///
/// `metadata` keys are not part of any static schema, and `toolkit-odata`'s
/// filter surface cannot express filtering on dynamic map keys. This is the
/// typed side-channel [`crate::v2::UsageCollectorClientV2::list_usage_records`]
/// and the plugin SPI use instead.
///
/// Semantics across a `&[MetadataFilter]`:
///
/// - AND across **every** filter in the slice, including two that name the
///   same key: a storage plugin emits one AND-ed clause per entry, so
///   `[k in {a}, k in {b}]` selects rows whose `k` is both — generally
///   nothing — and is **not** equivalent to the single filter
///   `k in {a, b}`. A consumer that merged same-key entries would widen
///   the result set.
/// - OR within a single filter's `values()`.
/// - An empty slice imposes no metadata filter.
///
/// Constructor [`Self::new`] enforces a validated [`MetadataKey`] (non-empty,
/// no NUL bytes) and a non-empty value set carrying no NUL of its own;
/// `Deserialize` routes through `new` so wire payloads cannot bypass
/// validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetadataFilter {
    key: MetadataKey,
    values: Vec<String>,
}

impl MetadataFilter {
    /// Creates a [`MetadataFilter`] after validating `key` (via
    /// [`MetadataKey::new`]) and that `values` carries at least one entry.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidArgument`] when the key
    /// is empty or contains a NUL byte, when `values` is empty, or when a
    /// value contains a NUL byte. A failing `MetadataKey::new` is rewrapped
    /// as `InvalidMetadataFilter` so callers see one variant for the whole
    /// `MetadataFilter::new` boundary.
    pub fn new(
        key: impl Into<String>,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, UsageCollectorError> {
        let key = MetadataKey::new(key).map_err(|err| match err {
            UsageCollectorError::InvalidArgument { detail, .. } => {
                UsageCollectorError::invalid_metadata_filter(detail)
            }
            other => other,
        })?;
        let values: Vec<String> = values.into_iter().map(Into::into).collect();
        if values.is_empty() {
            return Err(UsageCollectorError::invalid_metadata_filter(format!(
                "metadata filter for key `{key}` must carry at least one value"
            )));
        }
        // The read-side half of `models::reject_nul_metadata_values`. A value
        // is bound against `r.metadata ->> $N`, which is `text`, and no
        // Postgres `text` carries a NUL — so one that reached the plugin would
        // fail there as an `Internal`, a 500 the caller provoked. Refused here
        // it is a 400 instead. Nothing is lost by refusing it: ingestion
        // rejects a stored value carrying a NUL, so such a filter could never
        // select a row.
        if let Some(value) = values.iter().find(|value| value.contains('\0')) {
            return Err(UsageCollectorError::invalid_metadata_filter(format!(
                "metadata filter value {value:?} for key `{key}` must not contain NUL: a \
                 value is matched against a `text` projection of the stored metadata, which \
                 cannot carry one"
            )));
        }
        Ok(Self { key, values })
    }

    /// Borrows the metadata key.
    #[must_use]
    pub fn key(&self) -> &MetadataKey {
        &self.key
    }

    /// Borrows the candidate value set.
    #[must_use]
    pub fn values(&self) -> &[String] {
        &self.values
    }
}

impl<'de> Deserialize<'de> for MetadataFilter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            key: String,
            values: Vec<String>,
        }
        let Raw { key, values } = Raw::deserialize(deserializer)?;
        MetadataFilter::new(key, values).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "filter_tests.rs"]
mod filter_tests;
