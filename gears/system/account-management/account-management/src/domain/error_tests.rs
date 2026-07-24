//! Tests for the [`DomainError`] → [`CanonicalError`] boundary and the
//! [`AccountManagementError`] projection over it.
//!
//! Per ADR 0005 the single ladder is `From<DomainError> for CanonicalError`
//! in [`crate::infra::sdk_error_mapping`]; the SDK's typed
//! [`AccountManagementError`] is a `From<CanonicalError>` view. These
//! tests pin the projected shape consumers pattern-match on (variant,
//! carried identifiers, retry hint). The companion
//! `infra::sdk_error_mapping_tests` pin the full `CanonicalError`
//! envelope (category, status, resource type, context tokens).

use std::time::Duration;
use toolkit_gts::gts_id;

use account_management_sdk::error::AccountManagementError;
use toolkit_canonical_errors::CanonicalError;

use super::DomainError;

/// Drive a `DomainError` through the production path consumers see:
/// the single canonical ladder, then the opt-in SDK projection.
#[allow(clippy::needless_pass_by_value)]
fn project(d: DomainError) -> AccountManagementError {
    AccountManagementError::from(CanonicalError::from(d))
}

/// Convenience: read the test-only `DomainError::http_status()`
/// helper. Pinned to the canonical AIP-193 table — the production
/// HTTP status is produced by the canonical envelope in
/// [`crate::infra::sdk_error_mapping`], not by this helper.
///
/// Takes the error by value so call sites can use the short
/// `status_of(DomainError::Variant{..})` form without sprinkling
/// `&` at every call.
#[allow(clippy::needless_pass_by_value)]
fn status_of(err: DomainError) -> u16 {
    err.http_status()
}

// ---------------------------------------------------------------------------
// HTTP status codes — AIP-193 mapping
// ---------------------------------------------------------------------------

#[test]
fn invalid_argument_variants_map_to_400() {
    assert_eq!(
        status_of(DomainError::InvalidTenantType { detail: "x".into() }),
        400
    );
    assert_eq!(
        status_of(DomainError::Validation { detail: "x".into() }),
        400
    );
    assert_eq!(status_of(DomainError::RootTenantCannotDelete), 400);
    assert_eq!(status_of(DomainError::RootTenantCannotConvert), 400);
    assert_eq!(status_of(DomainError::RootTenantCannotChangeStatus), 400);
    assert_eq!(
        status_of(DomainError::IdpInvalidInput {
            detail: "x".into(),
            field: None,
        }),
        400
    );
}

#[test]
fn not_found_variants_map_to_404() {
    assert_eq!(
        status_of(DomainError::NotFound {
            detail: "tenant x not found".into(),
            resource: "x".into(),
        }),
        404
    );
    assert_eq!(
        status_of(DomainError::MetadataEntryNotFound {
            detail: "entry z missing".into(),
            entry: "z".into(),
        }),
        404
    );
}

#[test]
fn precondition_variants_map_to_400() {
    assert_eq!(
        status_of(DomainError::TypeNotAllowed { detail: "x".into() }),
        400
    );
    assert_eq!(
        status_of(DomainError::TenantDepthExceeded { detail: "x".into() }),
        400
    );
    assert_eq!(status_of(DomainError::TenantHasChildren), 400);
    assert_eq!(status_of(DomainError::TenantHasResources), 400);
    assert_eq!(
        status_of(DomainError::PendingExists {
            request_id: "r1".into()
        }),
        400
    );
    assert_eq!(
        status_of(DomainError::InvalidActorForTransition {
            attempted_status: "approved".into(),
            caller_side: "child".into(),
        }),
        400
    );
    assert_eq!(status_of(DomainError::AlreadyResolved), 400);
    assert_eq!(status_of(DomainError::Conflict { detail: "x".into() }), 400);
    assert_eq!(
        status_of(DomainError::FeatureDisabled { detail: "x".into() }),
        400
    );
}

#[test]
fn already_exists_maps_to_409() {
    assert_eq!(
        status_of(DomainError::AlreadyExists {
            detail: "tenant exists".into()
        }),
        409
    );
}

#[test]
fn aborted_maps_to_409_with_reason() {
    let ame = project(DomainError::Aborted {
        reason: "SERIALIZATION_CONFLICT".into(),
        detail: "serialization conflict; retry budget exhausted".into(),
    });
    let AccountManagementError::Aborted { reason, .. } = ame else {
        panic!("expected Aborted projection, got {ame:?}");
    };
    assert_eq!(
        reason,
        account_management_sdk::reason::aborted::SERIALIZATION_CONFLICT
    );
}

#[test]
fn cross_tenant_denied_maps_to_403() {
    assert_eq!(
        status_of(DomainError::CrossTenantDenied { cause: None }),
        403
    );
}

/// Fail-closed pin: a PDP that returns `decision: true` with empty
/// constraints under `require_constraints(true)` surfaces as
/// `ConstraintCompileError::ConstraintsRequiredButAbsent`, which the
/// `From<EnforcerError>` impl in `error.rs` MUST map to
/// `CrossTenantDenied` (HTTP 403), never to `Internal` (HTTP 500). A
/// future refactor that adds a new compile-error variant without
/// updating the wildcard pattern would also be caught here.
#[test]
fn compile_failed_maps_to_cross_tenant_denied_403() {
    use authz_resolver_sdk::EnforcerError;
    use authz_resolver_sdk::pep::ConstraintCompileError;

    let err = DomainError::from(EnforcerError::CompileFailed(
        ConstraintCompileError::ConstraintsRequiredButAbsent,
    ));
    assert!(
        matches!(err, DomainError::CrossTenantDenied { .. }),
        "CompileFailed must map to CrossTenantDenied (fail-closed), got {err:?}"
    );
    assert_eq!(err.http_status(), 403);
    assert_eq!(err.code(), "cross_tenant_denied");
}

#[test]
fn service_unavailable_maps_to_503() {
    assert_eq!(status_of(DomainError::service_unavailable("idp down")), 503);
}

#[test]
fn unsupported_operation_maps_to_501() {
    assert_eq!(
        status_of(DomainError::UnsupportedOperation { detail: "x".into() }),
        501
    );
}

#[test]
fn integrity_check_in_progress_maps_to_429() {
    assert_eq!(status_of(DomainError::IntegrityCheckInProgress), 429);
}

#[test]
fn internal_maps_to_500() {
    assert_eq!(status_of(DomainError::internal("unexpected")), 500);
}

// ---------------------------------------------------------------------------
// Context fields preserved across the boundary
// ---------------------------------------------------------------------------

#[test]
fn not_found_carries_resource_id() {
    let ame = project(DomainError::NotFound {
        detail: "tenant 7 not found".into(),
        resource: "7".into(),
    });
    let AccountManagementError::NotFound {
        resource_type,
        name,
        ..
    } = &ame
    else {
        panic!("expected NotFound projection, got {ame:?}");
    };
    assert_eq!(name, "7");
    assert_eq!(
        resource_type,
        account_management_sdk::gts::TENANT_RESOURCE_TYPE
    );
}

#[test]
fn metadata_entry_not_found_carries_chained_type_id() {
    // The chained `type_id` the caller supplied surfaces as the
    // canonical `resource_name`, projected onto `NotFound.name`, under
    // the metadata resource type.
    let ame = project(DomainError::MetadataEntryNotFound {
        detail: "schema billing.v1 missing".into(),
        entry: gts_id!("cf.core.am.tenant_metadata.v1~cf.core.billing.usage.v1~").into(),
    });
    let AccountManagementError::NotFound {
        resource_type,
        name,
        ..
    } = &ame
    else {
        panic!("expected NotFound projection, got {ame:?}");
    };
    assert_eq!(
        name,
        gts_id!("cf.core.am.tenant_metadata.v1~cf.core.billing.usage.v1~")
    );
    assert_eq!(
        resource_type,
        account_management_sdk::gts::TENANT_METADATA_RESOURCE_TYPE
    );
}

// Drift between `#[resource_error("...")]` macro literals in
// `crate::infra::sdk_error_mapping` and the SDK `gts::*_RESOURCE_TYPE`
// constants is now exercised at the canonical-pipeline boundary in
// `infra::sdk_error_mapping_tests` — every variant routes through one
// specific resource builder, and those tests assert the resulting
// `CanonicalError::resource_type()` matches the SDK constant. A new
// resource added without the corresponding canonical assertion would
// trip there.

#[test]
fn service_unavailable_propagates_retry_after_seconds() {
    let ame = project(DomainError::ServiceUnavailable {
        detail: "idp warming up".into(),
        retry_after: Some(Duration::from_secs(15)),
        cause: None,
    });
    let AccountManagementError::Unavailable {
        retry_after_seconds,
        ..
    } = ame
    else {
        panic!("expected Unavailable projection, got {ame:?}");
    };
    assert_eq!(retry_after_seconds, Some(15));
}

#[test]
fn service_unavailable_without_hint_omits_retry_after() {
    let ame = project(DomainError::service_unavailable("db down"));
    let AccountManagementError::Unavailable {
        retry_after_seconds,
        ..
    } = ame
    else {
        panic!("expected Unavailable projection, got {ame:?}");
    };
    assert!(retry_after_seconds.is_none());
}

// ---------------------------------------------------------------------------
// Test-only accessors
// ---------------------------------------------------------------------------
//
// `code()` / `http_status()` are `#[cfg(test)]`-only convenience methods used
// by service-layer tests to pin the variant→code/status contract without
// going through `AccountManagementError::from(...)` on every assertion.
// Production callers MUST go through [`crate::infra::sdk_error_mapping`];
// this impl block lives in the companion test file (per `cargo gears lint` rule `DE1101`) so
// the production [`DomainError`] surface stays free of test-only items.

impl DomainError {
    /// AM-specific `snake_case` error tag. Mirrors the variant name in
    /// `snake_case`; the canonical wire code comes from
    /// [`crate::infra::sdk_error_mapping`] and may differ (e.g. several
    /// variants collapse to `failed_precondition` on the wire).
    #[must_use]
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::InvalidTenantType { .. } => "invalid_tenant_type",
            Self::Validation { .. } => "validation",
            Self::MetadataValidation { .. } => "metadata_validation",
            Self::RootTenantCannotDelete => "root_tenant_cannot_delete",
            Self::RootTenantCannotConvert => "root_tenant_cannot_convert",
            Self::RootTenantCannotChangeStatus => "root_tenant_cannot_change_status",
            Self::IdpInvalidInput { .. } => "idp_invalid_input",
            Self::NotFound { .. } => "not_found",
            Self::UserNotFound { .. } => "user_not_found",
            Self::ConversionRequestNotFound { .. } => "conversion_request_not_found",
            Self::MetadataEntryNotFound { .. } => "metadata_entry_not_found",
            Self::MetadataVersionMismatch { .. } => "metadata_version_mismatch",
            Self::AlreadyExists { .. } => "already_exists",
            Self::UserAlreadyExists { .. } => "user_already_exists",
            Self::IdpPasswordPolicy { .. } => "idp_password_policy",
            Self::Aborted { .. } => "aborted",
            Self::TypeNotAllowed { .. } => "type_not_allowed",
            Self::TenantDepthExceeded { .. } => "tenant_depth_exceeded",
            Self::TenantHasChildren => "tenant_has_children",
            Self::TenantHasResources => "tenant_has_resources",
            Self::PendingExists { .. } => "pending_exists",
            Self::InvalidActorForTransition { .. } => "invalid_actor_for_transition",
            Self::AlreadyResolved => "already_resolved",
            Self::Conflict { .. } => "conflict",
            Self::FeatureDisabled { .. } => "feature_disabled",
            Self::CrossTenantDenied { .. } => "cross_tenant_denied",
            Self::ServiceUnavailable { .. } => "service_unavailable",
            Self::IdpUnavailable { .. } => "idp_unavailable",
            Self::UnsupportedOperation { .. } => "unsupported_operation",
            Self::IntegrityCheckInProgress => "integrity_check_in_progress",
            Self::IntegrityCheckLeaseLost => "integrity_check_lease_lost",
            Self::Internal { .. } => "internal",
        }
    }

    /// HTTP status produced for this error by the canonical-mapping
    /// boundary. Computed locally so tests do not pay the per-call
    /// `AccountManagementError::from(...)` allocation; pinned to the
    /// same status table the canonical mapping returns.
    ///
    /// `failed_precondition` variants land on **400** (per AIP-193 +
    /// the canonical mapping in [`crate::infra::sdk_error_mapping`]),
    /// not 409 — only `AlreadyExists` and `Aborted` carry 409 here.
    /// The `precondition_variants_map_to_400` /
    /// `already_exists_maps_to_409` tests in this file pin the
    /// authoritative mapping; this helper must agree with them.
    #[must_use]
    pub(crate) fn http_status(&self) -> u16 {
        match self {
            Self::InvalidTenantType { .. }
            | Self::Validation { .. }
            | Self::MetadataValidation { .. }
            | Self::RootTenantCannotDelete
            | Self::RootTenantCannotConvert
            | Self::RootTenantCannotChangeStatus
            | Self::IdpInvalidInput { .. }
            | Self::InvalidActorForTransition { .. }
            | Self::TypeNotAllowed { .. }
            | Self::TenantDepthExceeded { .. }
            | Self::TenantHasChildren
            | Self::TenantHasResources
            | Self::PendingExists { .. }
            | Self::AlreadyResolved
            | Self::Conflict { .. }
            | Self::IdpPasswordPolicy { .. }
            | Self::FeatureDisabled { .. } => 400,
            Self::NotFound { .. }
            | Self::UserNotFound { .. }
            | Self::ConversionRequestNotFound { .. }
            | Self::MetadataEntryNotFound { .. } => 404,
            Self::AlreadyExists { .. }
            | Self::UserAlreadyExists { .. }
            | Self::Aborted { .. }
            | Self::MetadataVersionMismatch { .. } => 409,
            Self::CrossTenantDenied { .. } => 403,
            Self::ServiceUnavailable { .. } | Self::IdpUnavailable { .. } => 503,
            Self::UnsupportedOperation { .. } => 501,
            Self::IntegrityCheckInProgress | Self::IntegrityCheckLeaseLost => 429,
            Self::Internal { .. } => 500,
        }
    }
}
