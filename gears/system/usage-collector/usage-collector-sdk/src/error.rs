//! Usage Collector SDK error types.
//!
//! Two flat `thiserror::Error` enums make up the SDK's error vocabulary:
//!
//! - [`UsageCollectorError`] — public envelope returned by every
//!   [`crate::api::UsageCollectorClientV1`] method.
//! - [`UsageCollectorPluginError`] — plugin-side vocabulary returned by
//!   every [`crate::plugin_api::UsageCollectorPluginV1`] method.
//!
//! This crate does NOT depend on `toolkit-canonical-errors`; the host crate
//! owns the lift to RFC-9457 `Problem` at the REST boundary.
//!
//! Validation failures are exposed as typed variants (one per validating
//! newtype, plus `NegativeCounterValue` / `NonNegativeCounterCompensation`
//! for the counter/gauge value matrix and a few aggregate checks). Callers
//! dispatch on the variant rather than parsing strings.

use rust_decimal::Decimal;
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::models::UsageTypeGtsId;

/// Public error envelope for the Usage Collector SDK and REST surfaces.
///
/// Flat, transport-agnostic, and free of any `toolkit-canonical-errors`
/// dependency. The variant → HTTP-status mapping is owned by the host crate,
/// not here. Catalog variants are keyed by `gts_id`; validation variants
/// carry typed payloads so callers do not need to parse strings.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UsageCollectorError {
    /// PDP denial on the requested operation. The message is the
    /// PDP-supplied reason; the SDK does not paraphrase.
    #[error("authorization denied: {0}")]
    Authorization(String),

    // ── Validation: counter / gauge value matrix ────────────────────────
    /// Counter ordinary record carried a negative value. Counter semantics
    /// require `value >= 0` on ordinary submissions; the only path to a
    /// negative delta is a compensation row (see
    /// [`Self::NonNegativeCounterCompensation`] for the inverse rule).
    #[error("counter ordinary record requires value >= 0, got {value}")]
    NegativeCounterValue {
        /// Caller-supplied value that violated the counter-usage rule.
        value: Decimal,
    },

    /// Counter compensation row carried a non-negative value. Counter
    /// compensations are the only way to record a negative delta and MUST
    /// carry `value < 0`
    #[error("counter compensation requires value < 0, got {value}")]
    NonNegativeCounterCompensation {
        /// Caller-supplied value that violated the compensation rule.
        value: Decimal,
    },

    // ── Validation: aggregate / structural ──────────────────────────────
    /// Batch submission size out of bounds. Empty batches and batches
    /// exceeding the per-call cap both surface through this variant.
    #[error("batch size {actual} out of bounds (expected [{min}, {max}])")]
    InvalidBatchSize {
        /// Caller-supplied batch size.
        actual: usize,
        /// Minimum admitted batch size (inclusive).
        min: usize,
        /// Maximum admitted batch size (inclusive).
        max: usize,
    },

    /// Serialized metadata exceeded the per-record size cap.
    #[error("metadata size {size} bytes exceeds cap {cap} bytes")]
    MetadataSizeExceeded {
        /// Caller-supplied serialized metadata size in bytes.
        size: usize,
        /// Operator-configured cap in bytes.
        cap: usize,
    },

    /// Entry in `CreateUsageType.metadata_fields` was not a well-formed
    /// metadata key (empty string, NUL byte, etc.). Carries the offending
    /// index plus the underlying reason.
    #[error("/metadata_fields/{index}: {reason}")]
    InvalidMetadataField {
        /// Zero-based index into the wire `metadata_fields` list.
        index: usize,
        /// Underlying reason — e.g. `"invalid_metadata_fields_empty_string"`
        /// or `"invalid_metadata_fields_key: metadata key must not contain NUL bytes"`.
        reason: String,
    },

    /// Duplicate entry in `CreateUsageType.metadata_fields`.
    #[error("/metadata_fields/{index}: invalid_metadata_fields_duplicate")]
    DuplicateMetadataField {
        /// Zero-based index of the duplicate entry.
        index: usize,
    },

    // ── Validation: SDK newtypes ────────────────────────────────────────
    /// `UsageTypeGtsId::new` rejected the input — malformed GTS id, type id
    /// (trailing `~`), wrong base, or missing derivation segment. The raw
    /// input is echoed back to the caller; `reason` carries the underlying
    /// detail from the GTS validator or the derivation check.
    #[error("{reason}")]
    InvalidUsageTypeGtsId {
        /// Caller-supplied raw string.
        raw: String,
        /// Underlying reason (already includes `usage type gts_id `{raw}`…`).
        reason: String,
    },

    /// `MetadataKey::new` rejected the input (empty or contains a NUL byte).
    /// The `reason` already carries the `metadata key …` prefix.
    #[error("{reason}")]
    InvalidMetadataKey {
        /// Underlying reason — `"metadata key must not be empty"` or
        /// `"metadata key must not contain NUL bytes"`.
        reason: String,
    },

    /// `MetadataFilter::new` rejected the input — empty values set, or the
    /// supplied key failed `MetadataKey::new`. `reason` is self-describing.
    #[error("{reason}")]
    InvalidMetadataFilter {
        /// Underlying reason.
        reason: String,
    },

    /// `ResourceRef::new` rejected the input — `resource_id` or
    /// `resource_type` empty or contained a NUL byte. `reason` carries the
    /// field-prefixed detail.
    #[error("{reason}")]
    InvalidResourceRef {
        /// Underlying reason — e.g. `"resource_id must not be empty"`.
        reason: String,
    },

    /// `SubjectRef::new` rejected the input — `subject_id` empty / NUL or
    /// `subject_type` supplied as `Some("")` / NUL. `reason` carries the
    /// field-prefixed detail.
    #[error("{reason}")]
    InvalidSubjectRef {
        /// Underlying reason — e.g. `"subject_id must not be empty"`.
        reason: String,
    },

    /// `IdempotencyKey::new` rejected the input (empty or contains a NUL byte).
    /// `reason` already carries the `idempotency_key …` prefix.
    #[error("{reason}")]
    InvalidIdempotencyKey {
        /// Underlying reason.
        reason: String,
    },

    /// The URL path `id` segment was not a valid UUID.
    #[error("usage record id `{raw}` is not a valid UUID")]
    InvalidUsageRecordId {
        /// Caller-supplied raw string.
        raw: String,
    },

    /// `TimeWindow::new` rejected the bounds (`from >= to`).
    #[error("time window `from` ({from}) must be strictly less than `to` ({to})")]
    InvalidTimeRange {
        /// Caller-supplied lower bound.
        from: OffsetDateTime,
        /// Caller-supplied upper bound.
        to: OffsetDateTime,
    },

    /// `UsageKind::from_str` received a wire string that was not
    /// `"counter"` or `"gauge"`.
    #[error("unknown usage kind `{raw}`; expected `counter` or `gauge`")]
    InvalidUsageKind {
        /// Caller-supplied raw string.
        raw: String,
    },

    // ── Catalog / domain ───────────────────────────────────────────────
    /// The referenced `gts_id` is not present in the plugin-owned catalog.
    /// Surfaces both catalog-admin misses (`get_usage_type` /
    /// `delete_usage_type`) and ingestion / aggregated-query references to
    /// an unregistered usage type — the wire shape is identical.
    #[error("usage type not found: {gts_id}")]
    UsageTypeNotFound {
        /// Catalog `gts_id` whose row was not present.
        gts_id: UsageTypeGtsId,
    },

    /// `create_usage_type` collided with an existing row whose payload
    /// differs. Identical-payload resubmission is idempotent and returns
    /// the stored row on `Ok`.
    #[error("usage type already exists: {gts_id}")]
    UsageTypeAlreadyExists {
        /// Catalog `gts_id` that collided.
        gts_id: UsageTypeGtsId,
    },

    /// `delete_usage_type` was rejected because the usage type is still
    /// referenced by usage samples. The caller MUST drain or deactivate
    /// dependents before retrying.
    #[error("usage type {gts_id} is still referenced by {sample_ref_count} samples")]
    UsageTypeReferenced {
        /// Catalog `gts_id` that could not be deleted.
        gts_id: UsageTypeGtsId,
        /// Bounded sample count of referencing rows (at least `1`).
        sample_ref_count: u64,
    },

    /// Ingestion supplied a metadata key not declared in the usage type's
    /// `metadata_fields`.
    #[error("unknown metadata key '{key}' for usage type {gts_id}")]
    UnknownMetadataKey {
        /// Catalog `gts_id` whose closed-shape contract rejected the key.
        gts_id: UsageTypeGtsId,
        /// Offending undeclared metadata key name.
        key: String,
    },

    /// No scoped `dyn UsageCollectorPluginV1` client was available at the
    /// time of the call. The SPI exposes no `Unready` variant and no
    /// `ready()` probe.
    #[error("plugin unavailable")]
    PluginUnavailable,

    /// The `types-registry` lookup the plugin host uses to bind the scoped
    /// client returned an unavailable result.
    #[error("types registry unavailable")]
    TypesRegistryUnavailable,

    /// `create_usage_record` was retried with the same `idempotency_key`
    /// but canonical-field-different payload. Exact-equality retries
    /// instead silently return the previously persisted record on `Ok`.
    /// Carries the UUID of the previously persisted record bound to the
    /// key so the caller can `get_usage_record` and reconcile.
    #[error("idempotency conflict: key {idempotency_key} already bound to record {existing_uuid}")]
    IdempotencyConflict {
        /// Caller-supplied idempotency key.
        idempotency_key: String,
        /// `UsageRecord.uuid` of the row the key is already bound to.
        existing_uuid: Uuid,
    },

    /// Deactivation referenced a `UsageRecord.uuid` that does not exist.
    #[error("usage record not found: {id}")]
    UsageRecordNotFound {
        /// Caller-supplied target `UsageRecord.uuid`.
        id: Uuid,
    },

    /// Deactivation targeted a record whose `status` was already `Inactive`.
    /// The one-way `active → inactive` latch rejects a second deactivation.
    #[error("usage record already inactive: {id}")]
    AlreadyInactive {
        /// Caller-supplied target `UsageRecord.uuid`.
        id: Uuid,
    },

    /// A compensation submission targeted a gauge usage type. Gauges have
    /// no `SUM` semantics; the only correction for a gauge is deactivation.
    #[error("gauge compensation rejected for usage type {gts_id}")]
    GaugeCompensationRejected {
        /// Catalog `gts_id` of the gauge usage type.
        gts_id: UsageTypeGtsId,
    },

    /// A compensation's `corrects_id` referenced a row that does not exist.
    #[error("corrects_id {corrects_id} does not reference an existing usage record")]
    CorrectsIdNotFound {
        /// Caller-supplied `corrects_id` whose target was not found.
        corrects_id: Uuid,
    },

    /// A compensation's `corrects_id` referenced another compensation row;
    /// compensating a compensation is a non-goal.
    #[error("corrects_id {corrects_id} targets a compensation row")]
    CorrectsIdTargetsCompensation {
        /// Caller-supplied `corrects_id` that targets a compensation row.
        corrects_id: Uuid,
    },

    /// A compensation's `corrects_id` referenced a row whose
    /// `(tenant_id, usage_type_gts_id, resource_ref, subject_ref)` identity
    /// tuple differs from the incoming row. `subject_ref` presence is part
    /// of the identity — `None` vs `Some(_)` is a scope mismatch.
    #[error(
        "corrects_id {corrects_id} references a row in a different tenant, usage type, resource, or subject"
    )]
    CorrectsIdWrongScope {
        /// Caller-supplied `corrects_id` whose scope differs.
        corrects_id: Uuid,
    },

    /// A compensation's `corrects_id` referenced an `inactive` row.
    #[error("corrects_id {corrects_id} references an inactive usage record")]
    CorrectsIdInactive {
        /// Caller-supplied `corrects_id` whose target is inactive.
        corrects_id: Uuid,
    },

    /// Transient infrastructure failure (plugin-reported `Transient` or
    /// host-side transient); carries an optional `retry_after_seconds`
    /// hint.
    #[error("service unavailable: {detail}")]
    ServiceUnavailable {
        /// Operator-facing detail.
        detail: String,
        /// Optional retry hint.
        retry_after_seconds: Option<u64>,
    },

    /// Unclassified failure. The detail MUST be DSN-free and pre-redacted
    /// at the construction site.
    #[error("internal error: {0}")]
    Internal(String),
}

impl UsageCollectorError {
    /// `true` for retryable classifications. The principal semantic the
    /// SDK exposes to retry-aware callers: plugin `Transient` failures
    /// lift to [`Self::ServiceUnavailable`] which satisfies this
    /// predicate.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::PluginUnavailable
                | Self::ServiceUnavailable { .. }
                | Self::TypesRegistryUnavailable
        )
    }
}

/// Plugin-side error vocabulary returned by every
/// [`crate::plugin_api::UsageCollectorPluginV1`] method.
///
/// Translated into [`UsageCollectorError`] at the dispatch boundary by the
/// `From` impl below. Structural unavailability is host-side and surfaces as
/// [`UsageCollectorError::PluginUnavailable`], not as a plugin error.
///
/// Plugins classify a failure into one of three non-domain buckets:
///
/// - [`Self::Transient`] — retryable backend failure (downstream timeout,
///   connection reset, upstream 5xx). Lifts to
///   [`UsageCollectorError::ServiceUnavailable`].
/// - [`Self::Internal`] — non-retryable unclassified failure (plugin
///   invariant broken, uncategorized backend error). Lifts to
///   [`UsageCollectorError::Internal`].
/// - The catalog / record variants below — typed domain outcomes.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UsageCollectorPluginError {
    /// Retryable backend failure — safe to retry (downstream timeout,
    /// connection reset, upstream 5xx). Lifts to
    /// [`UsageCollectorError::ServiceUnavailable`] at the dispatch
    /// boundary and is observed as retryable by
    /// [`UsageCollectorError::is_retryable`].
    #[error("transient plugin error: {0}")]
    Transient(String),

    /// `get_usage_type` / `delete_usage_type` referenced a `gts_id` absent
    /// from the catalog.
    #[error("usage type not found: {gts_id}")]
    UsageTypeNotFound {
        /// Catalog `gts_id` that was not found.
        gts_id: UsageTypeGtsId,
    },

    /// `create_usage_type` collided with an existing row whose payload
    /// differs.
    #[error("usage type already exists: {gts_id}")]
    UsageTypeAlreadyExists {
        /// Catalog `gts_id` that collided.
        gts_id: UsageTypeGtsId,
    },

    /// `delete_usage_type` was rejected because the usage type is still
    /// referenced by `sample_ref_count` samples (a bounded count, at
    /// least `1`).
    #[error("usage type {gts_id} is still referenced by {sample_ref_count} samples")]
    UsageTypeReferenced {
        /// Catalog `gts_id` that could not be deleted.
        gts_id: UsageTypeGtsId,
        /// Bounded sample count of referencing rows.
        sample_ref_count: u64,
    },

    /// Idempotency conflict at the persistence boundary: the supplied
    /// `idempotency_key` is already bound to a different stored record.
    /// Carries the UUID of the previously persisted record (the plugin
    /// detects the conflict against a specific row, so the row's UUID is
    /// the actionable handle for the gateway).
    #[error("idempotency conflict: key {idempotency_key} already bound to record {existing_uuid}")]
    IdempotencyConflict {
        /// Caller-supplied idempotency key.
        idempotency_key: String,
        /// `UsageRecord.uuid` of the previously persisted row the key is
        /// already bound to.
        existing_uuid: Uuid,
    },

    /// `get_usage_record` / `deactivate_usage_record` referenced an `id`
    /// that does not exist.
    #[error("usage record not found: {id}")]
    UsageRecordNotFound {
        /// Caller-supplied target `UsageRecord.uuid`.
        id: Uuid,
    },

    /// `deactivate_usage_record` targeted a record whose status was
    /// already `Inactive`.
    #[error("usage record already inactive: {id}")]
    UsageRecordAlreadyInactive {
        /// Caller-supplied target `UsageRecord.uuid`.
        id: Uuid,
    },

    /// Non-retryable unclassified plugin-side failure (plugin invariant
    /// broken, uncategorized backend error). Use [`Self::Transient`] for
    /// retryable backend errors.
    #[error("plugin internal error: {0}")]
    Internal(String),
}

impl UsageCollectorPluginError {
    /// Constructs a [`UsageCollectorPluginError::Internal`].
    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal(detail.into())
    }

    /// Constructs a [`UsageCollectorPluginError::Transient`].
    #[must_use]
    pub fn transient(detail: impl Into<String>) -> Self {
        Self::Transient(detail.into())
    }
}

/// Dispatch-boundary translation from plugin-side vocabulary to the public
/// SDK envelope. Catalog variants route 1:1 to their SDK counterparts;
/// `Transient` lifts to the retryable `ServiceUnavailable` envelope and
/// `Internal` lifts to the non-retryable `Internal` envelope.
impl From<UsageCollectorPluginError> for UsageCollectorError {
    fn from(err: UsageCollectorPluginError) -> Self {
        match err {
            UsageCollectorPluginError::Transient(detail) => Self::ServiceUnavailable {
                detail,
                retry_after_seconds: None,
            },
            UsageCollectorPluginError::Internal(detail) => Self::Internal(detail),
            UsageCollectorPluginError::UsageTypeNotFound { gts_id } => {
                Self::UsageTypeNotFound { gts_id }
            }
            UsageCollectorPluginError::UsageTypeAlreadyExists { gts_id } => {
                Self::UsageTypeAlreadyExists { gts_id }
            }
            UsageCollectorPluginError::UsageTypeReferenced {
                gts_id,
                sample_ref_count,
            } => Self::UsageTypeReferenced {
                gts_id,
                sample_ref_count,
            },
            UsageCollectorPluginError::IdempotencyConflict {
                idempotency_key,
                existing_uuid,
            } => Self::IdempotencyConflict {
                idempotency_key,
                existing_uuid,
            },
            UsageCollectorPluginError::UsageRecordNotFound { id } => {
                Self::UsageRecordNotFound { id }
            }
            UsageCollectorPluginError::UsageRecordAlreadyInactive { id } => {
                Self::AlreadyInactive { id }
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
