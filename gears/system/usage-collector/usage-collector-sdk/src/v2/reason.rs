//! Typed `reason` discriminators carried by the compacted
//! [`crate::v2::UsageCollectorError`] category variants.
//!
//! Most of them are typed views over the stable `SCREAMING_SNAKE` wire
//! codes, with `from_wire` / `as_wire` round-trips and an
//! [`Unknown`](ValidationReason::Unknown) catch-all that preserves any
//! future code verbatim. The host-side lift in
//! `usage-collector::infra::sdk_error_mapping` projects each onto the
//! RFC-9457 `Problem` (`field_violations[].reason` for [`ValidationReason`],
//! `context.reason` for [`ConflictReason`]).
//!
//! [`NotFoundReason`] is the exception: it is **not projected onto the
//! wire**, because the platform-shared
//! `toolkit_canonical_errors::NotFoundV1` context has no reason slot for it
//! to land in. It therefore carries no wire constants and no round-trip,
//! and serves in-process consumers and the gear's own classification.

use core::fmt;

// ─────────────────────────────────────────────────────────────────────
// ValidationReason — 400 InvalidArgument `field_violations[].reason`.
// ─────────────────────────────────────────────────────────────────────

/// Reserved, and emitted by nothing in this crate.
///
/// Kept rather than removed: [`ValidationReason`] is `#[non_exhaustive]`,
/// so dropping a variant is silent for a downstream matcher, and a
/// consumer reading an envelope stored while the rule that raised it was
/// live still needs this to model itself.
pub const SEMANTICS_VIOLATION: &str = "SEMANTICS_VIOLATION";
/// Generic request-shape validation failure (batch size, validating
/// newtypes, closed-kind enum parse). Catch-all when no finer code applies.
///
/// Also the code for a *declared* metadata key whose value breaks a
/// per-value constraint its meter declaration names — see
/// [`crate::v2::UsageCollectorError::metadata_constraint_violation`] for why
/// that is this code rather than [`METADATA_VALIDATION`].
pub const VALIDATION: &str = "VALIDATION";
/// Per-record serialized-metadata size cap exceeded, on `metadata`.
///
/// This cap and nothing else, per `usage-collector-v2.yaml`. Emitted solely
/// by [`crate::v2::UsageCollectorError::metadata_size_exceeded`]; the detail
/// states the measured size and the configured cap. A metadata *shape* or
/// *value* rejection is [`UNKNOWN_METADATA_KEY`] or [`VALIDATION`] — never
/// this.
pub const METADATA_VALIDATION: &str = "METADATA_VALIDATION";
/// A metadata key the resolved meter declaration does not carry in its
/// closed `metadata_fields` shape. `context.resource_name` carries the
/// offending `gts_type_id`.
///
/// Emitted on both surfaces that resolve a key against a declaration:
///
/// * **Ingest**, on `metadata` — one or more submitted keys are undeclared,
///   via [`crate::v2::UsageCollectorError::unknown_metadata_keys`]. Raised
///   only when every violation in the entry is an undeclared key; an entry
///   that also breaks a declared key's per-value constraint carries
///   [`VALIDATION`] instead, with every violation in the detail.
/// * **Read**, on `metadata` for a `metadata_filter` predicate key and on
///   `group_by` for an aggregation dimension — via
///   [`crate::v2::UsageCollectorError::unknown_metadata_key`] and
///   [`crate::v2::UsageCollectorError::undeclared_metadata_dimension`],
///   each naming the first undeclared key.
pub const UNKNOWN_METADATA_KEY: &str = "UNKNOWN_METADATA_KEY";
/// Malformed / wrong-base `gts_id` on a type or record DTO.
pub const INVALID_BASE_GTS_ID: &str = "INVALID_BASE_GTS_ID";
/// `metadata_fields[i]` entry was the empty string.
pub const INVALID_METADATA_FIELDS_EMPTY_STRING: &str = "INVALID_METADATA_FIELDS_EMPTY_STRING";
/// `metadata_fields[i]` entry failed `MetadataKey::new` (e.g. NUL byte).
pub const INVALID_METADATA_FIELDS_INVALID_KEY: &str = "INVALID_METADATA_FIELDS_INVALID_KEY";
/// Duplicate `metadata_fields[i]` entry.
pub const INVALID_METADATA_FIELDS_DUPLICATE: &str = "INVALID_METADATA_FIELDS_DUPLICATE";
/// An aggregated query produced more distinct groups than
/// [`crate::v2::MAX_AGGREGATION_BUCKETS`] — typically a high-cardinality `group_by`
/// (e.g. a per-record metadata key) over a wide range. Narrow the read-path
/// time range or drop the high-cardinality dimension.
pub const AGGREGATION_RESULT_TOO_LARGE: &str = "AGGREGATION_RESULT_TOO_LARGE";
/// An invalidation departed from its target in a field it must copy.
pub const INVALIDATION_FIELD_MISMATCH: &str = "INVALIDATION_FIELD_MISMATCH";
/// The covered period ends further into the future than the ingestion
/// path's configured future tolerance. Raised on **both** ingestion
/// routes — one configured value governs each of them: the bound guards
/// against an emitter opening a period that does not yet exist, and the
/// backfill route lifts only the past bound.
pub const FUTURE_WINDOW: &str = "FUTURE_WINDOW";
/// The covered period ends further into the past than the live path's
/// configured past tolerance. Raised on the live path only; the detail
/// names the backfill route, which exists for exactly these periods
/// (`cpt-cf-usage-collector-adr-backfill-isolation`).
pub const PAST_WINDOW: &str = "PAST_WINDOW";
/// A quantity outside the published `UsageQuantity` range or precision: not
/// matching the wire pattern, more than 28 significant digits, or a negative
/// zero (which no backend can read back digit for digit).
pub const QUANTITY_OUT_OF_RANGE: &str = "QUANTITY_OUT_OF_RANGE";
/// A feed cursor names a position after which retention has already
/// removed an entry the read would have had to deliver.
///
/// Published by `usage-collector-v2.yaml` as a `cursor` field violation. The
/// storage plugin decides it from what it still holds rather than from the
/// cursor's age, and `DESIGN.md` §3.3 lifts the plugin's
/// `CursorBeyondRetention` onto an `InvalidArgument` carrying this reason.
pub const CURSOR_BEYOND_RETENTION: &str = "CURSOR_BEYOND_RETENTION";

/// Typed view of the `field_violations[].reason` codes carried by
/// [`crate::v2::UsageCollectorError::InvalidArgument`].
///
/// It does **not** model `INVALID_CURSOR`, `FILTER_MISMATCH` or
/// `ORDER_WITH_CURSOR`: all three belong to `toolkit_odata`, which declares
/// them on its own cursor error enum and maps them to `Problem` field
/// violations. `INVALID_CURSOR` and `FILTER_MISMATCH` travel through
/// [`crate::v2::UsageCollectorError::CursorRejected`], which carries the
/// upstream error; `ORDER_WITH_CURSOR` never reaches this gear at all,
/// because `toolkit_odata`'s axum extractor refuses `$orderby` alongside a
/// cursor before the handler runs.
///
/// A consumer matching on any of the three reads it out of the wire string
/// via [`Self::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValidationReason {
    /// See [`SEMANTICS_VIOLATION`].
    SemanticsViolation,
    /// See [`VALIDATION`].
    Validation,
    /// See [`METADATA_VALIDATION`].
    MetadataValidation,
    /// See [`UNKNOWN_METADATA_KEY`].
    UnknownMetadataKey,
    /// See [`INVALID_BASE_GTS_ID`].
    InvalidBaseGtsId,
    /// See [`INVALID_METADATA_FIELDS_EMPTY_STRING`].
    MetadataFieldEmptyString,
    /// See [`INVALID_METADATA_FIELDS_INVALID_KEY`].
    MetadataFieldInvalidKey,
    /// See [`INVALID_METADATA_FIELDS_DUPLICATE`].
    MetadataFieldDuplicate,
    /// See [`AGGREGATION_RESULT_TOO_LARGE`].
    AggregationResultTooLarge,
    /// See [`INVALIDATION_FIELD_MISMATCH`].
    InvalidationFieldMismatch,
    /// See [`FUTURE_WINDOW`].
    FutureWindow,
    /// See [`PAST_WINDOW`].
    PastWindow,
    /// See [`QUANTITY_OUT_OF_RANGE`].
    QuantityOutOfRange,
    /// See [`CURSOR_BEYOND_RETENTION`].
    CursorBeyondRetention,
    /// Unmodeled / future reason — preserves the raw wire string.
    Unknown(String),
}

impl ValidationReason {
    /// Project a wire `field_violations[].reason` string into the typed
    /// discriminator. Any unmodeled value is preserved in [`Self::Unknown`].
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            SEMANTICS_VIOLATION => Self::SemanticsViolation,
            VALIDATION => Self::Validation,
            METADATA_VALIDATION => Self::MetadataValidation,
            UNKNOWN_METADATA_KEY => Self::UnknownMetadataKey,
            INVALID_BASE_GTS_ID => Self::InvalidBaseGtsId,
            INVALID_METADATA_FIELDS_EMPTY_STRING => Self::MetadataFieldEmptyString,
            INVALID_METADATA_FIELDS_INVALID_KEY => Self::MetadataFieldInvalidKey,
            INVALID_METADATA_FIELDS_DUPLICATE => Self::MetadataFieldDuplicate,
            AGGREGATION_RESULT_TOO_LARGE => Self::AggregationResultTooLarge,
            INVALIDATION_FIELD_MISMATCH => Self::InvalidationFieldMismatch,
            FUTURE_WINDOW => Self::FutureWindow,
            PAST_WINDOW => Self::PastWindow,
            QUANTITY_OUT_OF_RANGE => Self::QuantityOutOfRange,
            CURSOR_BEYOND_RETENTION => Self::CursorBeyondRetention,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Render the discriminator back to its wire `reason` string. Inverse of
    /// [`Self::from_wire`] for the modeled variants.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::SemanticsViolation => SEMANTICS_VIOLATION,
            Self::Validation => VALIDATION,
            Self::MetadataValidation => METADATA_VALIDATION,
            Self::UnknownMetadataKey => UNKNOWN_METADATA_KEY,
            Self::InvalidBaseGtsId => INVALID_BASE_GTS_ID,
            Self::MetadataFieldEmptyString => INVALID_METADATA_FIELDS_EMPTY_STRING,
            Self::MetadataFieldInvalidKey => INVALID_METADATA_FIELDS_INVALID_KEY,
            Self::MetadataFieldDuplicate => INVALID_METADATA_FIELDS_DUPLICATE,
            Self::AggregationResultTooLarge => AGGREGATION_RESULT_TOO_LARGE,
            Self::InvalidationFieldMismatch => INVALIDATION_FIELD_MISMATCH,
            Self::FutureWindow => FUTURE_WINDOW,
            Self::PastWindow => PAST_WINDOW,
            Self::QuantityOutOfRange => QUANTITY_OUT_OF_RANGE,
            Self::CursorBeyondRetention => CURSOR_BEYOND_RETENTION,
            Self::Unknown(s) => s.as_str(),
        }
    }
}

impl fmt::Display for ValidationReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

// ─────────────────────────────────────────────────────────────────────
// ConflictReason — 409 Aborted `context.reason`.
// ─────────────────────────────────────────────────────────────────────

/// Same `idempotency_key`, canonical-field-different payload.
pub const IDEMPOTENCY_CONFLICT: &str = "IDEMPOTENCY_CONFLICT";
/// A second invalidation of a record under another reason code: the dedup conflict of an invalidation, reported by the gateway.
pub const ALREADY_INVALIDATED: &str = "ALREADY_INVALIDATED";
/// An invalidation named a target whose dedup identity has not converged yet.
/// Retryable: it clears within the plugin's convergence bound plus its
/// query-path lag bound.
pub const TARGET_NOT_CONVERGED: &str = "TARGET_NOT_CONVERGED";

/// Typed view of the `context.reason` codes carried by
/// [`crate::v2::UsageCollectorError::Conflict`] (the AIP-193 `Aborted` /
/// HTTP 409 category).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConflictReason {
    /// See [`IDEMPOTENCY_CONFLICT`].
    IdempotencyConflict,
    /// See [`ALREADY_INVALIDATED`].
    AlreadyInvalidated,
    /// See [`TARGET_NOT_CONVERGED`].
    TargetNotConverged,
    /// Unmodeled / future reason — preserves the raw wire string.
    Unknown(String),
}

impl ConflictReason {
    /// Project a wire `context.reason` string into the typed discriminator.
    /// Any unmodeled value is preserved in [`Self::Unknown`].
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            IDEMPOTENCY_CONFLICT => Self::IdempotencyConflict,
            ALREADY_INVALIDATED => Self::AlreadyInvalidated,
            TARGET_NOT_CONVERGED => Self::TargetNotConverged,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Render the discriminator back to its wire `reason` string. Inverse of
    /// [`Self::from_wire`] for the modeled variants.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::IdempotencyConflict => IDEMPOTENCY_CONFLICT,
            Self::AlreadyInvalidated => ALREADY_INVALIDATED,
            Self::TargetNotConverged => TARGET_NOT_CONVERGED,
            Self::Unknown(s) => s.as_str(),
        }
    }
}

impl fmt::Display for ConflictReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

// ─────────────────────────────────────────────────────────────────────
// NotFoundReason — 404. In-process only; NOT projected onto the wire.
// ─────────────────────────────────────────────────────────────────────

/// Which lookup failed, on [`crate::v2::UsageCollectorError::NotFound`].
///
/// **Not projected onto the wire, and deliberately not.** An in-process SDK
/// consumer — one resolving [`crate::v2::api::UsageCollectorClientV2`] through
/// `ClientHub` — reads this discriminator directly. A REST client does not,
/// and tells the three cases apart only by `detail` prose. So unlike
/// [`ValidationReason`] and [`ConflictReason`] these have no
/// `SCREAMING_SNAKE` constants, no `from_wire` / `as_wire`, and never
/// appear on a `Problem` body.
///
/// Projecting it would take a platform change first:
/// `toolkit_canonical_errors::NotFoundV1` is an empty, platform-shared
/// context struct with no reason path at all.
///
/// What it exists for is the gear's own classification — separating a
/// failed invalidation-target resolution from an ordinary missing entry
/// without matching on caller-facing prose.
///
/// `#[non_exhaustive]` so the SDK can add a variant in a minor release. A
/// new variant must be reviewed against DESIGN §3.11.5 and given an
/// explicit arm: a wildcard silently absorbs it, and the gear's own
/// wildcard in `classify_record_error` counts it as
/// `semantics_violation`.
#[expect(
    clippy::enum_variant_names,
    reason = "the shared `NotFound` postfix is the point: each variant is named for the error it lifts from, so a reader can follow a reason back to its origin by name. Renaming to satisfy the lint would sever that"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NotFoundReason {
    /// A `gts_type_id` that does not resolve to a usable declaration —
    /// the Type Resolver's `DeclarationNotFound`, whether the registry
    /// never declared it or the declaration was rejected as incomplete.
    DeclarationNotFound,
    /// A lookup named a `UsageRecord.id` that did not resolve. Three
    /// producers, and the third is why this variant must stay one variant:
    ///
    /// - the by-id point read, when the store genuinely holds no such row;
    /// - a plugin's own `UsageRecordNotFound` raised from an SPI call
    ///   other than the invalidation-target lookup (that one is converted
    ///   to [`Self::InvalidationTargetNotFound`] at the fan-out);
    /// - a **PDP denial on the by-id point read**, collapsed into this
    ///   exact reason by the host so the surface is not an existence
    ///   oracle. There the row may well exist.
    ///
    /// The collapsed denial and the genuine miss are indistinguishable on
    /// purpose: splitting them off into a variant of their own would hand
    /// every in-process consumer the oracle the collapse exists to deny.
    UsageRecordNotFound,
    /// The target an invalidation locates through its own tenant, GTS type,
    /// idempotency key and covered period resolved to nothing: the Valid
    /// target rule of
    /// `cpt-cf-usage-collector-adr-append-only-invalidation`. A plugin's
    /// `UsageRecordNotFound` from the target lookup is converted to this
    /// at the ingestion fan-out, so on the ingest path it is this reason,
    /// not [`Self::UsageRecordNotFound`], that the target check raises.
    InvalidationTargetNotFound,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "reason_tests.rs"]
mod reason_tests;
