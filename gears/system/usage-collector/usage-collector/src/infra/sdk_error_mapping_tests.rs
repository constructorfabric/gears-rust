//! Unit tests for the host-owned `UsageCollectorError` -> `CanonicalError` lift.
//!
//! Each test asserts the DESIGN §3.3 AIP-193 mapping (category -> HTTP status)
//! plus the module-specific `context.reason` / resource-type carry. The lift is
//! exposed as two surface-specific free fns
//! ([`usage_collector_error_to_canonical_for_usage_type`] for the catalog REST
//! surface and [`usage_collector_error_to_canonical_for_usage_record`] for the
//! ingestion REST surface); only `PermissionDenied` depends on the surface, so
//! the two entry points special-case it and share [`super::lift_common`] for
//! everything else.
//!
//! After the error-envelope compaction, the 503 `context.reason` triage codes
//! (`PLUGIN_READINESS` / `PLUGIN_TRANSIENT` / `AUTHZ_UNAVAILABLE`) and the
//! corrects-id `CORRECTS_ID_NOT_FOUND` 404 reason are no longer emitted — the
//! canonical `ServiceUnavailable` / `NotFound` contexts have no reason slot, so
//! those were batch-only JSON post-injections that the compaction removed.
//! Operator triage for 503s reads the curated `detail` string instead.

use toolkit_canonical_errors::{CanonicalError, Problem};
use usage_collector_sdk::{UsageCollectorError, UsageTypeGtsId};
use uuid::Uuid;

use super::{
    UsageRecordResource, UsageTypeResource,
    usage_collector_error_to_canonical_for_usage_record as lift_record,
    usage_collector_error_to_canonical_for_usage_type as lift_type, usage_record_error_to_problem,
};

const SAMPLE_USAGE_TYPE_ID: &str =
    "gts.cf.core.uc.usage_record.v1~cf.mini_chat._.tokens_consumed.v1";

fn sample_gts_id() -> UsageTypeGtsId {
    UsageTypeGtsId::new(SAMPLE_USAGE_TYPE_ID).expect("valid usage_record-derived usage-type gts_id")
}

#[test]
fn usage_type_resource_type_matches_uc_usage_type_marker() {
    let err: CanonicalError = UsageTypeResource::not_found("x")
        .with_resource("x")
        .create();
    assert_eq!(err.resource_type(), Some("gts.cf.core.uc.usage_type.v1~"));
}

#[test]
fn usage_record_resource_type_is_record_sibling() {
    let err: CanonicalError = UsageRecordResource::not_found("r")
        .with_resource("r")
        .create();
    assert_eq!(err.resource_type(), Some("gts.cf.core.uc.usage_record.v1~"));
}

// ---------------------------------------------------------------------------
// PermissionDenied is the only category whose canonical resource depends on the
// REST surface the call originated from.
// ---------------------------------------------------------------------------

#[test]
fn authorization_from_usage_type_surface_uses_usage_type_resource() {
    let c = lift_type(UsageCollectorError::permission_denied("denied"));
    assert_eq!(c.status_code(), 403);
    assert_eq!(c.title(), "Permission Denied");
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_type.v1~"));
}

#[test]
fn authorization_from_usage_record_surface_uses_usage_record_resource() {
    let c = lift_record(UsageCollectorError::permission_denied("denied"));
    assert_eq!(c.status_code(), 403);
    assert_eq!(c.title(), "Permission Denied");
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_record.v1~"));
}

#[test]
fn authorization_carries_native_authz_reason_on_both_surfaces() {
    for problem in [
        Problem::from(lift_type(UsageCollectorError::permission_denied(
            "tenant scope mismatch",
        ))),
        Problem::from(lift_record(UsageCollectorError::permission_denied(
            "tenant scope mismatch",
        ))),
    ] {
        assert_eq!(problem.status, 403);
        assert_eq!(
            problem_context_string(&problem, "reason").as_deref(),
            Some("AUTHZ")
        );
    }
}

#[test]
fn invalid_resource_ref_maps_to_400_invalid_argument() {
    let c = lift_record(UsageCollectorError::invalid_resource_ref(
        "resource_id must not be empty",
    ));
    assert_eq!(c.status_code(), 400);
    assert_eq!(c.title(), "Invalid Argument");
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_record.v1~"));
}

#[test]
fn metadata_size_exceeded_maps_to_400_invalid_argument_with_usage_record_resource() {
    let c = lift_record(UsageCollectorError::metadata_size_exceeded(9000, 8192));
    assert_eq!(c.status_code(), 400);
    assert_eq!(c.title(), "Invalid Argument");
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_record.v1~"));
}

#[test]
fn invalid_metadata_field_maps_to_400_invalid_argument() {
    let c = lift_type(UsageCollectorError::invalid_metadata_field(1, true));
    assert_eq!(c.status_code(), 400);
    assert_eq!(c.title(), "Invalid Argument");
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_type.v1~"));
}

#[test]
fn duplicate_metadata_field_maps_to_400_invalid_argument() {
    let c = lift_type(UsageCollectorError::duplicate_metadata_field(2));
    assert_eq!(c.status_code(), 400);
    assert_eq!(c.title(), "Invalid Argument");
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_type.v1~"));
}

#[test]
fn invalid_batch_size_maps_to_400_invalid_argument() {
    let c = lift_record(UsageCollectorError::invalid_batch_size(0, 1, 100));
    assert_eq!(c.status_code(), 400);
    assert_eq!(c.title(), "Invalid Argument");
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_record.v1~"));
}

/// `UnknownMetadataKey` lifts onto `InvalidArgument` (HTTP 400) and identifies
/// the `UsageType` whose closed shape was violated (`resource_type` +
/// `resource.name = gts_id`), even though the failing operation is record
/// submission — the variant's intrinsic resource is the type it references.
#[test]
fn unknown_metadata_key_maps_to_invalid_argument() {
    let gts_id = sample_gts_id();
    let c = lift_record(UsageCollectorError::unknown_metadata_key(
        &gts_id,
        "unexpected",
    ));
    assert_eq!(c.status_code(), 400);
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_type.v1~"));
    assert_eq!(c.resource_name(), Some(gts_id.as_ref()));
}

#[test]
fn usage_type_not_found_maps_to_404() {
    let gts_id = sample_gts_id();
    let c = lift_type(UsageCollectorError::usage_type_not_found(&gts_id));
    assert_eq!(c.status_code(), 404);
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_type.v1~"));
    assert_eq!(c.resource_name(), Some(gts_id.as_ref()));
}

#[test]
fn usage_record_not_found_maps_to_404() {
    let id = Uuid::from_u128(0x1234);
    let c = lift_record(UsageCollectorError::usage_record_not_found(id));
    assert_eq!(c.status_code(), 404);
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_record.v1~"));
}

#[test]
fn already_inactive_maps_to_409_aborted_with_already_inactive_reason() {
    let id = Uuid::from_u128(0xCAFE_BABE);
    let c = lift_record(UsageCollectorError::already_inactive(id));
    assert_eq!(c.status_code(), 409);
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_record.v1~"));
    assert_eq!(c.resource_name(), Some(id.to_string().as_str()));

    let problem = Problem::from(c);
    assert_eq!(
        problem_context_string(&problem, "reason").as_deref(),
        Some("ALREADY_INACTIVE"),
        "AlreadyInactive MUST carry context.reason=ALREADY_INACTIVE per the feature spec",
    );
}

#[test]
fn usage_type_already_exists_maps_to_409() {
    let gts_id = sample_gts_id();
    let c = lift_type(UsageCollectorError::usage_type_already_exists(&gts_id));
    assert_eq!(c.status_code(), 409);
    assert_eq!(c.resource_name(), Some(gts_id.as_ref()));
}

#[test]
fn usage_type_referenced_maps_to_409_aborted() {
    let gts_id = sample_gts_id();
    let c = lift_type(UsageCollectorError::usage_type_referenced(&gts_id, 7));
    assert_eq!(c.status_code(), 409);
    assert_eq!(c.resource_name(), Some(gts_id.as_ref()));
}

#[test]
fn idempotency_conflict_maps_to_409_aborted() {
    let existing_uuid = Uuid::from_u128(0xAABB);
    let c = lift_record(UsageCollectorError::idempotency_conflict(
        "key-1",
        existing_uuid,
    ));
    assert_eq!(c.status_code(), 409);
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_record.v1~"));
    assert_eq!(c.resource_name(), Some(existing_uuid.to_string().as_str()));
}

#[test]
fn negative_counter_value_carries_semantics_violation_reason() {
    use rust_decimal::Decimal;
    let c = lift_record(UsageCollectorError::negative_counter_value(Decimal::from(
        -1,
    )));
    assert_eq!(c.status_code(), 400);
    let problem = Problem::from(c);
    assert_eq!(
        first_field_violation_string(&problem, "reason").as_deref(),
        Some("SEMANTICS_VIOLATION"),
        "NegativeCounterValue routes to the SEMANTICS_VIOLATION wire reason",
    );
}

#[test]
fn non_negative_counter_compensation_carries_semantics_violation_reason() {
    use rust_decimal::Decimal;
    let c = lift_record(UsageCollectorError::non_negative_counter_compensation(
        Decimal::ZERO,
    ));
    assert_eq!(c.status_code(), 400);
    let problem = Problem::from(c);
    assert_eq!(
        first_field_violation_string(&problem, "reason").as_deref(),
        Some("SEMANTICS_VIOLATION"),
        "NonNegativeCounterCompensation routes to the SEMANTICS_VIOLATION wire reason",
    );
}

#[test]
fn gauge_compensation_rejected_maps_to_invalid_argument_with_reason() {
    let gts_id =
        UsageTypeGtsId::new("gts.cf.core.uc.usage_record.v1~tenant.example._.cpu_seconds.v1")
            .expect("gauge gts_id");
    let c = lift_record(UsageCollectorError::gauge_compensation_rejected(&gts_id));
    assert_eq!(c.status_code(), 400);
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_record.v1~"));
    let problem = Problem::from(c);
    assert_eq!(
        first_field_violation_string(&problem, "reason").as_deref(),
        Some("GAUGE_COMPENSATION_REJECTED"),
    );
}

#[test]
fn corrects_id_not_found_maps_to_404_without_reason() {
    // Post-injection removed: corrects-id-not-found now collapses into the
    // plain record `NotFound` (404). The `CORRECTS_ID_NOT_FOUND` machine
    // reason is no longer emitted (it was batch-only); the `detail` text
    // preserves the human distinction.
    let corrects_id = Uuid::from_u128(0xDEAD_BEEF);
    let problem =
        usage_record_error_to_problem(UsageCollectorError::corrects_id_not_found(corrects_id));
    assert_eq!(problem.status, 404);
    assert_eq!(problem_context_string(&problem, "reason"), None);
}

#[test]
fn plugin_unavailable_per_record_problem_is_503_without_reason() {
    let problem = usage_record_error_to_problem(UsageCollectorError::plugin_unavailable());
    assert_eq!(problem.status, 503);
    assert_eq!(problem_context_string(&problem, "reason"), None);
}

#[test]
fn service_unavailable_per_record_problem_is_503_without_reason() {
    let problem = usage_record_error_to_problem(UsageCollectorError::service_unavailable(
        "downstream connection reset",
        None,
    ));
    assert_eq!(problem.status, 503);
    assert_eq!(problem_context_string(&problem, "reason"), None);
}

#[test]
fn types_registry_unavailable_per_record_problem_is_503() {
    let problem = usage_record_error_to_problem(UsageCollectorError::types_registry_unavailable());
    assert_eq!(problem.status, 503);
}

#[test]
fn corrects_id_targets_compensation_maps_to_409_aborted_with_reason() {
    let corrects_id = Uuid::from_u128(7);
    let c = lift_record(UsageCollectorError::corrects_id_targets_compensation(
        corrects_id,
    ));
    assert_eq!(c.status_code(), 409);
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_record.v1~"));
    let problem = Problem::from(c);
    assert_eq!(
        problem_context_string(&problem, "reason").as_deref(),
        Some("CORRECTS_ID_TARGETS_COMPENSATION"),
    );
}

#[test]
fn corrects_id_wrong_scope_maps_to_409_aborted_with_reason() {
    let corrects_id = Uuid::from_u128(8);
    let c = lift_record(UsageCollectorError::corrects_id_wrong_scope(corrects_id));
    assert_eq!(c.status_code(), 409);
    let problem = Problem::from(c);
    assert_eq!(
        problem_context_string(&problem, "reason").as_deref(),
        Some("CORRECTS_ID_WRONG_SCOPE"),
    );
}

#[test]
fn corrects_id_inactive_maps_to_409_aborted_with_reason() {
    let corrects_id = Uuid::from_u128(9);
    let c = lift_record(UsageCollectorError::corrects_id_inactive(corrects_id));
    assert_eq!(c.status_code(), 409);
    let problem = Problem::from(c);
    assert_eq!(
        problem_context_string(&problem, "reason").as_deref(),
        Some("CORRECTS_ID_INACTIVE"),
    );
}

#[test]
fn plugin_unavailable_maps_to_503_on_both_surfaces() {
    for c in [
        lift_type(UsageCollectorError::plugin_unavailable()),
        lift_record(UsageCollectorError::plugin_unavailable()),
    ] {
        assert_eq!(c.status_code(), 503);
    }
}

#[test]
fn service_unavailable_maps_to_503() {
    let c = lift_record(UsageCollectorError::service_unavailable(
        "transient",
        Some(30),
    ));
    assert_eq!(c.status_code(), 503);
}

#[test]
fn types_registry_unavailable_maps_to_503() {
    let c = lift_record(UsageCollectorError::types_registry_unavailable());
    assert_eq!(c.status_code(), 503);
}

#[test]
fn internal_maps_to_500_and_carries_diagnostic() {
    let c = lift_record(UsageCollectorError::internal("secret diag"));
    assert_eq!(c.status_code(), 500);
    assert_eq!(c.diagnostic(), Some("secret diag"));
}

// ---------------------------------------------------------------------------
// Register-flow Problem envelope tests.
// ---------------------------------------------------------------------------

fn problem_context_string(problem: &Problem, key: &str) -> Option<String> {
    problem
        .context
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn first_field_violation_string(problem: &Problem, key: &str) -> Option<String> {
    problem
        .context
        .get("field_violations")
        .and_then(serde_json::Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|v| v.get(key))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

#[test]
fn invalid_base_gts_id_envelope_carries_field_violation_discriminator() {
    let raw = "gts.cf.core.metric.histogram.v1~bogus".to_owned();
    let reason = "usage type gts_id must derive from the reserved base \
                  `gts.cf.core.uc.usage_record.v1~`"
        .to_owned();
    let problem = Problem::from(lift_type(UsageCollectorError::invalid_usage_type_gts_id(
        &raw, &reason,
    )));
    assert_eq!(problem.status, 400);
    assert_eq!(
        first_field_violation_string(&problem, "field").as_deref(),
        Some("gts_id")
    );
    assert_eq!(
        first_field_violation_string(&problem, "reason").as_deref(),
        Some("INVALID_BASE_GTS_ID")
    );
    let description = first_field_violation_string(&problem, "description").unwrap_or_default();
    assert!(
        description.contains(raw.as_str()),
        "field_violations[0].description MUST echo the rejected raw value (got `{description}`)",
    );
}

#[test]
fn create_usage_type_already_exists_envelope_carries_resource_identity() {
    let gts_id = sample_gts_id();
    let canonical = lift_type(UsageCollectorError::usage_type_already_exists(&gts_id));
    assert_eq!(canonical.status_code(), 409);
    assert_eq!(
        canonical.resource_type(),
        Some("gts.cf.core.uc.usage_type.v1~")
    );
    assert_eq!(canonical.resource_name(), Some(gts_id.as_ref()));
}

#[test]
fn duplicate_metadata_field_envelope_carries_indexed_field_path() {
    let problem = Problem::from(lift_type(UsageCollectorError::duplicate_metadata_field(1)));
    assert_eq!(problem.status, 400);
    assert_eq!(
        first_field_violation_string(&problem, "field").as_deref(),
        Some("metadata_fields[1]")
    );
    assert_eq!(
        first_field_violation_string(&problem, "reason").as_deref(),
        Some("INVALID_METADATA_FIELDS_DUPLICATE")
    );
}

#[test]
fn empty_string_metadata_field_envelope_carries_indexed_field_path() {
    let problem = Problem::from(lift_type(UsageCollectorError::invalid_metadata_field(
        0, true,
    )));
    assert_eq!(problem.status, 400);
    assert_eq!(
        first_field_violation_string(&problem, "field").as_deref(),
        Some("metadata_fields[0]")
    );
    assert_eq!(
        first_field_violation_string(&problem, "reason").as_deref(),
        Some("INVALID_METADATA_FIELDS_EMPTY_STRING")
    );
}

#[test]
fn plugin_unavailable_lifts_to_service_unavailable_envelope() {
    let problem = Problem::from(lift_type(UsageCollectorError::plugin_unavailable()));
    assert_eq!(problem.status, 503);
}

#[test]
fn invalid_metadata_key_uses_usage_record_resource() {
    let c = lift_record(UsageCollectorError::invalid_metadata_key(
        "metadata key must not be empty",
    ));
    assert_eq!(c.status_code(), 400);
    assert_eq!(c.resource_type(), Some("gts.cf.core.uc.usage_record.v1~"));
}

// ---------------------------------------------------------------------------
// Exhaustiveness fences: drive every variant that can fire on a given surface
// through that surface's lift and assert an in-range status. A future variant
// added without a corresponding `lift_common` arm trips the `debug_assert!`.
// ---------------------------------------------------------------------------

fn every_usage_type_surface_variant() -> Vec<UsageCollectorError> {
    let gts_id = sample_gts_id();
    vec![
        UsageCollectorError::permission_denied("denied"),
        UsageCollectorError::invalid_metadata_field(0, true),
        UsageCollectorError::duplicate_metadata_field(1),
        UsageCollectorError::invalid_usage_type_gts_id("bad", "r"),
        UsageCollectorError::invalid_usage_kind("x"),
        UsageCollectorError::usage_type_not_found(&gts_id),
        UsageCollectorError::usage_type_already_exists(&gts_id),
        UsageCollectorError::usage_type_referenced(&gts_id, 3),
        UsageCollectorError::plugin_unavailable(),
        UsageCollectorError::types_registry_unavailable(),
        UsageCollectorError::service_unavailable("x", None),
        UsageCollectorError::internal("x"),
    ]
}

fn every_usage_record_surface_variant() -> Vec<UsageCollectorError> {
    use rust_decimal::Decimal;

    let gts_id = sample_gts_id();
    let uuid = Uuid::new_v4();
    vec![
        UsageCollectorError::permission_denied("denied"),
        UsageCollectorError::negative_counter_value(Decimal::from(-1)),
        UsageCollectorError::non_negative_counter_compensation(Decimal::ZERO),
        UsageCollectorError::invalid_batch_size(0, 1, 100),
        UsageCollectorError::metadata_size_exceeded(9000, 8192),
        UsageCollectorError::invalid_metadata_key("r"),
        UsageCollectorError::invalid_metadata_filter("r"),
        UsageCollectorError::invalid_resource_ref("r"),
        UsageCollectorError::invalid_subject_ref("r"),
        UsageCollectorError::invalid_idempotency_key("r"),
        UsageCollectorError::invalid_usage_type_gts_id("bad", "r"),
        UsageCollectorError::unknown_metadata_key(&gts_id, "k"),
        UsageCollectorError::usage_record_not_found(uuid),
        UsageCollectorError::already_inactive(uuid),
        UsageCollectorError::idempotency_conflict("idem-fence", uuid),
        UsageCollectorError::gauge_compensation_rejected(&gts_id),
        UsageCollectorError::corrects_id_not_found(uuid),
        UsageCollectorError::corrects_id_targets_compensation(uuid),
        UsageCollectorError::corrects_id_wrong_scope(uuid),
        UsageCollectorError::corrects_id_inactive(uuid),
        UsageCollectorError::plugin_unavailable(),
        UsageCollectorError::types_registry_unavailable(),
        UsageCollectorError::service_unavailable("x", None),
        UsageCollectorError::internal("x"),
    ]
}

#[test]
fn lift_type_covers_every_usage_type_surface_variant() {
    for err in every_usage_type_surface_variant() {
        let label = format!("{err:?}");
        let problem = Problem::from(lift_type(err));
        assert!(
            (400..=599).contains(&problem.status),
            "lift_type({label}) produced an out-of-range status {}",
            problem.status,
        );
    }
}

#[test]
fn lift_record_covers_every_usage_record_surface_variant() {
    for err in every_usage_record_surface_variant() {
        let label = format!("{err:?}");
        let problem = Problem::from(lift_record(err));
        assert!(
            (400..=599).contains(&problem.status),
            "lift_record({label}) produced an out-of-range status {}",
            problem.status,
        );
    }
}
