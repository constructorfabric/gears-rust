//! Usage Collector SDK error types.
//!
//! Two `thiserror::Error` enums make up the SDK's error vocabulary:
//!
//! - [`UsageCollectorError`] — public envelope returned by every
//!   [`crate::v2::api::UsageCollectorClientV2`] method. A flat, AIP-193-shaped
//!   set of **nine category variants**: the discriminator
//!   inside a category is normally a typed [`crate::v2::reason`] sub-enum
//!   ([`ValidationReason`], [`ConflictReason`], [`NotFoundReason`]) rather
//!   than a dedicated variant per failure.
//! - [`UsageCollectorPluginError`] — plugin-side vocabulary returned by
//!   every [`crate::v2::plugin_api::UsageCollectorPluginV2`] method.
//!
//! This crate does NOT depend on `toolkit-canonical-errors`; the host crate
//! owns the lift to RFC-9457 `Problem` at the REST boundary. The category +
//! typed reason + `resource_type` carried here are what the lift projects
//! onto the canonical envelope, so callers dispatch on the variant (and,
//! within a category, the typed reason) rather than parsing strings.

use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::v2::aggregation::AggregationDimension;
use crate::v2::gts::USAGE_RECORD_RESOURCE;
use crate::v2::id::UsageRecordIdInvalid;
use crate::v2::models::{
    BACKFILL_ROUTE_PATH, IdempotencyKey, MetadataKey, MeterTypeId, WINDOW_END_FIELD,
    WINDOW_START_FIELD,
};
use crate::v2::reason::{ConflictReason, NotFoundReason, ValidationReason};

/// Renders an instant the way a caller-facing `detail` must echo it: RFC
/// 3339, matching the wire `Timestamp` contract.
///
/// Formatting can fail, and on the *value* rather than the descriptor:
/// [`Rfc3339`] rejects a year outside `0..10_000` and an offset carrying
/// non-zero seconds, both constructible in-process though no RFC 3339 wire
/// payload can express either. The fallback is therefore a genuine last
/// resort, trading a panic for a self-consistent `Display` rendering.
fn rfc3339(instant: OffsetDateTime) -> String {
    instant
        .format(&Rfc3339)
        .unwrap_or_else(|_| instant.to_string())
}

/// How much of a rejected caller value a `detail` echoes back. Wider than
/// anything these surfaces admit (a `gts_type_id` is ~65 characters, the
/// quantity pattern caps at 58), so a legitimate-but-wrong value is echoed
/// whole.
pub(crate) const MAX_ECHOED_VALUE_CHARS: usize = 128;

/// Renders a rejected caller value for a `detail`: bounded, control
/// characters escaped.
///
/// The value a length or control-character check rejects is by definition
/// unbounded and unsanitized, and it reaches both the caller and the log.
/// `escape_debug` is truncated before it is collected, so an oversized input
/// costs the bound rather than its own length.
pub(crate) fn echo_rejected_value(raw: &str) -> String {
    let rendered: String = raw.escape_debug().take(MAX_ECHOED_VALUE_CHARS).collect();
    if raw.escape_debug().nth(MAX_ECHOED_VALUE_CHARS).is_some() {
        format!("{rendered}... ({} characters in all)", raw.chars().count())
    } else {
        rendered
    }
}

/// Renders an [`AggregationDimension`] the way a caller spells it in a
/// `group_by` request, so a `detail` naming the offending dimension echoes
/// something the caller actually wrote rather than a Rust-side debug form.
///
/// Serialization cannot fail for this type, but it falls back rather than
/// asserting, for [`rfc3339`]'s reason: this builds the `detail` of a 400,
/// so a panic here turns a caller-facing rejection into an outage.
fn dimension_wire_name(dimension: &AggregationDimension) -> String {
    match serde_json::to_value(dimension) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => format!("{dimension:?}"),
    }
}

/// Which cursor-bearing parameter a [`UsageCollectorError::CursorRejected`]
/// is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorField {
    /// The `cursor` parameter — a resume point, on the raw path and the feed.
    Cursor,
    /// The `until` parameter — the feed's replay bound.
    Until,
}

impl CursorField {
    /// The wire parameter name this violation is reported on.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Cursor => "cursor",
            Self::Until => "until",
        }
    }
}

/// The reason a [`UsageCollectorError::Conflict`] was raised, and the data
/// specific to that reason.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConflictOutcome {
    /// See [`ConflictReason::IdempotencyConflict`].
    IdempotencyConflict,
    /// See [`ConflictReason::AlreadyInvalidated`].
    AlreadyInvalidated {
        /// The stored invalidation that already withdrew the target.
        invalidated_by: Uuid,
        /// The reason code the stored invalidation carries.
        reason_code: crate::v2::models::ReasonCode,
    },
    /// See [`ConflictReason::TargetNotConverged`].
    TargetNotConverged {
        /// The configured `target_not_converged_retry_after_secs` delay
        /// (DESIGN §3.8) where the host supplied one, `None` otherwise. This
        /// crate carries no config of its own, so the value is never
        /// synthesized. Read by [`UsageCollectorError::retry_after`].
        retry_after_seconds: Option<u64>,
    },
}

impl ConflictOutcome {
    /// The bare wire-facing discriminator this payload is for — what the
    /// host lift projects onto `context.reason`.
    #[must_use]
    pub fn reason(&self) -> ConflictReason {
        match self {
            Self::IdempotencyConflict => ConflictReason::IdempotencyConflict,
            Self::AlreadyInvalidated { .. } => ConflictReason::AlreadyInvalidated,
            Self::TargetNotConverged { .. } => ConflictReason::TargetNotConverged,
        }
    }
}

/// Public error envelope for the Usage Collector SDK and REST surfaces.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UsageCollectorError {
    /// PDP denial on the requested operation (HTTP 403). The host lift emits
    /// `context.reason="AUTHZ"` and no reason text (the SDK never paraphrases
    /// PDP detail).
    ///
    /// `Display` renders no reason: an error written out generically — into a
    /// response body, a `Problem` detail, an `anyhow` chain — must not carry
    /// it. A host that wants it in its operator log reads
    /// [`UsageCollectorError::pdp_detail`] and logs it deliberately.
    #[error("authorization denied")]
    PermissionDenied {
        /// PDP-supplied reason. Operator-log facing, through
        /// [`UsageCollectorError::pdp_detail`]; never rendered by `Display`.
        detail: String,
    },

    /// Request-shape / semantics validation failure (HTTP 400). `field` is
    /// the attributed request field, `reason` the typed
    /// [`ValidationReason`] discriminator, and `detail` the wire
    /// `field_violations[0].description`. `resource_type` identifies the GTS
    /// resource the violation is about (a `gts_type_id`-shaped field
    /// violation attributes to the referenced meter even on the ingestion
    /// surface); `resource_name`, when present, is the offending identifier.
    #[error("invalid argument [{field}/{reason}]: {detail}")]
    InvalidArgument {
        /// GTS resource type — always [`USAGE_RECORD_RESOURCE`]; v2 publishes
        /// one resource.
        resource_type: &'static str,
        /// The offending identifier, when the violation is about a
        /// specific resource; `None` otherwise. Which identifier depends on
        /// what the violation is about: a `gts_id` for a meter-shaped
        /// violation, the target's `UsageRecord.id` for one about an
        /// invalidation's target.
        resource_name: Option<String>,
        /// Attributed request field (`quantity`, `records`, `metadata`, …).
        field: String,
        /// Typed `field_violations[0].reason` discriminator.
        reason: ValidationReason,
        /// Wire `field_violations[0].description`.
        detail: String,
    },

    /// A continuation token refused by the gear, carrying the wire code
    /// `toolkit_odata` owns.
    #[error("cursor rejected [{source}]: {detail}")]
    CursorRejected {
        /// The upstream cursor error — sole source of the wire **reason**
        /// for a malformed token or a changed filter, and not the source of
        /// the wire *field* (see `field` below). An order supplied alongside
        /// a cursor is intercepted upstream and never reaches `source` (see
        /// [`crate::v2::reason::ValidationReason`]); a retention refusal is the
        /// plugin's and lifts to `InvalidArgument` carrying
        /// `ValidationReason::CursorBeyondRetention`.
        source: toolkit_odata::Error,
        /// Gear-authored caller guidance, rendered as the violation
        /// description.
        detail: String,
        /// Which cursor-bearing parameter the violation is reported on.
        field: CursorField,
    },

    /// Referenced resource not found (HTTP 404). `resource_type` is the GTS
    /// type, `name` the raw identifier (a `gts_type_id` or a record UUID),
    /// and `reason` says which lookup failed.
    #[error("not found [{resource_type}]: {detail}")]
    NotFound {
        /// GTS resource type — always [`USAGE_RECORD_RESOURCE`].
        resource_type: &'static str,
        /// Raw identifier whose row was not present.
        name: String,
        /// Which lookup failed. **Not projected onto the wire** — the
        /// platform-shared `toolkit_canonical_errors::NotFoundV1` context has
        /// no reason slot, so a REST client tells the cases apart only by
        /// `detail`. It exists so the gear can classify its own metric labels
        /// without matching a substring of caller-facing prose.
        reason: NotFoundReason,
        /// Wire `detail` message.
        detail: String,
    },

    /// Duplicate-on-create conflict (HTTP 409).
    ///
    /// **Reserved, and produced by nothing in v2.** Duplicate-on-create is
    /// resolved by the deterministic record identity: an identical-payload
    /// resubmission is absorbed and returns the stored row on `Ok`, and a
    /// differing one is [`Self::Conflict`] carrying
    /// [`ConflictOutcome::IdempotencyConflict`]. A consumer handling duplicates
    /// matches that, not this. Kept for [`crate::v2::reason::SEMANTICS_VIOLATION`]'s
    /// reason: the envelope is `#[non_exhaustive]`, so removing a variant is
    /// silent for a downstream matcher.
    #[error("already exists [{resource_type}]: {detail}")]
    AlreadyExists {
        /// GTS resource type — always [`USAGE_RECORD_RESOURCE`].
        resource_type: &'static str,
        /// Raw identifier (`gts_id`) that collided.
        name: String,
        /// Wire `detail` message.
        detail: String,
    },

    /// State / concurrency / referential-integrity conflict (HTTP 409,
    /// AIP-193 `Aborted`). `outcome` carries the typed [`ConflictReason`]
    /// discriminator (via [`ConflictOutcome::reason`], projected onto the
    /// wire `context.reason`) together with the data specific to that
    /// reason; `resource_type` / `name` identify the row involved.
    #[error("conflict [{}]: {detail}", .outcome.reason())]
    Conflict {
        /// GTS resource type — always [`USAGE_RECORD_RESOURCE`].
        resource_type: &'static str,
        /// Raw identifier of the row involved (`gts_id` / record UUID).
        name: String,
        /// The reason this conflict was raised, and the data that reason
        /// alone carries.
        outcome: ConflictOutcome,
        /// Wire `detail` message.
        detail: String,
    },

    /// Transient infrastructure unavailability (HTTP 503). Covers
    /// host-structural readiness (plugin / types-registry), plugin-reported
    /// transience, and PDP-transport outages — operator triage reads the
    /// curated `detail` string. Carries an optional `retry_after_seconds`
    /// hint. The only classification [`Self::is_infrastructure_retryable`]
    /// recognizes; the complete retry decision is [`Self::retry_after`].
    #[error("service unavailable: {detail}")]
    ServiceUnavailable {
        /// Optional retry hint forwarded onto the `Retry-After` slot.
        retry_after_seconds: Option<u64>,
        /// Operator-facing detail (DSN-free, pre-redacted at construction).
        detail: String,
    },

    /// Per-subject ingestion allowance exhausted (HTTP 429). The submission is
    /// rejected whole before any entry is validated, as the structural entry
    /// cap already is. `retry_after_seconds` is the delay after which the
    /// bucket holds enough tokens for a submission of this size — computable
    /// because the entry cap runs first and bounds the cost at
    /// `max_batch_records` (DESIGN §3.2). `detail` names the allowance and the
    /// submitted count, so an operator can tell a burst from a sustained
    /// overrun.
    #[error("ingestion quota exceeded: {detail}")]
    ResourceExhausted {
        /// Seconds until a submission of this size could be admitted.
        /// Forwarded onto the `Retry-After` slot, like
        /// [`Self::ServiceUnavailable`]'s hint.
        retry_after_seconds: u64,
        /// Operator-facing detail naming the allowance and the submitted count.
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

/// Named arguments for [`UsageCollectorError::already_invalidated`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlreadyInvalidatedArgs {
    /// The usage record the invalidation targets.
    pub target: Uuid,
    /// The stored invalidation that already withdrew the target.
    pub invalidated_by: Uuid,
    /// The reason code the stored invalidation carries.
    pub reason_code: crate::v2::models::ReasonCode,
}

/// Named arguments for [`UsageCollectorError::ingestion_quota_exceeded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestionQuotaExceededArgs {
    /// The configured per-subject allowance the submission was rejected
    /// against.
    pub allowance: u64,
    /// The number of entries the rejected submission carried.
    pub submitted: usize,
    /// Seconds until a submission of this size could be admitted.
    pub retry_after_seconds: u64,
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

    /// The PDP-supplied denial reason, or `None` on any other variant.
    ///
    /// The only way to read it: [`Self::PermissionDenied`]'s `Display` renders
    /// no reason, so a host that wants it in its operator log asks for it here
    /// and cannot leak it by writing the error out generically.
    #[must_use]
    pub fn pdp_detail(&self) -> Option<&str> {
        match self {
            Self::PermissionDenied { detail } => Some(detail),
            _ => None,
        }
    }

    // ── InvalidArgument (400) ───────────────────────────────────────────

    /// Batch submission size out of bounds (empty or over the per-call cap).
    #[must_use]
    pub fn invalid_batch_size(actual: usize, min: usize, max: usize) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "records".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!("batch size {actual} out of bounds (expected [{min}, {max}])"),
        }
    }

    /// A quantity outside the published `UsageQuantity` range or precision.
    /// `field` is `quantity`.
    #[must_use]
    pub fn quantity_out_of_range(detail: impl Into<String>) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "quantity".to_owned(),
            reason: ValidationReason::QuantityOutOfRange,
            detail: detail.into(),
        }
    }

    /// Serialized metadata exceeded the per-record size cap.
    #[must_use]
    pub fn metadata_size_exceeded(size: usize, cap: usize) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "metadata".to_owned(),
            reason: ValidationReason::MetadataValidation,
            detail: format!("metadata size {size} bytes exceeds cap {cap} bytes"),
        }
    }

    /// A covered-period bound carried finer than microsecond precision.
    #[must_use]
    pub fn sub_microsecond_period_bound(field: &str, bound: OffsetDateTime) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: field.to_owned(),
            reason: ValidationReason::Validation,
            detail: format!(
                "covered-period bound `{field}` requires at most microsecond \
                 precision (got {}, carrying {} ns); round the bound to the \
                 microsecond before submitting — the entry identity \
                 derivation reads a fixed-width microsecond form, so a finer \
                 value is rejected rather than truncated",
                rfc3339(bound),
                bound.nanosecond(),
            ),
        }
    }

    /// The covered period was inverted (`window_end < window_start`).
    #[must_use]
    pub fn inverted_covered_period(
        window_start: OffsetDateTime,
        window_end: OffsetDateTime,
    ) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: WINDOW_END_FIELD.to_owned(),
            reason: ValidationReason::Validation,
            detail: format!(
                "covered period requires window_start <= window_end (got \
                 window_start={}, window_end={}); equal bounds are a point \
                 event and are valid",
                rfc3339(window_start),
                rfc3339(window_end),
            ),
        }
    }

    /// A covered-period bound falls outside the canonical identity form's
    /// year range, so no entry identity derives from it.
    ///
    /// `field` names which of the two bounds failed, which is why
    /// [`UsageRecordIdInvalid`] splits its two bound arms rather than carrying
    /// one.
    #[must_use]
    pub fn non_canonical_period_bound(field: &'static str, detail: impl Into<String>) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: field.to_owned(),
            reason: ValidationReason::Validation,
            detail: detail.into(),
        }
    }

    /// The covered period ends beyond the ingestion path's future tolerance.
    #[must_use]
    pub fn covered_period_beyond_future_tolerance(
        window_end: OffsetDateTime,
        now: OffsetDateTime,
        tolerance: Duration,
    ) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: WINDOW_END_FIELD.to_owned(),
            reason: ValidationReason::FutureWindow,
            detail: format!(
                "covered period ends at {}, more than {tolerance} after now \
                 ({}); every ingestion path admits only a period ending \
                 within that tolerance of the present",
                rfc3339(window_end),
                rfc3339(now),
            ),
        }
    }

    /// The covered period ends beyond the live path's past tolerance.
    ///
    /// The detail names the backfill route on both surfaces, per
    /// `cpt-cf-usage-collector-adr-backfill-isolation`.
    #[must_use]
    pub fn covered_period_before_past_tolerance(
        window_end: OffsetDateTime,
        now: OffsetDateTime,
        tolerance: Duration,
    ) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: WINDOW_END_FIELD.to_owned(),
            reason: ValidationReason::PastWindow,
            detail: format!(
                "covered period ends at {}, more than {tolerance} before now \
                 ({}); the live path admits only a period ending within that \
                 tolerance. Submit this entry on the backfill route instead \
                 — `POST {BACKFILL_ROUTE_PATH}`, or `backfill_usage_records` \
                 on the SDK trait",
                rfc3339(window_end),
                rfc3339(now),
            ),
        }
    }

    /// A read-path time range was empty or inverted (`to <= from`).
    ///
    /// [`crate::v2::TimeRange`] is mandatory on both range-taking read paths
    /// (the point lookup selects by `id` and takes none) and selects an
    /// entry when `from <= window_end < to`
    /// (`cpt-cf-usage-collector-adr-window-end-selection`), so a range that
    /// is not strictly ordered selects nothing whatever is stored.
    #[must_use]
    pub fn invalid_time_range(from: OffsetDateTime, to: OffsetDateTime) -> Self {
        let from = rfc3339(from);
        let to = rfc3339(to);
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "time_range".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!(
                "time range requires from < to (got from={from}, to={to}); supply a \
                 lower bound strictly before the upper bound. The range arrives as \
                 the `from` / `to` query parameters on the raw path and as \
                 `time_range` in the aggregate request body"
            ),
        }
    }

    /// An aggregated query produced more than `cap`
    /// ([`crate::v2::MAX_AGGREGATION_BUCKETS`]) buckets — a high-cardinality
    /// `group_by` (e.g. a per-record metadata key) over a wide range. The
    /// plugin bounds its own scan to `cap + 1` rows (memory guard); the gateway
    /// rejects the over-cap result as this client-fixable `400`.
    #[must_use]
    pub fn aggregation_result_too_large(cap: usize) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "group_by".to_owned(),
            reason: ValidationReason::AggregationResultTooLarge,
            detail: format!(
                "aggregated query produced more than {cap} groups; narrow the \
                 time range or drop a high-cardinality group_by dimension"
            ),
        }
    }

    /// `MeterTypeId::new` rejected `raw` — malformed, wrong-base, or
    /// multi-segment `gts_type_id`.
    ///
    /// `raw` is echoed through [`echo_rejected_value`]: the length and
    /// control-character branches both call this with a value that has
    /// passed no check at all.
    #[must_use]
    pub fn invalid_meter_type_id(raw: &str, reason: &str) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "gts_type_id".to_owned(),
            reason: ValidationReason::InvalidBaseGtsId,
            detail: format!(
                "gts_type_id `{}` rejected: {reason}",
                echo_rejected_value(raw)
            ),
        }
    }

    /// `AggregationFold::from_str` received a string outside the declared
    /// set. Raised when a resolved declaration names a fold this major
    /// version does not serve.
    #[must_use]
    pub fn invalid_aggregation_fold(raw: &str) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "aggregation_fold".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!(
                "unknown aggregation fold `{}`; expected one of SUM, COUNT, MAX, MIN, LATEST",
                echo_rejected_value(raw)
            ),
        }
    }

    /// Build a record-surface validating-newtype `InvalidArgument` whose wire
    /// `detail` is the newtype's self-describing reason. `field` attributes
    /// the violation (`metadata`, `resource_ref`, …).
    #[must_use]
    fn newtype_validation(field: &str, detail: impl Into<String>) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
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

    /// A metadata value carried a NUL. `field` is `metadata`.
    #[must_use]
    pub fn metadata_value_carries_nul(key: &MetadataKey) -> Self {
        Self::newtype_validation(
            "metadata",
            format!(
                "the value of metadata key '{key}' must not contain NUL: metadata reaches \
                 storage as a `jsonb` document, which cannot carry one in a string"
            ),
        )
    }

    /// `MetadataFilter::new` rejected the input. `field` is `metadata_filter`.
    #[must_use]
    pub fn invalid_metadata_filter(detail: impl Into<String>) -> Self {
        Self::newtype_validation("metadata_filter", detail)
    }

    /// `ResourceRef::new` or `SubjectRef::new` rejected a component. `field`
    /// is the component's dotted path, e.g. `resource_ref.resource_id` — the
    /// same spelling [`Self::attribution_too_long`] stamps, so one component
    /// reports one `field` whichever of its rules fired.
    #[must_use]
    pub fn invalid_attribution_component(field: &str, detail: impl Into<String>) -> Self {
        Self::newtype_validation(field, detail)
    }

    /// `IdempotencyKey::new` rejected the input. `field` is `idempotency_key`.
    #[must_use]
    pub fn invalid_idempotency_key(detail: impl Into<String>) -> Self {
        Self::newtype_validation("idempotency_key", detail)
    }

    /// `ReasonCode::new` rejected the input. `field` is `reason_code`.
    #[must_use]
    pub fn invalid_reason_code(detail: impl Into<String>) -> Self {
        Self::newtype_validation("reason_code", detail)
    }

    /// An entry carried no idempotency key. `field` is `idempotency_key`.
    #[must_use]
    pub fn missing_idempotency_key() -> Self {
        Self::newtype_validation(
            "idempotency_key",
            "idempotency_key is required on every entry; an invalidation repeats its target's",
        )
    }

    /// [`crate::v2::CreateUsageRecord::try_into_usage_record`] was handed a
    /// submission that declares `entry_type: invalidation`.
    ///
    /// **[`Self::Internal`], not [`Self::InvalidArgument`].** Reaching this
    /// means the gateway chose the projection that contradicts the submission
    /// it holds — a host-contract breach, not anything the emitter did, so the
    /// `detail` is addressed to whoever wired the gateway and must not travel
    /// to a REST caller as a 400 against a field they may have sent correctly.
    #[must_use]
    pub fn withdrawal_needs_its_target() -> Self {
        Self::internal(
            "try_into_usage_record was handed a submission declaring entry_type invalidation; a \
             withdrawal is projected with try_into_invalidation_record and its resolved target",
        )
    }

    /// [`crate::v2::CreateUsageRecord::try_into_invalidation_record`] was handed
    /// a submission that declares `entry_type: record`.
    ///
    /// **[`Self::Internal`] on the same grounds as
    /// [`Self::withdrawal_needs_its_target`], and on the same narrowed
    /// precondition.**
    #[must_use]
    pub fn missing_reason_code() -> Self {
        Self::internal(
            "try_into_invalidation_record was handed a submission declaring entry_type record; \
             the gateway reads entry_type before it chooses a projection",
        )
    }

    /// A submission declared `entry_type: invalidation` and stated no reason
    /// code. `field` is `reason_code`.
    ///
    /// **[`Self::InvalidArgument`], not [`Self::Internal`]** — unlike
    /// [`Self::missing_reason_code`], which reports the same field missing.
    /// Both halves of the contradiction are caller-supplied (DESIGN §3.1,
    /// Field ownership), so this is an emitter error whichever projection it
    /// is handed to. The declared kind is taken as what they meant, so the
    /// violation is attributed to the half they have to add.
    #[must_use]
    pub fn reason_code_required_on_invalidation() -> Self {
        Self::newtype_validation(
            "reason_code",
            "reason_code is required when entry_type is invalidation",
        )
    }

    /// A submission declared `entry_type: record` and stated a reason code.
    /// `field` is `reason_code`.
    ///
    /// **[`Self::InvalidArgument`], not [`Self::Internal`]** — unlike
    /// [`Self::withdrawal_needs_its_target`], which fires on the declared
    /// `entry_type` and attributes no field at all. See
    /// [`Self::reason_code_required_on_invalidation`] for the line between
    /// the two.
    #[must_use]
    pub fn reason_code_forbidden_on_record() -> Self {
        Self::newtype_validation(
            "reason_code",
            "reason_code must not appear when entry_type is record; an entry that withdraws \
             another declares entry_type invalidation",
        )
    }

    /// An attribution component exceeded its character cap. `field` is the
    /// component's dotted path, e.g. `resource_ref.resource_id`; see
    /// [`Self::invalid_attribution_component`].
    #[must_use]
    pub fn attribution_too_long(field: &str, max: usize) -> Self {
        Self::newtype_validation(field, format!("{field} must be at most {max} characters"))
    }

    /// Ingestion supplied a metadata key not declared in the referenced
    /// meter's resolved closed `metadata_fields`. Attributed to the record
    /// resource, with `resource_name` carrying the offending `gts_type_id`.
    #[must_use]
    pub fn unknown_metadata_key(gts_type_id: &MeterTypeId, key: &str) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: Some(gts_type_id.as_ref().to_owned()),
            field: "metadata".to_owned(),
            reason: ValidationReason::UnknownMetadataKey,
            detail: format!("unknown metadata key '{key}' for meter {gts_type_id}"),
        }
    }

    /// A `$filter` predicate named a field reserved to a typed parameter
    /// (`gts_type_id`, or the covered-period bounds `window_start` /
    /// `window_end`). Each already travels as a typed parameter alongside
    /// `$filter`, so a predicate naming one in `$filter` would express a
    /// second, possibly contradictory, constraint on something already
    /// fixed — rejected rather than silently honored. Attributed to
    /// `$filter`.
    #[must_use]
    pub fn reserved_filter_field(field: &str) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "$filter".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!(
                "'{field}' is reserved and cannot be named in $filter: it travels \
                 as a typed parameter, not a filterable property"
            ),
        }
    }

    /// A `$filter` predicate named a field the public contract does not
    /// publish — the generated filter schema declares three more than the
    /// published eight (`id` and the two covered-period bounds) for reasons
    /// unrelated to `$filter` admissibility. Distinct from
    /// [`Self::reserved_filter_field`] on purpose: a reserved field travels as
    /// a typed parameter and the caller can move the predicate there, while
    /// this one is simply not on the surface. Attributed to `$filter`.
    #[must_use]
    pub fn unpublished_filter_field(field: &str) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "$filter".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!(
                "'{field}' is not a filterable field: $filter admits one of {:?}",
                crate::v2::PUBLISHED_FILTER_FIELDS,
            ),
        }
    }

    /// A `$filter` predicate compared a closed-label field (`entry_type`,
    /// `origin`) against a literal outside its label set.
    #[must_use]
    pub fn inadmissible_filter_literal(field: &str, value: &str, labels: &[&str]) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "$filter".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!(
                "'{}' is not an admissible {field} label: {field} admits one of {labels:?}",
                echo_rejected_value(value)
            ),
        }
    }

    /// A `metadata_filter` carried more predicates than the published cap of
    /// 16 (`usage-collector-v2.yaml`'s `POST /records/aggregate` operation
    /// description — prose, not `maxItems`). `actual` counts entries in the
    /// caller's slice, which may exceed the number of distinct keys when one
    /// key is named twice: slightly stricter than counting distinct keys, and
    /// the correct bound, since entries are what size the fingerprint and the
    /// SQL. Attributed to `metadata`, the wire field the REST handler used for
    /// this cap. No `gts_type_id`: the cap is identical for every meter.
    #[must_use]
    pub fn too_many_metadata_filters(actual: usize, cap: usize) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "metadata".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!("{actual} metadata filter predicates exceeds cap {cap}"),
        }
    }

    /// One `metadata_filter` predicate carried more values than the
    /// published `maxItems: 32` on `MetadataFilter.values`. Attributed to
    /// `metadata.<key>`, the wire field the REST handler already used for
    /// this cap.
    #[must_use]
    pub fn too_many_metadata_filter_values(key: &str, actual: usize, cap: usize) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: format!("metadata.{key}"),
            reason: ValidationReason::Validation,
            detail: format!("{actual} values on `metadata.{key}` exceeds cap {cap}"),
        }
    }

    /// A caller order mixed sort directions across its keys. The keyset
    /// continuation is a row-value tuple comparison, which only composes
    /// over a single direction, so a mixed-direction order can never
    /// become a usable keyset — it is refused rather than forwarded to a
    /// plugin that would reject it late and unspecifically.
    #[must_use]
    pub fn mixed_direction_order(deviating_field: &str) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "$orderby".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!(
                "order keys must all share one sort direction, but \
                 '{}' sorts against the first key: keyset \
                 pagination is a row-value tuple comparison and cannot \
                 compose a mixed-direction tuple",
                echo_rejected_value(deviating_field)
            ),
        }
    }

    /// A caller order named a key that is not sound to paginate on — a
    /// domain-optional attribute, one derived from an optional attribute, or a
    /// name that is not a record attribute at all. Every caller key leads the
    /// effective keyset, and a row-value tuple whose leading column is NULL
    /// compares as NULL, so NULL-keyed rows would silently drop out of the
    /// page. The classification is [`crate::v2::is_keyset_safe_record_field`], a
    /// fail-closed allowlist — hence the same rejection for an unknown name.
    #[must_use]
    pub fn inadmissible_order_key(field: &str) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "$orderby".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!(
                "order key '{}' is not supported: keyset pagination needs a \
                 record attribute present on every entry in its own right, so an \
                 optional or derived attribute, or an unrecognised name, is \
                 refused; order by one of {:?}",
                echo_rejected_value(field),
                crate::v2::KEYSET_SAFE_RECORD_FIELDS,
            ),
        }
    }

    /// A caller order named one key more than once.
    ///
    /// A repeated key would ride all the way to a minted keyset
    /// carrying one boundary value, and one signed token, per repetition —
    /// `$orderby=resource_id,resource_id,resource_id` alone exceeds
    /// `crate::v2::keyset::MAX_KEYSET_BYTES` before the canonical tiebreaker is
    /// appended, breaching the cursor's published `maxLength: 4096`.
    #[must_use]
    pub fn duplicate_order_key(field: &str) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: None,
            field: "$orderby".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!(
                "order key '{}' is named more than once: a repeated key widens the \
                 keyset the server would have to mint without narrowing the sort, and risks \
                 the cursor's published 4096-character bound; name each key at most once",
                echo_rejected_value(field)
            ),
        }
    }

    /// A continuation token's bound order is not a usable keyset.
    ///
    /// The wire code is not chosen here: this carries `toolkit_odata`'s own
    /// `InvalidCursor` and the host lift converts it (Spec §3.13). The wire
    /// field is [`CursorField::Cursor`], chosen here, upstream having no
    /// notion of which cursor-bearing parameter is meant.
    #[must_use]
    pub fn inadmissible_cursor_keyset(defect: impl Into<String>) -> Self {
        let defect = defect.into();
        Self::CursorRejected {
            source: toolkit_odata::Error::InvalidCursor,
            detail: format!(
                "the cursor's bound order is not a usable keyset ({defect}); \
                 restart pagination without a cursor"
            ),
            field: CursorField::Cursor,
        }
    }

    /// A continuation token names a direction this read path does not serve.
    ///
    /// Same wire code and field as [`Self::inadmissible_cursor_keyset`], for
    /// the reasons that constructor gives.
    #[must_use]
    pub fn unsupported_cursor_direction(defect: impl Into<String>) -> Self {
        let defect = defect.into();
        Self::CursorRejected {
            source: toolkit_odata::Error::InvalidCursor,
            detail: format!(
                "the cursor's direction is not supported ({defect}); restart pagination \
                 without a cursor"
            ),
            field: CursorField::Cursor,
        }
    }

    /// A feed `cursor` this gear will not continue.
    ///
    /// Same wire code and field as the raw path's, for the reasons
    /// [`Self::inadmissible_cursor_keyset`] gives.
    #[must_use]
    pub fn inadmissible_feed_cursor(defect: impl Into<String>) -> Self {
        let defect = defect.into();
        Self::CursorRejected {
            source: toolkit_odata::Error::InvalidCursor,
            detail: format!(
                "the `cursor` is not a usable feed cursor ({defect}); \
                 resend the request without `cursor` to restart the subscription from its \
                 oldest retained position, or with one this feed issued"
            ),
            field: CursorField::Cursor,
        }
    }

    /// As [`Self::inadmissible_cursor_keyset`], for the feed's `until` bound.
    #[must_use]
    pub fn inadmissible_until_keyset(defect: impl Into<String>) -> Self {
        let defect = defect.into();
        Self::CursorRejected {
            source: toolkit_odata::Error::InvalidCursor,
            detail: format!(
                "the `until` bound is not a usable feed cursor ({defect}); \
                 resend the request without `until`, or with one this feed issued"
            ),
            field: CursorField::Until,
        }
    }

    /// A continuation token was minted over a different query than the
    /// request carrying it.
    ///
    /// This carries `toolkit_odata`'s own `FilterMismatch` and the gear spells
    /// the wire field itself (Spec §3.13).
    #[must_use]
    pub fn cursor_query_mismatch() -> Self {
        Self::CursorRejected {
            source: toolkit_odata::Error::FilterMismatch,
            detail: "the cursor was minted over a different query: continue a page by \
                     resending the same request, cursor apart, or restart pagination \
                     without a cursor. The cursor binds `gts_type_id`, the `from` / \
                     `to` range, `$filter` and every `metadata.<key>` filter, so \
                     changing any of them invalidates it"
                .to_owned(),
            field: CursorField::Cursor,
        }
    }

    /// A feed `cursor` minted over a different subscription than the
    /// request carrying it.
    ///
    /// Same `FilterMismatch` source as the raw path's, and for the same
    /// reason (Spec §3.13).
    #[must_use]
    pub fn cursor_subscription_mismatch() -> Self {
        Self::CursorRejected {
            source: toolkit_odata::Error::FilterMismatch,
            detail: "the cursor was minted over a different subscription: resend it with the \
                     same `gts_type_id` set the cursor was issued under, or restart the \
                     subscription without a cursor"
                .to_owned(),
            field: CursorField::Cursor,
        }
    }

    /// As [`Self::cursor_query_mismatch`], for the feed's `until` bound.
    #[must_use]
    pub fn until_query_mismatch() -> Self {
        Self::CursorRejected {
            source: toolkit_odata::Error::FilterMismatch,
            detail: "the `until` bound was minted over a different subscription: resend it \
                     with the same `gts_type_id` set the bound was issued under, or drop \
                     `until` to read the live feed"
                .to_owned(),
            field: CursorField::Until,
        }
    }

    /// A `group_by` dimension named a metadata key the queried meter's
    /// resolved declaration does not declare.
    #[must_use]
    pub fn undeclared_metadata_dimension(gts_type_id: &MeterTypeId, key: &str) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: Some(gts_type_id.as_ref().to_owned()),
            field: "group_by".to_owned(),
            reason: ValidationReason::UnknownMetadataKey,
            detail: format!(
                "unknown metadata key '{key}' in group_by for meter {gts_type_id}: not declared"
            ),
        }
    }

    /// A `group_by` list naming one dimension more than once.
    ///
    /// Deliberately not
    /// [`ValidationReason::MetadataFieldDuplicate`]: that variant's wire
    /// string names the meter type's `metadata_fields` *declaration*, a
    /// different surface, and would be wrong for a repeated fixed dimension
    /// such as `tenant_id`. No new published reason code is minted either,
    /// which would widen the wire contract; the specifics live in `detail`.
    #[must_use]
    pub fn repeated_grouping_dimension(
        gts_type_id: &MeterTypeId,
        dimension: &AggregationDimension,
    ) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: Some(gts_type_id.as_ref().to_owned()),
            field: "group_by".to_owned(),
            reason: ValidationReason::Validation,
            detail: format!(
                "grouping dimension `{}` appears more than once in group_by for meter \
                 {gts_type_id}: each admissible dimension may be named at most once, in any \
                 combination and any order",
                dimension_wire_name(dimension),
            ),
        }
    }

    /// An invalidation departed from its target in a field it must copy.
    #[must_use]
    pub fn invalidation_field_mismatch(field: &str, target: Uuid) -> Self {
        Self::InvalidArgument {
            resource_type: USAGE_RECORD_RESOURCE,
            resource_name: Some(target.to_string()),
            field: field.to_owned(),
            reason: ValidationReason::InvalidationFieldMismatch,
            detail: format!(
                "`{field}` differs from usage record {target}; an invalidation copies every \
                 caller-supplied field of the entry it withdraws"
            ),
        }
    }

    // ── NotFound (404) ──────────────────────────────────────────────────

    /// A lookup referenced a `UsageRecord.id` that does not exist.
    #[must_use]
    pub fn usage_record_not_found(id: Uuid) -> Self {
        Self::NotFound {
            resource_type: USAGE_RECORD_RESOURCE,
            name: id.to_string(),
            reason: NotFoundReason::UsageRecordNotFound,
            detail: format!("usage record not found: {id}"),
        }
    }

    /// The target an invalidation locates through its own fields resolved to
    /// nothing.
    ///
    /// `NotFound` rather than a conflict: the ledger holds no entry under the
    /// derived identifier. `name` carries that identifier, and
    /// [`NotFoundReason::InvalidationTargetNotFound`] separates this
    /// in-process from the other `NotFound` a submission can raise — a wire
    /// client still has only `detail` to tell the cases apart.
    #[must_use]
    pub fn invalidation_target_not_found(target: Uuid) -> Self {
        Self::NotFound {
            resource_type: USAGE_RECORD_RESOURCE,
            name: target.to_string(),
            reason: NotFoundReason::InvalidationTargetNotFound,
            detail: format!(
                "no usage record matches this invalidation's target, derived as {target}: \
                 the target is identified by tenant, GTS type, idempotency key, and covered \
                 period, all read from this entry's own fields, so a typo in any of the four \
                 surfaces here rather than as a field mismatch"
            ),
        }
    }

    // ── Conflict / Aborted (409) ────────────────────────────────────────

    /// A second invalidation of a record under a reason code other than the
    /// stored one: the dedup conflict of an invalidation. `name` is
    /// the target; `invalidated_by` and `reason_code` name the invalidation in
    /// place.
    #[must_use]
    pub fn already_invalidated(args: AlreadyInvalidatedArgs) -> Self {
        let AlreadyInvalidatedArgs {
            target,
            invalidated_by,
            reason_code,
        } = args;
        Self::Conflict {
            resource_type: USAGE_RECORD_RESOURCE,
            name: target.to_string(),
            detail: format!(
                "usage record {target} is already invalidated by {invalidated_by} with reason code {}",
                reason_code.as_str()
            ),
            outcome: ConflictOutcome::AlreadyInvalidated {
                invalidated_by,
                reason_code,
            },
        }
    }

    /// Same `idempotency_key`, canonical-field-different payload. `name`
    /// carries the previously persisted record id so the caller can
    /// reconcile.
    #[must_use]
    pub fn idempotency_conflict(idempotency_key: &IdempotencyKey, existing_id: Uuid) -> Self {
        Self::Conflict {
            resource_type: USAGE_RECORD_RESOURCE,
            name: existing_id.to_string(),
            detail: format!(
                "idempotency key {} already bound to record {existing_id}",
                idempotency_key.as_str()
            ),
            outcome: ConflictOutcome::IdempotencyConflict,
        }
    }

    /// An invalidation's target has not converged under the active plugin's
    /// dedup level. `name` is the target. Retryable through the wire
    /// `context.retryable = true` **and** through [`Self::retry_after`]
    /// (DESIGN §3.3), though [`Self::is_infrastructure_retryable`] stays true
    /// for `ServiceUnavailable` alone — this is a domain race, not an
    /// outage. `retry_after_seconds` is `None` unless the
    /// host supplies the configured `target_not_converged_retry_after_secs`
    /// delay (DESIGN §3.8), which its error lift does.
    #[must_use]
    pub fn target_not_converged(target: Uuid, retry_after_seconds: Option<u64>) -> Self {
        Self::Conflict {
            resource_type: USAGE_RECORD_RESOURCE,
            name: target.to_string(),
            detail: format!(
                "this invalidation's target, derived as {target}, names an entry whose \
                 dedup identity has not converged yet; retry"
            ),
            outcome: ConflictOutcome::TargetNotConverged {
                retry_after_seconds,
            },
        }
    }

    // ── ServiceUnavailable (503) ────────────────────────────────────────

    /// No scoped storage-plugin client was available (host-structural
    /// readiness) — a dispatch that reached no plugin at all (DESIGN §3.8).
    /// `retry_after_seconds` is the configured `unavailable_retry_after_secs`
    /// delay where the host supplies one; this crate carries no config of
    /// its own, so a caller building one directly gets `None`.
    #[must_use]
    pub fn plugin_unavailable(retry_after_seconds: Option<u64>) -> Self {
        Self::service_unavailable("storage plugin unavailable", retry_after_seconds)
    }

    /// The `types-registry` lookup the host uses to bind the scoped client
    /// returned an unavailable result. `retry_after_seconds` is the same
    /// configured default [`Self::plugin_unavailable`] takes.
    #[must_use]
    pub fn types_registry_unavailable(retry_after_seconds: Option<u64>) -> Self {
        Self::service_unavailable("types-registry unavailable", retry_after_seconds)
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

    // ── ResourceExhausted (429) ─────────────────────────────────────────

    /// Per-subject ingestion allowance exhausted.
    #[must_use]
    pub fn ingestion_quota_exceeded(args: IngestionQuotaExceededArgs) -> Self {
        let IngestionQuotaExceededArgs {
            allowance,
            submitted,
            retry_after_seconds,
        } = args;
        Self::ResourceExhausted {
            retry_after_seconds,
            detail: format!("submitted {submitted} entries against an allowance of {allowance}"),
        }
    }

    /// Unclassified failure. `detail` MUST be DSN-free / pre-redacted.
    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal {
            detail: detail.into(),
        }
    }

    /// `true` for [`Self::ServiceUnavailable`] alone — plugin `Transient`,
    /// host readiness, and PDP-transport failures all lift there.
    ///
    /// **Not the complete retry decision, and named so it cannot be mistaken
    /// for it** (DESIGN §3.3): [`Self::ResourceExhausted`] and
    /// [`Self::Conflict`] carrying
    /// [`crate::v2::reason::ConflictReason::TargetNotConverged`] are
    /// retryable too, without being infrastructure failures.
    /// [`Self::retry_after`] covers all three and is what a retry-aware
    /// caller dispatches on; this answers the narrower question of whether
    /// the infrastructure failed, which is what separates backpressure and a
    /// domain race from an outage.
    #[must_use]
    pub fn is_infrastructure_retryable(&self) -> bool {
        matches!(self, Self::ServiceUnavailable { .. })
    }

    /// The delay before retrying is worthwhile, for every outcome DESIGN
    /// §3.3 Error Contract names retryable — `ServiceUnavailable`,
    /// `ResourceExhausted`, and `Conflict(TargetNotConverged)` — and `None`
    /// for every other classification.
    ///
    /// `ServiceUnavailable` and `Conflict(TargetNotConverged)` answer `None`
    /// from a bare, config-free construction: this crate carries no
    /// configuration of its own and cannot synthesize a delay it was never
    /// given.
    ///
    /// **A floor, not a schedule.** Every caller that took the same outage
    /// is handed the same server-supplied delay, so retrying at exactly this
    /// value retries in lockstep — the storm the delay exists to avoid. A
    /// caller owes randomized jitter on top of it and a bounded attempt
    /// count, on all three retryable classifications. `None` does not mean
    /// "retry at once": it means this error carried no hint, and the caller
    /// backs off on its own schedule.
    ///
    /// No wildcard arm: a variant added later that carries its own
    /// `retry_after_seconds` must be classified here, not inherit `None`.
    #[must_use]
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            Self::ServiceUnavailable {
                retry_after_seconds: Some(seconds),
                ..
            }
            | Self::ResourceExhausted {
                retry_after_seconds: seconds,
                ..
            }
            | Self::Conflict {
                outcome:
                    ConflictOutcome::TargetNotConverged {
                        retry_after_seconds: Some(seconds),
                    },
                ..
            } => Some(std::time::Duration::from_secs(*seconds)),
            // Retryable, but with no hint to report (see above).
            Self::ServiceUnavailable {
                retry_after_seconds: None,
                ..
            }
            | Self::Conflict {
                outcome:
                    ConflictOutcome::TargetNotConverged {
                        retry_after_seconds: None,
                    },
                ..
            }
            // Not retryable at all.
            | Self::Conflict {
                outcome:
                    ConflictOutcome::IdempotencyConflict
                    | ConflictOutcome::AlreadyInvalidated { .. },
                ..
            }
            | Self::PermissionDenied { .. }
            | Self::InvalidArgument { .. }
            | Self::CursorRejected { .. }
            | Self::NotFound { .. }
            | Self::AlreadyExists { .. }
            | Self::Internal { .. } => None,
        }
    }
}

/// Plugin-side error vocabulary returned by every
/// [`crate::v2::plugin_api::UsageCollectorPluginV2`] method.
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
/// - The record variants below — typed domain outcomes.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UsageCollectorPluginError {
    /// Retryable backend failure — safe to retry (downstream timeout,
    /// connection reset, upstream 5xx). Lifts to
    /// [`UsageCollectorError::ServiceUnavailable`] at the dispatch
    /// boundary, forwarding the optional `retry_after_seconds` hint, and is
    /// observed as retryable by
    /// [`UsageCollectorError::is_infrastructure_retryable`].
    #[error("transient plugin error: {detail}")]
    Transient {
        /// Operator-facing detail (DSN-free, pre-redacted at the plugin).
        detail: String,
        /// Optional retry-after hint (seconds) forwarded onto the
        /// `ServiceUnavailable` envelope's `Retry-After` slot. Plugins
        /// that have no actionable hint pass `None`.
        retry_after_seconds: Option<u64>,
    },

    /// Idempotency conflict at the persistence boundary: an entry with this
    /// dedup identity is already stored, and its caller-supplied fields differ
    /// from the submission's ([`crate::v2::models::UsageRecord::caller_supplied_eq`]).
    /// `existing` is the stored entry, so the gateway can name it, and when the
    /// dispatched entry is an invalidation, report the conflict as
    /// `AlreadyInvalidated` naming `existing.id` and its reason code.
    #[error("idempotency conflict: key {} already bound to record {}", .idempotency_key.as_str(), .existing.id)]
    IdempotencyConflict {
        /// The dispatched entry's idempotency key. Caller-supplied on both
        /// entry kinds: an invalidation repeats its target's.
        ///
        /// Carried as the validated newtype rather than a `String`: the entry
        /// it is read off holds one, `existing` holds one, and dropping to a
        /// bare string at the reporting boundary alone is what made every call
        /// site spell `.as_str()`.
        idempotency_key: IdempotencyKey,
        /// The stored entry the key is already bound to.
        existing: Box<crate::v2::stored::StoredUsageRecord>,
    },

    /// A lookup referenced a `UsageRecord.id` the store does not hold —
    /// `get_usage_record`, or the target of a submitted invalidation.
    #[error("usage record not found: {id}")]
    UsageRecordNotFound {
        /// The `UsageRecord.id` the lookup asked for. Caller-supplied on
        /// `get_usage_record`; on an invalidation's target lookup it is
        /// derived from the withdrawal's own identity inputs with
        /// `entry_type = record`, never sent
        /// (`cpt-cf-usage-collector-adr-record-identity-derivation`).
        id: Uuid,
    },

    /// A converged-only lookup cannot yet decide whether `id` exists: its
    /// identity has not converged under the plugin's dedup level. Only
    /// `get_usage_record(.., converged_only = true)` may answer it. The
    /// gateway lifts it to a retryable `Conflict(TargetNotConverged)`.
    #[error("usage record not converged: {id}")]
    UsageRecordNotConverged {
        /// The `UsageRecord.id` the lookup named.
        id: Uuid,
    },

    /// A cursor names a position after which retention has already removed an
    /// entry of a subscribed GTS type, so the continuation cannot be served
    /// whole.
    ///
    /// The plugin decides this from what it still holds, not from the cursor's
    /// own age — a sweep clamps that age to the retention boundary, so an age
    /// test would serve a silently truncated range. `DESIGN.md` §3.3 lifts it
    /// to [`UsageCollectorError::InvalidArgument`] carrying
    /// [`crate::v2::reason::ValidationReason::CursorBeyondRetention`] against the
    /// `cursor` field.
    #[error("the cursor names a position retention has already passed")]
    CursorBeyondRetention,

    /// Non-retryable unclassified plugin-side failure (plugin invariant
    /// broken, uncategorized backend error). Use [`Self::Transient`] for
    /// retryable backend errors.
    #[error("plugin internal error: {0}")]
    Internal(String),
}

/// A derivation refusal is caller-supplied data that did not pass, so it
/// reaches the caller as [`UsageCollectorError::InvalidArgument`] naming the
/// field rather than as an internal fault. It is also what lets a call site
/// dispatch to [`crate::v2::derive_usage_record_id`] with no period guard of
/// its own.
impl From<UsageRecordIdInvalid> for UsageCollectorError {
    fn from(invalid: UsageRecordIdInvalid) -> Self {
        match invalid {
            UsageRecordIdInvalid::PeriodInverted {
                window_start,
                window_end,
            } => Self::inverted_covered_period(window_start, window_end),
            UsageRecordIdInvalid::WindowStart(ref bound) => {
                Self::non_canonical_period_bound(WINDOW_START_FIELD, bound.to_string())
            }
            UsageRecordIdInvalid::WindowEnd(ref bound) => {
                Self::non_canonical_period_bound(WINDOW_END_FIELD, bound.to_string())
            }
        }
    }
}

impl UsageCollectorPluginError {
    /// Constructs a [`UsageCollectorPluginError::IdempotencyConflict`] against
    /// the stored entry.
    #[must_use]
    pub fn idempotency_conflict(
        idempotency_key: IdempotencyKey,
        existing: crate::v2::stored::StoredUsageRecord,
    ) -> Self {
        Self::IdempotencyConflict {
            idempotency_key,
            existing: Box::new(existing),
        }
    }

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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
