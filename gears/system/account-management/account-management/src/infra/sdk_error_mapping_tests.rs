//! Regression tests for the single `From<DomainError> for CanonicalError`
//! ladder in [`super::sdk_error_mapping`].
//!
//! Per ADR 0005 there is one classification ladder; these tests pin the
//! exact `CanonicalError` envelope it produces — AIP-193 category, HTTP
//! status, resource type, and key context fields (`field_violations`,
//! `violations`, `reason`) — for every `DomainError` variant. They are
//! the unit-level wire-invariant guard; the HTTP-level sweep lives in
//! `tests/api_status_mapping_test.rs`.

use std::time::Duration;
use toolkit_gts::gts_id;

use toolkit_canonical_errors::{CanonicalError, InvalidArgument};

use crate::domain::error::DomainError;

/// Run a `DomainError` through the single canonical ladder.
fn round_trip(d: DomainError) -> CanonicalError {
    CanonicalError::from(d)
}

// ---------------------------------------------------------------------------
// InvalidArgument (HTTP 400)
// ---------------------------------------------------------------------------

#[test]
fn invalid_tenant_type_maps_to_invalid_argument() {
    let canonical = round_trip(DomainError::InvalidTenantType {
        detail: "bad type".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

#[test]
fn validation_maps_to_invalid_argument_with_tenant_resource() {
    let canonical = round_trip(DomainError::Validation {
        detail: "bad name".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

#[test]
fn metadata_validation_maps_to_invalid_argument_with_metadata_resource() {
    // Pins the split introduced for the REST surface: metadata-content
    // failures (malformed `type_id`, null body, GTS body validation
    // failure) MUST carry the metadata GTS resource type on the
    // canonical envelope. The tenant-state guards keep `Validation`
    // (and `TenantResource`) — see `validation_maps_to_invalid_argument_with_tenant_resource`
    // above for the sibling pin.
    let canonical = round_trip(DomainError::MetadataValidation {
        detail: "metadata value must not be null".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_METADATA_RESOURCE_TYPE)
    );
}

#[test]
fn root_tenant_cannot_delete_maps_to_400() {
    let canonical = round_trip(DomainError::RootTenantCannotDelete);
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

#[test]
fn root_tenant_cannot_convert_maps_to_400() {
    let canonical = round_trip(DomainError::RootTenantCannotConvert);
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

/// Symmetric coverage with `_delete` / `_convert` above: the new
/// `RootTenantCannotChangeStatus` variant (added to close the
/// suspend/unsuspend protection gap discovered via e2e probing) must
/// map to 400 `invalid_argument` with the tenant resource type, so
/// the wire envelope is indistinguishable from the existing
/// root-protection rejections.
#[test]
fn root_tenant_cannot_change_status_maps_to_400() {
    let canonical = round_trip(DomainError::RootTenantCannotChangeStatus);
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

/// Pin the full wire shape for the `IdP` plugin's permanent
/// shape-rejection: 400 `invalid_argument` on `TenantResource` with
/// the dotted-path `field` carried as the canonical
/// `field_violations[0].field` (not squashed into the description).
/// `reason = "IDP_INVALID_INPUT"` is the discriminator clients use
/// to tell this rejection from generic `VALIDATION` (`InvalidRequest`)
/// without parsing `detail`.
#[test]
fn idp_invalid_input_with_field_carries_dotted_path_on_canonical() {
    let canonical = round_trip(DomainError::IdpInvalidInput {
        detail: "realm_name must be non-empty".to_owned(),
        field: Some("provisioning_metadata.realm_name".to_owned()),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::InvalidArgument { ctx, .. } = canonical else {
        panic!("expected CanonicalError::InvalidArgument");
    };
    let InvalidArgument::FieldViolations { field_violations } = ctx else {
        panic!("expected InvalidArgument::FieldViolations ctx");
    };
    assert_eq!(field_violations.len(), 1);
    assert_eq!(
        field_violations[0].field, "provisioning_metadata.realm_name",
        "dotted-path field MUST survive to the canonical envelope, not be squashed into detail"
    );
    assert_eq!(
        field_violations[0].description,
        "realm_name must be non-empty"
    );
    assert_eq!(field_violations[0].reason, "IDP_INVALID_INPUT");
}

/// Companion to the `Some(...)` pin above: when the plugin can't
/// localise the violation to a sub-key (`field = None`), the
/// canonical envelope falls back to the shared
/// `"provisioning_metadata"` field key — every `IdP` plugin shares
/// this surface — so callers always see a structured attribution
/// rather than a missing field.
#[test]
fn idp_invalid_input_without_field_falls_back_to_provisioning_metadata() {
    let canonical = round_trip(DomainError::IdpInvalidInput {
        detail: "metadata body rejected".to_owned(),
        field: None,
    });
    assert_eq!(canonical.status_code(), 400);
    let CanonicalError::InvalidArgument { ctx, .. } = canonical else {
        panic!("expected CanonicalError::InvalidArgument");
    };
    let InvalidArgument::FieldViolations { field_violations } = ctx else {
        panic!("expected InvalidArgument::FieldViolations ctx");
    };
    assert_eq!(field_violations[0].field, "provisioning_metadata");
    assert_eq!(field_violations[0].reason, "IDP_INVALID_INPUT");
}

// ---------------------------------------------------------------------------
// NotFound (HTTP 404)
// ---------------------------------------------------------------------------

#[test]
fn not_found_carries_resource_name_and_tenant_type() {
    let canonical = round_trip(DomainError::NotFound {
        detail: "tenant 7 not found".to_owned(),
        resource: "7".to_owned(),
    });
    assert_eq!(canonical.status_code(), 404);
    assert_eq!(canonical.resource_name(), Some("7"));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

#[test]
fn user_not_found_maps_to_not_found_404_with_user_resource() {
    // Pins the per-resource NotFound for `delete_user` / `list_users`
    // user-id lookups: 404 + `USER_RESOURCE_TYPE` + the supplied id
    // surfaces as `resource_name`. Without this, a future drift in
    // the mapper (e.g. routing through `TenantResource::not_found`
    // because the variant lives under tenant scope) would silently
    // change the resource type on the wire.
    let user_id = "00000000-0000-0000-0000-000000000077";
    let canonical = round_trip(DomainError::UserNotFound {
        detail: format!("user {user_id} not found"),
        resource: user_id.to_owned(),
    });
    assert_eq!(canonical.status_code(), 404);
    assert_eq!(canonical.resource_name(), Some(user_id));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::USER_RESOURCE_TYPE)
    );
    assert!(
        matches!(canonical, CanonicalError::NotFound { .. }),
        "UserNotFound MUST surface as the NotFound variant"
    );
}

#[test]
fn conversion_request_not_found_maps_to_not_found_404() {
    // Pins the wire shape for conversion-request lookups that miss
    // their target (`cancel` / `reject` / `approve` / `get`).
    // Distinct from `PendingExists` (covered separately at line 250):
    // 404 instead of 409, NotFound variant instead of AlreadyExists,
    // request-id carried as `resource_name` so the caller can show
    // the missing id without parsing `detail`.
    let req_id = "11111111-2222-3333-4444-555555555555";
    let canonical = round_trip(DomainError::ConversionRequestNotFound {
        detail: format!("conversion request {req_id} not found"),
        resource: req_id.to_owned(),
    });
    assert_eq!(canonical.status_code(), 404);
    assert_eq!(canonical.resource_name(), Some(req_id));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::CONVERSION_REQUEST_RESOURCE_TYPE)
    );
    assert!(
        matches!(canonical, CanonicalError::NotFound { .. }),
        "ConversionRequestNotFound MUST surface as the NotFound variant"
    );
}

#[test]
fn metadata_entry_not_found_uses_metadata_resource_type_with_chained_type_id_as_name() {
    // Unified metadata 404: both "schema unknown to registry" and
    // "entry missing for tenant" collapse to
    // `MetadataEntryNotFound` and surface as
    // `TENANT_METADATA_RESOURCE_TYPE` (`gts.cf.core.am.tenant_metadata.v1~`)
    // with the chained `type_id` the caller supplied as
    // `resource_name`.
    let chain = gts_id!("cf.core.am.tenant_metadata.v1~cf.core.billing.usage.v1~");
    let canonical = round_trip(DomainError::MetadataEntryNotFound {
        detail: "entry missing".to_owned(),
        entry: chain.to_owned(),
    });
    assert_eq!(canonical.status_code(), 404);
    assert_eq!(canonical.resource_name(), Some(chain));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_METADATA_RESOURCE_TYPE)
    );
}

// ---------------------------------------------------------------------------
// AlreadyExists (HTTP 409)
// ---------------------------------------------------------------------------

#[test]
fn already_exists_maps_to_409() {
    let canonical = round_trip(DomainError::AlreadyExists {
        detail: "tenant exists".to_owned(),
    });
    assert_eq!(canonical.status_code(), 409);
    assert_eq!(canonical.resource_name(), Some("tenant"));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

// ---------------------------------------------------------------------------
// Aborted (HTTP 409 with reason)
// ---------------------------------------------------------------------------

#[test]
fn aborted_maps_to_409_with_reason() {
    let canonical = round_trip(DomainError::Aborted {
        reason: "SERIALIZATION_CONFLICT".to_owned(),
        detail: "serialization conflict; retry budget exhausted".to_owned(),
    });
    assert_eq!(canonical.status_code(), 409);
    let CanonicalError::Aborted { ctx, .. } = canonical else {
        panic!("expected Aborted variant");
    };
    assert_eq!(ctx.reason, "SERIALIZATION_CONFLICT");
}

#[test]
fn metadata_version_mismatch_maps_to_aborted_409_with_reason() {
    // `upsert_metadata` with `expected_version` not matching the stored
    // row surfaces as the Aborted variant (HTTP 409) tagged with the
    // `METADATA_VERSION_MISMATCH` reason. The reason token is the
    // contract callers branch on to distinguish a stale-version
    // conflict from a generic 409, and a future mapper drift that
    // dropped `with_reason` would change the wire envelope in a way
    // unit-tested ONLY here.
    let chain = gts_id!("cf.core.am.tenant_metadata.v1~cf.core.billing.usage.v1~");
    let canonical = round_trip(DomainError::MetadataVersionMismatch {
        entry: chain.to_owned(),
        expected: 4,
        current: 7,
    });
    assert_eq!(canonical.status_code(), 409);
    assert_eq!(canonical.resource_name(), Some(chain));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_METADATA_RESOURCE_TYPE)
    );
    let CanonicalError::Aborted { ctx, .. } = canonical else {
        panic!("MetadataVersionMismatch MUST surface as the Aborted variant");
    };
    assert_eq!(
        ctx.reason, "METADATA_VERSION_MISMATCH",
        "envelope MUST pin the `METADATA_VERSION_MISMATCH` reason token"
    );
}

// ---------------------------------------------------------------------------
// FailedPrecondition (HTTP 400)
// ---------------------------------------------------------------------------

#[test]
fn type_not_allowed_maps_to_failed_precondition() {
    let canonical = round_trip(DomainError::TypeNotAllowed {
        detail: "child of leaf".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations.len(), 1);
    assert_eq!(ctx.violations[0].subject, "tenant_type");
    assert_eq!(ctx.violations[0].type_, "TYPE_NOT_ALLOWED");
}

#[test]
fn tenant_depth_exceeded_maps_to_failed_precondition() {
    let canonical = round_trip(DomainError::TenantDepthExceeded {
        detail: "depth 7 > 6".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "depth");
    assert_eq!(ctx.violations[0].type_, "TENANT_DEPTH_EXCEEDED");
}

#[test]
fn tenant_has_children_maps_to_failed_precondition() {
    let canonical = round_trip(DomainError::TenantHasChildren);
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "tenant");
    assert_eq!(ctx.violations[0].type_, "TENANT_HAS_CHILDREN");
}

#[test]
fn tenant_has_resources_maps_to_failed_precondition() {
    let canonical = round_trip(DomainError::TenantHasResources);
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "tenant");
    assert_eq!(ctx.violations[0].type_, "TENANT_HAS_RESOURCES");
}

#[test]
fn pending_exists_maps_to_already_exists_409_on_conversion_request() {
    // Duplicate-on-create per AIP-193: the at-most-one-pending invariant
    // surfaces as `code=pending_exists` (HTTP 409). The OpenAPI spec
    // (`docs/account-management-v1.yaml`) documents the 409, so this
    // test pins both the wire status and the resource_name carrying
    // the existing `request_id`.
    let canonical = round_trip(DomainError::PendingExists {
        request_id: "req-1".to_owned(),
    });
    assert_eq!(canonical.status_code(), 409);
    assert_eq!(canonical.resource_name(), Some("req-1"));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::CONVERSION_REQUEST_RESOURCE_TYPE)
    );
    assert!(
        matches!(canonical, CanonicalError::AlreadyExists { .. }),
        "expected AlreadyExists variant for pending_exists; the duplicate-on-create \
         contract is HTTP 409, not 400 failed_precondition",
    );
}

#[test]
fn invalid_actor_for_transition_maps_to_failed_precondition_on_conversion_request() {
    let canonical = round_trip(DomainError::InvalidActorForTransition {
        attempted_status: "approved".to_owned(),
        caller_side: "child".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::CONVERSION_REQUEST_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "conversion_request");
    assert_eq!(ctx.violations[0].type_, "INVALID_ACTOR_FOR_TRANSITION");
}

#[test]
fn already_resolved_maps_to_failed_precondition_on_conversion_request() {
    let canonical = round_trip(DomainError::AlreadyResolved);
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::CONVERSION_REQUEST_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "conversion_request");
    assert_eq!(ctx.violations[0].type_, "ALREADY_RESOLVED");
}

#[test]
fn conflict_maps_to_failed_precondition_with_request_subject() {
    let canonical = round_trip(DomainError::Conflict {
        detail: "tenant deleted".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "request");
    assert_eq!(ctx.violations[0].type_, "PRECONDITION_FAILED");
}

#[test]
fn feature_disabled_maps_to_failed_precondition_on_configuration() {
    let canonical = round_trip(DomainError::FeatureDisabled {
        detail: "feature off".to_owned(),
    });
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::FailedPrecondition { ctx, .. } = canonical else {
        panic!("expected FailedPrecondition variant");
    };
    assert_eq!(ctx.violations[0].subject, "configuration");
    assert_eq!(ctx.violations[0].type_, "FEATURE_DISABLED");
}

// ---------------------------------------------------------------------------
// PermissionDenied (HTTP 403)
// ---------------------------------------------------------------------------

#[test]
fn cross_tenant_denied_maps_to_403_with_reason() {
    let canonical = round_trip(DomainError::CrossTenantDenied { cause: None });
    assert_eq!(canonical.status_code(), 403);
    let CanonicalError::PermissionDenied { ctx, .. } = canonical else {
        panic!("expected PermissionDenied variant");
    };
    assert_eq!(ctx.reason, "CROSS_TENANT_DENIED");
}

// ---------------------------------------------------------------------------
// ServiceUnavailable (HTTP 503)
// ---------------------------------------------------------------------------

#[test]
fn service_unavailable_maps_to_503_with_retry_after() {
    let canonical = round_trip(DomainError::ServiceUnavailable {
        detail: "idp warming up".to_owned(),
        retry_after: Some(Duration::from_secs(15)),
        cause: None,
    });
    assert_eq!(canonical.status_code(), 503);
    let CanonicalError::ServiceUnavailable { ctx, .. } = canonical else {
        panic!("expected ServiceUnavailable variant");
    };
    assert_eq!(ctx.retry_after_seconds, Some(15));
}

#[test]
fn idp_unavailable_maps_to_503_without_retry_after() {
    let canonical = round_trip(DomainError::IdpUnavailable {
        detail: "vendor SDK error: token expired".to_owned(),
    });
    assert_eq!(canonical.status_code(), 503);
    let CanonicalError::ServiceUnavailable { ctx, .. } = canonical else {
        panic!("expected ServiceUnavailable variant");
    };
    assert!(ctx.retry_after_seconds.is_none());
}

// ---------------------------------------------------------------------------
// Unimplemented (HTTP 501)
// ---------------------------------------------------------------------------

#[test]
fn unsupported_operation_maps_to_501() {
    let canonical = round_trip(DomainError::UnsupportedOperation {
        detail: "vendor x lacks profile-edit".to_owned(),
    });
    assert_eq!(canonical.status_code(), 501);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
}

// ---------------------------------------------------------------------------
// ResourceExhausted (HTTP 429)
// ---------------------------------------------------------------------------

#[test]
fn integrity_check_in_progress_maps_to_429_with_quota_violation() {
    let canonical = round_trip(DomainError::IntegrityCheckInProgress);
    assert_eq!(canonical.status_code(), 429);
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_RESOURCE_TYPE)
    );
    let CanonicalError::ResourceExhausted { ctx, .. } = canonical else {
        panic!("expected ResourceExhausted variant");
    };
    assert_eq!(ctx.violations.len(), 1);
    assert_eq!(ctx.violations[0].subject, "integrity_check");
}

// ---------------------------------------------------------------------------
// Internal (HTTP 500)
// ---------------------------------------------------------------------------

#[test]
fn internal_maps_to_500() {
    let canonical = round_trip(DomainError::Internal {
        diagnostic: "unclassified".to_owned(),
        cause: None,
    });
    assert_eq!(canonical.status_code(), 500);
}
