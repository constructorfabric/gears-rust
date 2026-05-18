//! Composition tests: `DomainError → AccountManagementError → CanonicalError`
//! preserves the pre-migration AIP-193 envelope shape variant-by-variant.
//!
//! The pre-migration regression line lived in `domain/error_tests.rs`
//! and asserted directly on `From<DomainError> for CanonicalError`.
//! That single-hop boundary is now replaced by the two-step pipeline
//! [`From<DomainError> for AccountManagementError`] +
//! [`From<AccountManagementError> for CanonicalError`] in
//! [`super::sdk_error_mapping`]. These tests assert that the
//! composition still produces the exact same `CanonicalError` envelope
//! shape — same AIP-193 category, HTTP status, resource type, and key
//! context fields — for every `DomainError` variant.

use std::time::Duration;

use account_management_sdk::error::AccountManagementError;
use modkit_canonical_errors::CanonicalError;

use crate::domain::error::DomainError;
use crate::infra::sdk_error_mapping::account_management_error_to_canonical;

/// Run a `DomainError` through the production pipeline. For variants
/// that travel via the SDK boundary this is the two-step
/// `DomainError → AccountManagementError → CanonicalError`; for the
/// `IntegrityCheckInProgress` bypass (not part of the inter-module
/// SDK contract) the `From<DomainError> for CanonicalError` impl
/// short-circuits directly to the canonical envelope.
fn round_trip(d: DomainError) -> CanonicalError {
    CanonicalError::from(d)
}

/// Variants of [`round_trip`] for tests that want to pin the SDK shape
/// before the canonical conversion. Unsuitable for
/// `IntegrityCheckInProgress` (it bypasses the SDK boundary).
#[allow(dead_code)]
fn round_trip_via_sdk(d: DomainError) -> CanonicalError {
    let sdk: AccountManagementError = d.into();
    account_management_error_to_canonical(sdk)
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
fn metadata_schema_not_registered_uses_metadata_resource_type() {
    let canonical = round_trip(DomainError::MetadataSchemaNotRegistered {
        detail: "schema billing.v1 missing".to_owned(),
        schema: "billing.v1".to_owned(),
    });
    assert_eq!(canonical.status_code(), 404);
    assert_eq!(canonical.resource_name(), Some("billing.v1"));
    assert_eq!(
        canonical.resource_type(),
        Some(account_management_sdk::gts::TENANT_METADATA_RESOURCE_TYPE)
    );
}

#[test]
fn metadata_entry_not_found_uses_metadata_resource_type() {
    let canonical = round_trip(DomainError::MetadataEntryNotFound {
        detail: "entry z missing".to_owned(),
        entry: "z".to_owned(),
    });
    assert_eq!(canonical.status_code(), 404);
    assert_eq!(canonical.resource_name(), Some("z"));
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
fn pending_exists_maps_to_failed_precondition_on_conversion_request() {
    let canonical = round_trip(DomainError::PendingExists {
        request_id: "req-1".to_owned(),
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
    assert_eq!(ctx.violations[0].type_, "PENDING_EXISTS");
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
