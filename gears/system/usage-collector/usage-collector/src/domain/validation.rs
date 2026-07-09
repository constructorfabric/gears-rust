//! Pure shape-validation algorithms for the catalog and ingest boundaries.
//!
//! - [`metadata_fields_from_wire`] converts the permissive wire shape
//!   (`Vec<String>`) supplied by the REST DTO into the SDK's
//!   [`BTreeSet<MetadataKey>`], surfacing duplicate / empty-string
//!   violations as the canonical
//!   `/metadata_fields/{i}: invalid_metadata_fields_*` Problem envelope.
//!   The SDK type's invariants (no empty key, no NUL bytes, set semantics)
//!   then make malformed declarations impossible past this conversion.
//! - [`validate_submit_record_metadata`] enforces the ingest-time closed
//!   shape membership and the configurable size cap against an already-typed
//!   `BTreeMap<MetadataKey, String>`.
//!
//! `gts_id` is NOT re-validated here: [`usage_collector_sdk::UsageTypeGtsId`]
//! is a validating newtype that already rejects empty values, ids missing
//! the reserved-prefix `~` segment, and ids whose prefix is not one of the
//! reserved counter / gauge base type ids.
//!
//! `metadata_fields = []` is accepted: DESIGN section 3.7 (table
//! `usage_type_catalog`) describes `metadata_fields` as `text[]` of "unique
//! non-empty strings" but does NOT require at least one entry — a usage
//! type may accept no caller-supplied metadata keys, only the mandatory
//! attribution composites.

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::Decimal;
use toolkit_macros::domain_model;
use usage_collector_sdk::{
    MetadataKey, UsageCollectorError, UsageRecord, UsageRecordStatus, UsageType,
};
use uuid::Uuid;

/// Convert the wire-permissive `Vec<String>` into the SDK's
/// [`BTreeSet<MetadataKey>`] per
/// `cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation`.
///
/// The SDK newtype [`MetadataKey`] enforces non-empty / no-NUL keys at
/// construction; the set guarantees no duplicates. This function surfaces
/// per-entry rejections as typed SDK variants
/// ([`UsageCollectorError::InvalidArgument`] /
/// [`UsageCollectorError::InvalidArgument`]), each carrying the
/// offending zero-based `index`.
///
/// Duplicate-`gts_id` detection is owned by the plugin's `UNIQUE(gts_id)`
/// constraint (ADR-0012), surfaced as
/// [`UsageCollectorError::AlreadyExists`].
///
/// # Errors
///
/// * [`UsageCollectorError::InvalidArgument`] when an entry is an
///   empty string or fails [`MetadataKey::new`] (e.g. contains a NUL byte).
/// * [`UsageCollectorError::InvalidArgument`] when an entry
///   duplicates an earlier one.
// @cpt-algo:cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation:p1
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-fr-data-classification:p1
pub fn metadata_fields_from_wire(
    fields: Vec<String>,
) -> Result<BTreeSet<MetadataKey>, UsageCollectorError> {
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation:p1:inst-algo-shape-invalid-metadata-fields
    let mut set: BTreeSet<MetadataKey> = BTreeSet::new();
    for (index, field) in fields.into_iter().enumerate() {
        if field.is_empty() {
            return Err(UsageCollectorError::invalid_metadata_field(index, true));
        }
        let key = MetadataKey::new(field)
            .map_err(|_err| UsageCollectorError::invalid_metadata_field(index, false))?;
        if !set.insert(key) {
            return Err(UsageCollectorError::duplicate_metadata_field(index));
        }
    }
    // @cpt-end:cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation:p1:inst-algo-shape-invalid-metadata-fields

    // @cpt-begin:cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation:p1:inst-algo-shape-return-valid
    Ok(set)
    // @cpt-end:cpt-cf-usage-collector-algo-usage-type-lifecycle-usage-type-shape-validation:p1:inst-algo-shape-return-valid
}

/// Per-record metadata payload size cap (8 KiB) enforced on
/// `create_usage_record` before plugin dispatch.
// @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1:inst-algo-metadata-read-cap
const RECORD_METADATA_SIZE_CAP_BYTES: usize = 8 * 1024;
// @cpt-end:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1:inst-algo-metadata-read-cap

/// Validates the `metadata` payload of a `UsageRecord` submission against the
/// referenced `UsageType` per the usage-emission ingestion contract.
///
/// Runs two checks: a closed-shape key check (every key MUST be a member of
/// `UsageType.metadata_fields`, surfaced as
/// [`UsageCollectorError::InvalidArgument`]) and a size cap (serialized
/// metadata ≤ [`RECORD_METADATA_SIZE_CAP_BYTES`], surfaced as
/// [`UsageCollectorError::InvalidArgument`]).
///
/// The "metadata must be a JSON object" and "value must be a string" branches
/// no longer exist here: the SDK / REST DTO now carries `metadata` as a
/// typed `BTreeMap<MetadataKey|String, String>`, so structural / value-shape
/// rejections happen at the deserialize boundary and never reach this
/// function.
// @cpt-algo:cpt-cf-usage-collector-algo-usage-type-lifecycle-ingest-metadata-validation:p1
// @cpt-algo:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-record-metadata:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-entity-record-metadata:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-constraint-no-business-logic:p1
#[allow(clippy::missing_errors_doc)]
pub fn validate_submit_record_metadata(
    usage_type: &UsageType,
    metadata: &BTreeMap<MetadataKey, String>,
) -> Result<(), UsageCollectorError> {
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-type-lifecycle-ingest-metadata-validation:p1:inst-algo-ingest-validate-closed-shape
    for key in metadata.keys() {
        if !usage_type.metadata_fields.contains(key) {
            // @cpt-begin:cpt-cf-usage-collector-algo-usage-type-lifecycle-ingest-metadata-validation:p1:inst-algo-ingest-validate-reject
            return Err(UsageCollectorError::unknown_metadata_key(
                &usage_type.gts_id,
                key.as_str(),
            ));
            // @cpt-end:cpt-cf-usage-collector-algo-usage-type-lifecycle-ingest-metadata-validation:p1:inst-algo-ingest-validate-reject
        }
    }
    // @cpt-end:cpt-cf-usage-collector-algo-usage-type-lifecycle-ingest-metadata-validation:p1:inst-algo-ingest-validate-closed-shape

    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1:inst-algo-metadata-read-input
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1:inst-algo-metadata-serialize
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1:inst-algo-metadata-measure
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1:inst-algo-metadata-exceeds
    // Serialized JSON byte size. The plugin re-serializes the same `metadata`
    // map at persistence time, so this buffer is dropped immediately. The
    // `BTreeMap<MetadataKey, String>` shape (string keys, string values)
    // makes serialization infallible; the defensive arm maps the impossible
    // failure to `Internal` rather than panicking.
    let size = serde_json::to_vec(metadata)
        .map(|bytes| bytes.len())
        .map_err(|err| {
            UsageCollectorError::internal(format!("metadata size measurement failed: {err}"))
        })?;
    if size > RECORD_METADATA_SIZE_CAP_BYTES {
        return Err(UsageCollectorError::metadata_size_exceeded(
            size,
            RECORD_METADATA_SIZE_CAP_BYTES,
        ));
    }
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1:inst-algo-metadata-exceeds
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1:inst-algo-metadata-measure
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1:inst-algo-metadata-serialize
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1:inst-algo-metadata-read-input

    // @cpt-begin:cpt-cf-usage-collector-algo-usage-type-lifecycle-ingest-metadata-validation:p1:inst-algo-ingest-validate-return-valid
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1:inst-algo-metadata-valid
    Ok(())
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement:p1:inst-algo-metadata-valid
    // @cpt-end:cpt-cf-usage-collector-algo-usage-type-lifecycle-ingest-metadata-validation:p1:inst-algo-ingest-validate-return-valid
}

/// Outcome of the synchronous four-cell value-matrix check inside
/// `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`.
///
/// The semantics-enforcement algorithm is split into two halves so the host
/// service can sequence them around the L1 `corrects_id` SPI lookup:
///
/// * [`validate_record_semantics`] is sync and runs the (`MetricSemantics` ×
///   `corrects_id` presence) value-sign matrix. When the submitted record
///   carries a `corrects_id` against a counter usage type it returns
///   [`SemanticsOutcome::NeedsL1Lookup`] so the caller dispatches the SPI
///   single-row read and runs [`verify_l1_corrects_id`] against the result.
/// * [`verify_l1_corrects_id`] is sync and runs the four L1 referential
///   checks against the referenced row returned by the SPI.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticsOutcome {
    /// Value-matrix accepted the submission AND no L1 lookup is needed
    /// (ordinary counter usage row or ordinary gauge submission).
    Valid,
    /// Value-matrix accepted the counter compensation submission; the
    /// caller MUST dispatch `get_usage_record(corrects_id)` and run
    /// [`verify_l1_corrects_id`] before persisting.
    NeedsL1Lookup {
        /// `corrects_id` to look up via the storage Plugin SPI.
        corrects_id: Uuid,
    },
}

/// Run the synchronous half of
/// `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`:
/// the four-cell `(MetricSemantics × corrects_id presence)` value-sign matrix.
///
/// Returns [`SemanticsOutcome::Valid`] for ordinary counter / gauge
/// submissions; [`SemanticsOutcome::NeedsL1Lookup`] for a counter
/// compensation whose value-sign passes (caller MUST follow up with an SPI
/// `get_usage_record` + [`verify_l1_corrects_id`]).
///
/// # Errors
///
/// * [`UsageCollectorError::InvalidArgument`] for an ordinary counter
///   record with `value < 0`.
/// * [`UsageCollectorError::InvalidArgument`] for a counter
///   compensation with `value >= 0` (zero is not accepted).
/// * [`UsageCollectorError::InvalidArgument`] (wire
///   `GAUGE_COMPENSATION_REJECTED`) when a gauge usage type is submitted
///   with `corrects_id` set.
/// * [`UsageCollectorError::Internal`] for the defensive unreachable
///   `kind` arm (the [`crate::models::UsageTypeGtsId`] newtype constrains
///   the prefix to exactly counter / gauge — hitting this means the
///   catalog read was inconsistent).
// @cpt-algo:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-counter-semantics:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-fr-gauge-semantics:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-value-matrix:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-compensation-no-business-logic:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-principle-semantics-enforcement:p1
pub fn validate_record_semantics(
    usage_type: &UsageType,
    record: &UsageRecord,
) -> Result<SemanticsOutcome, UsageCollectorError> {
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-read-input-v2
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-read-type-v2
    let value_signum = value_signum(record.value);
    let corrects_id = record.corrects_id;
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-read-type-v2
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-read-input-v2

    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-branch-v2
    if usage_type.is_counter() {
        match corrects_id {
            // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-usage
            None => {
                // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-usage-negative
                if value_signum < 0 {
                    return Err(UsageCollectorError::negative_counter_value(record.value));
                }
                // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-usage-negative
                // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-usage-valid
                Ok(SemanticsOutcome::Valid)
                // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-usage-valid
            }
            // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-usage
            // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-compensation
            Some(corrects_id) => {
                // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-compensation-non-negative
                if value_signum >= 0 {
                    return Err(UsageCollectorError::non_negative_counter_compensation(
                        record.value,
                    ));
                }
                // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-compensation-non-negative
                Ok(SemanticsOutcome::NeedsL1Lookup { corrects_id })
            } // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-compensation
        }
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-branch-v2
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-gauge-branch-v2
    } else if usage_type.is_gauge() {
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-gauge-compensation-rejected
        if corrects_id.is_some() {
            return Err(UsageCollectorError::gauge_compensation_rejected(
                &usage_type.gts_id,
            ));
        }
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-gauge-compensation-rejected
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-gauge-accept-v2
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-gauge-valid-v2
        Ok(SemanticsOutcome::Valid)
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-gauge-valid-v2
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-gauge-accept-v2
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-gauge-branch-v2
    } else {
        // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-unsupported-v2
        // Defensive — the `UsageTypeGtsId` newtype constrains the prefix to
        // exactly counter / gauge, so this arm is unreachable when the
        // catalog read is consistent. Hitting it means catalog corruption,
        // not a caller error, so it lifts to `Internal` (HTTP 500).
        Err(UsageCollectorError::internal(format!(
            "unsupported usage type semantics for {gts_id}",
            gts_id = usage_type.gts_id,
        )))
        // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-unsupported-v2
    }
}

/// Run the L1 `corrects_id` referential checks of
/// `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`
/// against the row the storage Plugin SPI returned for the caller-supplied
/// `corrects_id`.
///
/// `record` is the incoming compensation submission; `referenced` is the
/// row returned by `get_usage_record(corrects_id)`. The plugin's
/// `UsageRecordNotFound` is re-classified to
/// [`UsageCollectorError::NotFound`] by the caller before this
/// helper runs, so the helper only sees an existing row.
///
/// # Errors
///
/// * [`UsageCollectorError::Conflict`] when the
///   referenced row is itself a compensation.
/// * [`UsageCollectorError::Conflict`] when the referenced row
///   does not share the full identity tuple
///   `(tenant_id, gts_id, resource_ref, subject_ref)` with the incoming
///   compensation. `subject_ref` presence is part of the identity — a
///   `None` vs `Some(_)` mismatch is a scope error.
/// * [`UsageCollectorError::Conflict`] when the referenced row is
///   not [`UsageRecordStatus::Active`] (including a row concurrently being
///   deactivated — the same active-status check serialises against the
///   cascade).
// (algo scope marker `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`
// is declared on `validate_record_semantics` above — this function continues the
// same algorithm; declaring it here would be a duplicate scope marker.)
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-corrects-id-l1:p1
// @cpt-dod:cpt-cf-usage-collector-dod-usage-emission-compensation-concurrency:p1
//
// `corrects_id` is taken as a typed `Uuid` so the `record.corrects_id == Some(_)`
// precondition is encoded at the type level: callers cannot accidentally invoke
// the helper for a non-compensation row.
pub fn verify_l1_corrects_id(
    record: &UsageRecord,
    corrects_id: Uuid,
    referenced: &UsageRecord,
) -> Result<(), UsageCollectorError> {
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-targets-compensation
    if referenced.corrects_id.is_some() {
        return Err(UsageCollectorError::corrects_id_targets_compensation(
            corrects_id,
        ));
    }
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-targets-compensation

    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-cross-scope
    if referenced.tenant_id != record.tenant_id
        || referenced.gts_id != record.gts_id
        || referenced.resource_ref != record.resource_ref
        || referenced.subject_ref != record.subject_ref
    {
        return Err(UsageCollectorError::corrects_id_wrong_scope(corrects_id));
    }
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-cross-scope

    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-inactive-or-deactivating
    if referenced.status != UsageRecordStatus::Active {
        return Err(UsageCollectorError::corrects_id_inactive(corrects_id));
    }
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-l1-inactive-or-deactivating

    // @cpt-begin:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-compensation-valid
    Ok(())
    // @cpt-end:cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2:p1:inst-algo-semantics-counter-compensation-valid
}

/// Read the sign of a `UsageRecord.value` (`<0`, `0`, or `>0`).
///
/// `UsageRecord.value` is carried as [`rust_decimal::Decimal`] on every
/// surface, so the helper reduces to a tri-state sign read; non-numeric
/// inputs are excluded by the type at the deserialize boundary, and
/// `Decimal` admits no NaN / ±Inf representations.
fn value_signum(value: Decimal) -> i32 {
    use std::cmp::Ordering;
    match value.cmp(&Decimal::ZERO) {
        Ordering::Less => -1,
        Ordering::Greater => 1,
        Ordering::Equal => 0,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "validation_tests.rs"]
mod validation_tests;
