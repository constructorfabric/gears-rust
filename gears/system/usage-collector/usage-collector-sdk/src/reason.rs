//! Typed `reason` discriminators carried by the compacted
//! [`crate::UsageCollectorError`] category variants.
//!
//! A typed view over
//! the stable `SCREAMING_SNAKE` wire codes, with `from_wire` / `as_wire`
//! round-trips and an [`Unknown`](ValidationReason::Unknown) catch-all that
//! preserves any future code verbatim. The host-side lift in
//! `usage-collector::infra::sdk_error_mapping` projects each onto the
//! RFC-9457 `Problem` (`field_violations[].reason` for [`ValidationReason`],
//! `context.reason` for [`ConflictReason`]).

use core::fmt;

// ─────────────────────────────────────────────────────────────────────
// ValidationReason — 400 InvalidArgument `field_violations[].reason`.
// ─────────────────────────────────────────────────────────────────────

/// Counter / gauge value-matrix violation (counter ordinary `value >= 0`,
/// counter compensation `value < 0`).
pub const SEMANTICS_VIOLATION: &str = "SEMANTICS_VIOLATION";
/// Generic request-shape validation failure (batch size, validating
/// newtypes, closed-kind enum parse). Catch-all when no finer code applies.
pub const VALIDATION: &str = "VALIDATION";
/// Per-record serialized-metadata size cap exceeded.
pub const METADATA_VALIDATION: &str = "METADATA_VALIDATION";
/// Ingestion supplied a metadata key not declared in the usage type's
/// closed `metadata_fields` shape.
pub const UNKNOWN_METADATA_KEY: &str = "UNKNOWN_METADATA_KEY";
/// Compensation submitted against a gauge usage type (gauges have no `SUM`).
pub const GAUGE_COMPENSATION_REJECTED: &str = "GAUGE_COMPENSATION_REJECTED";
/// A raw / aggregated query omitted the mandatory bounded `created_at`
/// window (a lower **and** an upper bound on `created_at` as top-level
/// `$filter` conjuncts), which would force an unbounded full-table scan.
pub const MISSING_TIME_WINDOW: &str = "MISSING_TIME_WINDOW";
/// Malformed / wrong-base `gts_id` on a type or record DTO.
pub const INVALID_BASE_GTS_ID: &str = "INVALID_BASE_GTS_ID";
/// `metadata_fields[i]` entry was the empty string.
pub const INVALID_METADATA_FIELDS_EMPTY_STRING: &str = "INVALID_METADATA_FIELDS_EMPTY_STRING";
/// `metadata_fields[i]` entry failed `MetadataKey::new` (e.g. NUL byte).
pub const INVALID_METADATA_FIELDS_INVALID_KEY: &str = "INVALID_METADATA_FIELDS_INVALID_KEY";
/// Duplicate `metadata_fields[i]` entry.
pub const INVALID_METADATA_FIELDS_DUPLICATE: &str = "INVALID_METADATA_FIELDS_DUPLICATE";

/// Typed view of the `field_violations[].reason` codes carried by
/// [`crate::UsageCollectorError::InvalidArgument`].
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
    /// See [`GAUGE_COMPENSATION_REJECTED`].
    GaugeCompensationRejected,
    /// See [`MISSING_TIME_WINDOW`].
    MissingTimeWindow,
    /// See [`INVALID_BASE_GTS_ID`].
    InvalidBaseGtsId,
    /// See [`INVALID_METADATA_FIELDS_EMPTY_STRING`].
    MetadataFieldEmptyString,
    /// See [`INVALID_METADATA_FIELDS_INVALID_KEY`].
    MetadataFieldInvalidKey,
    /// See [`INVALID_METADATA_FIELDS_DUPLICATE`].
    MetadataFieldDuplicate,
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
            GAUGE_COMPENSATION_REJECTED => Self::GaugeCompensationRejected,
            MISSING_TIME_WINDOW => Self::MissingTimeWindow,
            INVALID_BASE_GTS_ID => Self::InvalidBaseGtsId,
            INVALID_METADATA_FIELDS_EMPTY_STRING => Self::MetadataFieldEmptyString,
            INVALID_METADATA_FIELDS_INVALID_KEY => Self::MetadataFieldInvalidKey,
            INVALID_METADATA_FIELDS_DUPLICATE => Self::MetadataFieldDuplicate,
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
            Self::GaugeCompensationRejected => GAUGE_COMPENSATION_REJECTED,
            Self::MissingTimeWindow => MISSING_TIME_WINDOW,
            Self::InvalidBaseGtsId => INVALID_BASE_GTS_ID,
            Self::MetadataFieldEmptyString => INVALID_METADATA_FIELDS_EMPTY_STRING,
            Self::MetadataFieldInvalidKey => INVALID_METADATA_FIELDS_INVALID_KEY,
            Self::MetadataFieldDuplicate => INVALID_METADATA_FIELDS_DUPLICATE,
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

/// `delete_usage_type` refused: still referenced by usage samples.
pub const USAGE_TYPE_REFERENCED: &str = "USAGE_TYPE_REFERENCED";
/// Deactivation targeted a record already `inactive` (one-way latch).
pub const ALREADY_INACTIVE: &str = "ALREADY_INACTIVE";
/// Same `idempotency_key`, canonical-field-different payload.
pub const IDEMPOTENCY_CONFLICT: &str = "IDEMPOTENCY_CONFLICT";
/// `corrects_id` referenced another compensation row.
pub const CORRECTS_ID_TARGETS_COMPENSATION: &str = "CORRECTS_ID_TARGETS_COMPENSATION";
/// `corrects_id` referenced a row in a different identity tuple.
pub const CORRECTS_ID_WRONG_SCOPE: &str = "CORRECTS_ID_WRONG_SCOPE";
/// `corrects_id` referenced an `inactive` row.
pub const CORRECTS_ID_INACTIVE: &str = "CORRECTS_ID_INACTIVE";

/// Typed view of the `context.reason` codes carried by
/// [`crate::UsageCollectorError::Conflict`] (the AIP-193 `Aborted` /
/// HTTP 409 category).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConflictReason {
    /// See [`USAGE_TYPE_REFERENCED`].
    UsageTypeReferenced,
    /// See [`ALREADY_INACTIVE`].
    AlreadyInactive,
    /// See [`IDEMPOTENCY_CONFLICT`].
    IdempotencyConflict,
    /// See [`CORRECTS_ID_TARGETS_COMPENSATION`].
    CorrectsIdTargetsCompensation,
    /// See [`CORRECTS_ID_WRONG_SCOPE`].
    CorrectsIdWrongScope,
    /// See [`CORRECTS_ID_INACTIVE`].
    CorrectsIdInactive,
    /// Unmodeled / future reason — preserves the raw wire string.
    Unknown(String),
}

impl ConflictReason {
    /// Project a wire `context.reason` string into the typed discriminator.
    /// Any unmodeled value is preserved in [`Self::Unknown`].
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            USAGE_TYPE_REFERENCED => Self::UsageTypeReferenced,
            ALREADY_INACTIVE => Self::AlreadyInactive,
            IDEMPOTENCY_CONFLICT => Self::IdempotencyConflict,
            CORRECTS_ID_TARGETS_COMPENSATION => Self::CorrectsIdTargetsCompensation,
            CORRECTS_ID_WRONG_SCOPE => Self::CorrectsIdWrongScope,
            CORRECTS_ID_INACTIVE => Self::CorrectsIdInactive,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Render the discriminator back to its wire `reason` string. Inverse of
    /// [`Self::from_wire`] for the modeled variants.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::UsageTypeReferenced => USAGE_TYPE_REFERENCED,
            Self::AlreadyInactive => ALREADY_INACTIVE,
            Self::IdempotencyConflict => IDEMPOTENCY_CONFLICT,
            Self::CorrectsIdTargetsCompensation => CORRECTS_ID_TARGETS_COMPENSATION,
            Self::CorrectsIdWrongScope => CORRECTS_ID_WRONG_SCOPE,
            Self::CorrectsIdInactive => CORRECTS_ID_INACTIVE,
            Self::Unknown(s) => s.as_str(),
        }
    }
}

impl fmt::Display for ConflictReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "reason_tests.rs"]
mod reason_tests;
