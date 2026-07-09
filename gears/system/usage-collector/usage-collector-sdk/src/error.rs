//! Usage Collector SDK error types.
//!
//! Two `thiserror::Error` enums make up the SDK's error vocabulary:
//!
//! - [`UsageCollectorError`] — public envelope returned by every
//!   [`crate::api::UsageCollectorClientV1`] method. A flat, AIP-193-shaped
//!   set of **seven category variants**: the discriminator
//!   inside a category is a typed [`crate::reason`] sub-enum
//!   ([`ValidationReason`] / [`ConflictReason`]) rather than a dedicated
//!   variant per failure.
//! - [`UsageCollectorPluginError`] — plugin-side vocabulary returned by
//!   every [`crate::plugin_api::UsageCollectorPluginV1`] method.
//!
//! This crate does NOT depend on `toolkit-canonical-errors`; the host crate
//! owns the lift to RFC-9457 `Problem` at the REST boundary. The category +
//! typed reason + `resource_type` carried here are exactly what the lift
//! projects onto the canonical envelope, so callers dispatch on the variant
//! (and, within a category, the typed reason) rather than parsing strings.

use rust_decimal::Decimal;
use thiserror::Error;
use uuid::Uuid;

use crate::gts::{USAGE_RECORD_RESOURCE, USAGE_TYPE_RESOURCE};
use crate::models::UsageTypeGtsId;
use crate::reason::{ConflictReason, ValidationReason};

/// Public error envelope for the Usage Collector SDK and REST surfaces.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UsageCollectorError {
    /// PDP denial on the requested operation (HTTP 403). `detail` is the
    /// PDP-supplied reason, kept for operator logs; the host lift drops it
    /// from the public wire body (the SDK never paraphrases PDP detail) and
    /// emits `context.reason="AUTHZ"`.
    #[error("authorization denied: {detail}")]
    PermissionDenied {
        /// PDP-supplied reason (operator-log facing; not echoed to the wire).
        detail: String,
    },

    /// Request-shape / semantics validation failure (HTTP 400). `field` is
    /// the attributed request field, `reason` the typed
    /// [`ValidationReason`] discriminator, and `detail` the wire
    /// `field_violations[0].description`. `resource_type` identifies the GTS
    /// resource the violation is about (a `gts_id`-shaped field violation
    /// attributes to the usage type even on the ingestion surface);
    /// `resource_name`, when present, is the offending `gts_id`.
    #[error("invalid argument [{field}/{reason}]: {detail}")]
    InvalidArgument {
        /// GTS resource type — [`USAGE_TYPE_RESOURCE`] or
        /// [`USAGE_RECORD_RESOURCE`].
        resource_type: String,
        /// Offending resource name (`gts_id`), when the violation is about a
        /// specific resource; `None` otherwise.
        resource_name: Option<String>,
        /// Attributed request field (`value`, `records`, `metadata`, …).
        field: String,
        /// Typed `field_violations[0].reason` discriminator.
        reason: ValidationReason,
        /// Wire `field_violations[0].description`.
        detail: String,
    },

    /// Referenced resource not found (HTTP 404). `resource_type` is the GTS
    /// type, `name` the raw identifier (`gts_id` or record UUID).
    #[error("not found [{resource_type}]: {detail}")]
    NotFound {
        /// GTS resource type — [`USAGE_TYPE_RESOURCE`] or
        /// [`USAGE_RECORD_RESOURCE`].
        resource_type: String,
        /// Raw identifier whose row was not present.
        name: String,
        /// Wire `detail` message.
        detail: String,
    },

    /// Duplicate-on-create conflict (HTTP 409). Identical-payload
    /// resubmission is idempotent and returns the stored row on `Ok`.
    #[error("already exists [{resource_type}]: {detail}")]
    AlreadyExists {
        /// GTS resource type — [`USAGE_TYPE_RESOURCE`].
        resource_type: String,
        /// Raw identifier (`gts_id`) that collided.
        name: String,
        /// Wire `detail` message.
        detail: String,
    },

    /// State / concurrency / referential-integrity conflict (HTTP 409,
    /// AIP-193 `Aborted`). `reason` is the typed [`ConflictReason`]
    /// discriminator carried on the wire `context.reason`; `resource_type` /
    /// `name` identify the row involved.
    #[error("conflict [{reason}]: {detail}")]
    Conflict {
        /// GTS resource type — [`USAGE_TYPE_RESOURCE`] or
        /// [`USAGE_RECORD_RESOURCE`].
        resource_type: String,
        /// Raw identifier of the row involved (`gts_id` / record UUID).
        name: String,
        /// Typed `context.reason` discriminator.
        reason: ConflictReason,
        /// Wire `detail` message.
        detail: String,
    },

    /// Transient infrastructure unavailability (HTTP 503). Covers
    /// host-structural readiness (plugin / types-registry), plugin-reported
    /// transience, and PDP-transport outages — operator triage reads the
    /// curated `detail` string. Carries an optional `retry_after_seconds`
    /// hint. The only retryable classification ([`Self::is_retryable`]).
    #[error("service unavailable: {detail}")]
    ServiceUnavailable {
        /// Optional retry hint forwarded onto the `Retry-After` slot.
        retry_after_seconds: Option<u64>,
        /// Operator-facing detail (DSN-free, pre-redacted at construction).
        detail: String,
    },

    /// Unclassified failure (HTTP 500). `detail` MUST be DSN-free and
    /// pre-redacted at the construction site.
    #[error("internal error: {detail}")]
    Internal {
        /// Pre-redacted operator-facing detail.
        detail: String,
    },
}

impl UsageCollectorError {
    // ── PermissionDenied (403) ──────────────────────────────────────────

    /// PDP denial. `detail` is the PDP-supplied reason (operator-log facing).
    #[must_use]
    pub fn permission_denied(detail: impl Into<String>) -> Self {
        Self::PermissionDenied {
            detail: detail.into(),
        }
    }

    // ── InvalidArgument (400) ───────────────────────────────────────────

    /// Counter ordinary record carried a negative value (`value >= 0`).
    #[must_use]
    pub fn negative_counter_value(value: Decimal) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            resource_name: None,
            field: "value".to_owned(),
            reason: ValidationReason::SemanticsViolation,
            detail: format!("counter ordinary record requires value >= 0 (got {value})"),
        }
    }

    /// Counter compensation row carried a non-negative value (`value < 0`).
    #[must_use]
    pub fn non_negative_counter_compensation(value: Decimal) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            resource_name: None,
            field: "value".to_owned(),
            reason: ValidationReason::SemanticsViolation,
            detail: format!("counter compensation requires value < 0 (got {value})"),
        }
    }

    /// Batch submission size out of bounds (empty or over the per-call cap).
    #[must_use]
    pub fn invalid_batch_size(actual: usize, min: usize, max: usize) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            resource_name: None,
            field: "records".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!("batch size {actual} out of bounds (expected [{min}, {max}])"),
        }
    }

    /// Serialized metadata exceeded the per-record size cap.
    #[must_use]
    pub fn metadata_size_exceeded(size: usize, cap: usize) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            resource_name: None,
            field: "metadata".to_owned(),
            reason: ValidationReason::MetadataValidation,
            detail: format!("metadata size {size} bytes exceeds cap {cap} bytes"),
        }
    }

    /// `CreateUsageType.metadata_fields[index]` was not a well-formed
    /// metadata key. `empty` selects the empty-string vs. invalid-key reason.
    #[must_use]
    pub fn invalid_metadata_field(index: usize, empty: bool) -> Self {
        let reason = if empty {
            ValidationReason::MetadataFieldEmptyString
        } else {
            ValidationReason::MetadataFieldInvalidKey
        };
        let wire = reason.as_wire().to_owned();
        Self::InvalidArgument {
            resource_type: USAGE_TYPE_RESOURCE.to_owned(),
            resource_name: None,
            field: format!("metadata_fields[{index}]"),
            reason,
            detail: format!("metadata_fields[{index}] rejected: {wire}"),
        }
    }

    /// Duplicate entry in `CreateUsageType.metadata_fields`.
    #[must_use]
    pub fn duplicate_metadata_field(index: usize) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_TYPE_RESOURCE.to_owned(),
            resource_name: None,
            field: format!("metadata_fields[{index}]"),
            reason: ValidationReason::MetadataFieldDuplicate,
            detail: format!("metadata_fields[{index}] is a duplicate entry"),
        }
    }

    /// A raw / aggregated query omitted the mandatory bounded `created_at`
    /// window.
    #[must_use]
    pub fn missing_time_window() -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            resource_name: None,
            field: "$filter".to_owned(),
            reason: ValidationReason::MissingTimeWindow,
            detail: "query requires a bounded created_at window: supply both a lower \
                     (created_at ge|gt ...) and an upper (created_at le|lt ...) bound as \
                     top-level $filter conjuncts"
                .to_owned(),
        }
    }

    /// `UsageTypeGtsId::new` rejected `raw` — malformed / wrong-base `gts_id`.
    #[must_use]
    pub fn invalid_usage_type_gts_id(raw: &str, reason: &str) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_TYPE_RESOURCE.to_owned(),
            resource_name: None,
            field: "gts_id".to_owned(),
            reason: ValidationReason::InvalidBaseGtsId,
            detail: format!("gts_id `{raw}` rejected: {reason}"),
        }
    }

    /// `UsageKind::from_str` received a string other than `counter`/`gauge`.
    #[must_use]
    pub fn invalid_usage_kind(raw: &str) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_TYPE_RESOURCE.to_owned(),
            resource_name: None,
            field: "kind".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!("unknown usage kind `{raw}`; expected `counter` or `gauge`"),
        }
    }

    /// Build a record-surface validating-newtype `InvalidArgument` whose wire
    /// `detail` is the newtype's self-describing reason. `field` attributes
    /// the violation (`metadata`, `resource_ref`, …).
    #[must_use]
    fn newtype_validation(field: &str, detail: impl Into<String>) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            resource_name: None,
            field: field.to_owned(),
            reason: ValidationReason::Validation,
            detail: detail.into(),
        }
    }

    /// `MetadataKey::new` rejected the input. `field` is `metadata`.
    #[must_use]
    pub fn invalid_metadata_key(detail: impl Into<String>) -> Self {
        Self::newtype_validation("metadata", detail)
    }

    /// `MetadataFilter::new` rejected the input. `field` is `metadata_filter`.
    #[must_use]
    pub fn invalid_metadata_filter(detail: impl Into<String>) -> Self {
        Self::newtype_validation("metadata_filter", detail)
    }

    /// `ResourceRef::new` rejected the input. `field` is `resource_ref`.
    #[must_use]
    pub fn invalid_resource_ref(detail: impl Into<String>) -> Self {
        Self::newtype_validation("resource_ref", detail)
    }

    /// `SubjectRef::new` rejected the input. `field` is `subject_ref`.
    #[must_use]
    pub fn invalid_subject_ref(detail: impl Into<String>) -> Self {
        Self::newtype_validation("subject_ref", detail)
    }

    /// `IdempotencyKey::new` rejected the input. `field` is `idempotency_key`.
    #[must_use]
    pub fn invalid_idempotency_key(detail: impl Into<String>) -> Self {
        Self::newtype_validation("idempotency_key", detail)
    }

    /// Compensation submitted against a gauge usage type. Emitted on the
    /// ingestion surface, so the wire `resource_type` is the usage **record**
    /// resource, with `resource_name` carrying the offending gauge `gts_id`
    /// (`field` = `corrects_id`).
    #[must_use]
    pub fn gauge_compensation_rejected(gts_id: &UsageTypeGtsId) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            resource_name: Some(gts_id.as_ref().to_owned()),
            field: "corrects_id".to_owned(),
            reason: ValidationReason::GaugeCompensationRejected,
            detail: format!("compensation against gauge usage type {gts_id} is rejected"),
        }
    }

    /// Ingestion supplied a metadata key not declared in the usage type's
    /// closed `metadata_fields`. Attributes to the usage type resource.
    #[must_use]
    pub fn unknown_metadata_key(gts_id: &UsageTypeGtsId, key: &str) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_TYPE_RESOURCE.to_owned(),
            resource_name: Some(gts_id.as_ref().to_owned()),
            field: "metadata".to_owned(),
            reason: ValidationReason::UnknownMetadataKey,
            detail: format!("unknown metadata key '{key}' for usage type {gts_id}"),
        }
    }

    // ── NotFound (404) ──────────────────────────────────────────────────

    /// Catalog `gts_id` not present (catalog admin op, ingestion, or query).
    #[must_use]
    pub fn usage_type_not_found(gts_id: &UsageTypeGtsId) -> Self {
        Self::NotFound {
            resource_type: USAGE_TYPE_RESOURCE.to_owned(),
            name: gts_id.as_ref().to_owned(),
            detail: format!("usage type not found: {gts_id}"),
        }
    }

    /// Deactivation / get referenced a `UsageRecord.uuid` that does not exist.
    #[must_use]
    pub fn usage_record_not_found(id: Uuid) -> Self {
        Self::NotFound {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            name: id.to_string(),
            detail: format!("usage record not found: {id}"),
        }
    }

    /// A compensation's `corrects_id` referenced a row that does not exist.
    /// (Collapsed into the record `NotFound` category — no distinct wire
    /// `context.reason`; the `detail` text carries the human distinction.)
    #[must_use]
    pub fn corrects_id_not_found(corrects_id: Uuid) -> Self {
        Self::NotFound {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            name: corrects_id.to_string(),
            detail: format!(
                "corrects_id {corrects_id} does not reference an existing usage record"
            ),
        }
    }

    // ── AlreadyExists (409) ─────────────────────────────────────────────

    /// `create_usage_type` collided with an existing, payload-different row.
    #[must_use]
    pub fn usage_type_already_exists(gts_id: &UsageTypeGtsId) -> Self {
        Self::AlreadyExists {
            resource_type: USAGE_TYPE_RESOURCE.to_owned(),
            name: gts_id.as_ref().to_owned(),
            detail: format!("usage type already exists: {gts_id}"),
        }
    }

    // ── Conflict / Aborted (409) ────────────────────────────────────────

    /// `delete_usage_type` refused: still referenced by `sample_ref_count`
    /// samples (at least `1`).
    #[must_use]
    pub fn usage_type_referenced(gts_id: &UsageTypeGtsId, sample_ref_count: u64) -> Self {
        Self::Conflict {
            resource_type: USAGE_TYPE_RESOURCE.to_owned(),
            name: gts_id.as_ref().to_owned(),
            reason: ConflictReason::UsageTypeReferenced,
            detail: format!(
                "usage type {gts_id} is still referenced by {sample_ref_count} samples"
            ),
        }
    }

    /// Deactivation targeted a record whose status was already `Inactive`.
    #[must_use]
    pub fn already_inactive(id: Uuid) -> Self {
        Self::Conflict {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            name: id.to_string(),
            reason: ConflictReason::AlreadyInactive,
            detail: format!("usage record already inactive: {id}"),
        }
    }

    /// Same `idempotency_key`, canonical-field-different payload. `name`
    /// carries the previously persisted record UUID so the caller can
    /// reconcile.
    #[must_use]
    pub fn idempotency_conflict(idempotency_key: &str, existing_uuid: Uuid) -> Self {
        Self::Conflict {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            name: existing_uuid.to_string(),
            reason: ConflictReason::IdempotencyConflict,
            detail: format!(
                "idempotency key {idempotency_key} already bound to record {existing_uuid}"
            ),
        }
    }

    /// A compensation's `corrects_id` referenced another compensation row.
    #[must_use]
    pub fn corrects_id_targets_compensation(corrects_id: Uuid) -> Self {
        Self::Conflict {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            name: corrects_id.to_string(),
            reason: ConflictReason::CorrectsIdTargetsCompensation,
            detail: format!("corrects_id {corrects_id} targets a compensation row"),
        }
    }

    /// A compensation's `corrects_id` referenced a row in a different
    /// `(tenant, usage type, resource, subject)` identity tuple.
    #[must_use]
    pub fn corrects_id_wrong_scope(corrects_id: Uuid) -> Self {
        Self::Conflict {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            name: corrects_id.to_string(),
            reason: ConflictReason::CorrectsIdWrongScope,
            detail: format!(
                "corrects_id {corrects_id} references a row in a different tenant, usage type, resource, or subject"
            ),
        }
    }

    /// A compensation's `corrects_id` referenced an `inactive` row.
    #[must_use]
    pub fn corrects_id_inactive(corrects_id: Uuid) -> Self {
        Self::Conflict {
            resource_type: USAGE_RECORD_RESOURCE.to_owned(),
            name: corrects_id.to_string(),
            reason: ConflictReason::CorrectsIdInactive,
            detail: format!("corrects_id {corrects_id} references an inactive usage record"),
        }
    }

    // ── ServiceUnavailable (503) ────────────────────────────────────────

    /// No scoped storage-plugin client was available (host-structural
    /// readiness).
    #[must_use]
    pub fn plugin_unavailable() -> Self {
        Self::ServiceUnavailable {
            retry_after_seconds: None,
            detail: "storage plugin unavailable".to_owned(),
        }
    }

    /// The `types-registry` lookup the host uses to bind the scoped client
    /// returned an unavailable result.
    #[must_use]
    pub fn types_registry_unavailable() -> Self {
        Self::ServiceUnavailable {
            retry_after_seconds: None,
            detail: "types-registry unavailable".to_owned(),
        }
    }

    /// Plugin-reported transient / PDP-transport outage. `detail` is curated
    /// for operator triage; `retry_after_seconds` is an optional hint.
    #[must_use]
    pub fn service_unavailable(
        detail: impl Into<String>,
        retry_after_seconds: Option<u64>,
    ) -> Self {
        Self::ServiceUnavailable {
            retry_after_seconds,
            detail: detail.into(),
        }
    }

    /// Unclassified failure. `detail` MUST be DSN-free / pre-redacted.
    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal {
            detail: detail.into(),
        }
    }

    /// `true` for retryable classifications — the principal semantic the SDK
    /// exposes to retry-aware callers. Plugin `Transient`, host readiness,
    /// and PDP-transport failures all lift to [`Self::ServiceUnavailable`].
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::ServiceUnavailable { .. })
    }
}

/// Plugin-side error vocabulary returned by every
/// [`crate::plugin_api::UsageCollectorPluginV1`] method.
///
/// Translated into [`UsageCollectorError`] by the host at the dispatch
/// boundary, routed through the host-internal domain-error vocabulary (this
/// crate intentionally provides no direct `From` to the public envelope).
/// Structural unavailability is host-side and surfaces as a
/// [`UsageCollectorError::ServiceUnavailable`], not as a plugin error.
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
    /// boundary, forwarding the optional `retry_after_seconds` hint, and
    /// is observed as retryable by [`UsageCollectorError::is_retryable`].
    #[error("transient plugin error: {detail}")]
    Transient {
        /// Operator-facing detail (DSN-free, pre-redacted at the plugin).
        detail: String,
        /// Optional retry-after hint (seconds) forwarded onto the
        /// `ServiceUnavailable` envelope's `Retry-After` slot. Plugins
        /// that have no actionable hint pass `None`.
        retry_after_seconds: Option<u64>,
    },

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

    /// Constructs a [`UsageCollectorPluginError::Transient`] with no retry
    /// hint. Plugins that know a sensible delay should use
    /// [`Self::transient_with_retry`] instead.
    #[must_use]
    pub fn transient(detail: impl Into<String>) -> Self {
        Self::Transient {
            detail: detail.into(),
            retry_after_seconds: None,
        }
    }

    /// Constructs a [`UsageCollectorPluginError::Transient`] carrying an
    /// optional retry hint. The hint is forwarded to the
    /// [`UsageCollectorError::ServiceUnavailable`] envelope at the
    /// dispatch boundary.
    #[must_use]
    pub fn transient_with_retry(
        detail: impl Into<String>,
        retry_after_seconds: Option<u64>,
    ) -> Self {
        Self::Transient {
            detail: detail.into(),
            retry_after_seconds,
        }
    }
}
