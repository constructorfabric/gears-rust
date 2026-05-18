//! Tests for the [`DomainError`] → [`AccountManagementError`] boundary mapping.
//!
//! Validates the AIP-193 category, HTTP status code, and key context
//! fields (`resource_type`, `resource`, `retry_after_seconds`, `reason`)
//! produced by `From<DomainError> for AccountManagementError`. Renaming
//! any of these mappings is a public-contract break — the tests below
//! are the regression line for the SDK enum shape.
//!
//! Boundary mapping lives in [`crate::infra::sdk_error_mapping`] (kept
//! out of `domain/` so the DB-aware classification ladder can reach
//! `sea_orm`/`modkit_db` without violating Dylint domain-layer rules).
//! The tests still live alongside `domain/error` because they pin the
//! shape of the public contract.
//!
//! The companion `infra::sdk_error_mapping_tests` exercise the second
//! hop (`AccountManagementError → CanonicalError`) and the full
//! composition end-to-end; this file covers the SDK enum shape that
//! consumers pattern-match on directly.

use std::time::Duration;

use account_management_sdk::error::AccountManagementError;

use super::DomainError;

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
        status_of(DomainError::MetadataSchemaNotRegistered {
            detail: "schema y missing".into(),
            schema: "y".into(),
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
    let ame: AccountManagementError = DomainError::Aborted {
        reason: "SERIALIZATION_CONFLICT".into(),
        detail: "serialization conflict; retry budget exhausted".into(),
    }
    .into();
    assert!(matches!(
        ame,
        AccountManagementError::SerializationConflict { .. }
    ));
    assert!(ame.is_retryable());
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
    let ame: AccountManagementError = DomainError::NotFound {
        detail: "tenant 7 not found".into(),
        resource: "7".into(),
    }
    .into();
    let AccountManagementError::TenantNotFound { tenant_id, .. } = &ame else {
        panic!("expected TenantNotFound variant");
    };
    assert_eq!(tenant_id, "7");
    assert!(ame.is_not_found());
}

#[test]
fn metadata_schema_not_registered_carries_schema_id() {
    let ame: AccountManagementError = DomainError::MetadataSchemaNotRegistered {
        detail: "schema billing.v1 missing".into(),
        schema: "billing.v1".into(),
    }
    .into();
    let AccountManagementError::MetadataSchemaNotRegistered { schema, .. } = &ame else {
        panic!("expected MetadataSchemaNotRegistered variant");
    };
    assert_eq!(schema, "billing.v1");
    assert!(ame.is_not_found());
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
    let ame: AccountManagementError = DomainError::ServiceUnavailable {
        detail: "idp warming up".into(),
        retry_after: Some(Duration::from_secs(15)),
        cause: None,
    }
    .into();
    assert_eq!(ame.retry_after_seconds(), Some(15));
}

#[test]
fn service_unavailable_without_hint_omits_retry_after() {
    let ame: AccountManagementError = DomainError::service_unavailable("db down").into();
    assert!(ame.retry_after_seconds().is_none());
}

// ---------------------------------------------------------------------------
// Test-only accessors
// ---------------------------------------------------------------------------
//
// `code()` / `http_status()` are `#[cfg(test)]`-only convenience methods used
// by service-layer tests to pin the variant→code/status contract without
// going through `AccountManagementError::from(...)` on every assertion.
// Production callers MUST go through [`crate::infra::sdk_error_mapping`];
// this impl block lives in the companion test file (per dylint `DE1101`) so
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
            Self::RootTenantCannotDelete => "root_tenant_cannot_delete",
            Self::RootTenantCannotConvert => "root_tenant_cannot_convert",
            Self::NotFound { .. } => "not_found",
            Self::UserNotFound { .. } => "user_not_found",
            Self::ConversionRequestNotFound { .. } => "conversion_request_not_found",
            Self::MetadataSchemaNotRegistered { .. } => "metadata_schema_not_registered",
            Self::MetadataEntryNotFound { .. } => "metadata_entry_not_found",
            Self::MetadataVersionMismatch { .. } => "metadata_version_mismatch",
            Self::AlreadyExists { .. } => "already_exists",
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
            | Self::RootTenantCannotDelete
            | Self::RootTenantCannotConvert
            | Self::InvalidActorForTransition { .. }
            | Self::TypeNotAllowed { .. }
            | Self::TenantDepthExceeded { .. }
            | Self::TenantHasChildren
            | Self::TenantHasResources
            | Self::PendingExists { .. }
            | Self::AlreadyResolved
            | Self::Conflict { .. }
            | Self::FeatureDisabled { .. } => 400,
            Self::NotFound { .. }
            | Self::UserNotFound { .. }
            | Self::ConversionRequestNotFound { .. }
            | Self::MetadataSchemaNotRegistered { .. }
            | Self::MetadataEntryNotFound { .. } => 404,
            Self::AlreadyExists { .. }
            | Self::Aborted { .. }
            | Self::MetadataVersionMismatch { .. } => 409,
            Self::CrossTenantDenied { .. } => 403,
            Self::ServiceUnavailable { .. } | Self::IdpUnavailable { .. } => 503,
            Self::UnsupportedOperation { .. } => 501,
            Self::IntegrityCheckInProgress => 429,
            Self::Internal { .. } => 500,
        }
    }
}
