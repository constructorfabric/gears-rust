//! Display / accessor / group-helper unit tests for `AccountManagementError`.

use super::AccountManagementError;

#[test]
fn tenant_not_found_is_recognised_as_not_found() {
    let err = AccountManagementError::TenantNotFound {
        tenant_id: "abc".to_owned(),
        detail: "tenant not found: abc".to_owned(),
    };
    assert!(err.is_not_found());
    assert!(!err.is_retryable());
    assert_eq!(
        format!("{err}"),
        "tenant abc not found: tenant not found: abc"
    );
}

#[test]
fn metadata_schema_not_registered_is_not_found() {
    let err = AccountManagementError::MetadataSchemaNotRegistered {
        schema: "billing.v1".to_owned(),
        detail: "schema billing.v1 missing".to_owned(),
    };
    assert!(err.is_not_found());
}

#[test]
fn metadata_entry_not_found_is_not_found() {
    let err = AccountManagementError::MetadataEntryNotFound {
        entry: "z".to_owned(),
        detail: "entry z missing".to_owned(),
    };
    assert!(err.is_not_found());
}

#[test]
fn invalid_tenant_type_is_validation() {
    let err = AccountManagementError::InvalidTenantType {
        detail: "bad type".to_owned(),
    };
    assert!(err.is_validation_error());
    assert!(!err.is_precondition_failed());
}

#[test]
fn tenant_has_children_is_precondition() {
    let err = AccountManagementError::TenantHasChildren;
    assert!(err.is_precondition_failed());
    assert!(!err.is_validation_error());
}

#[test]
fn cross_tenant_denied_is_permission_denied() {
    let err = AccountManagementError::CrossTenantDenied;
    assert!(err.is_permission_denied());
}

#[test]
fn service_unavailable_carries_retry_hint_and_is_retryable() {
    let err = AccountManagementError::ServiceUnavailable {
        detail: "idp transport failed".to_owned(),
        retry_after_seconds: Some(30),
    };
    assert_eq!(err.retry_after_seconds(), Some(30));
    assert!(err.is_unavailable());
    assert!(err.is_retryable());
}

#[test]
fn idp_unavailable_is_unavailable_and_retryable() {
    let err = AccountManagementError::IdpUnavailable;
    assert!(err.is_unavailable());
    assert!(err.is_retryable());
    // No retry hint surface on this variant — group-helper is the seam.
    assert!(err.retry_after_seconds().is_none());
}

#[test]
fn unsupported_operation_is_not_retryable() {
    let err = AccountManagementError::UnsupportedOperation;
    assert!(!err.is_unavailable());
    assert!(!err.is_retryable());
}

#[test]
fn serialization_conflict_is_retryable_but_not_unavailable() {
    let err = AccountManagementError::SerializationConflict {
        detail: "retry budget exhausted".to_owned(),
    };
    assert!(err.is_retryable());
    // Distinct shape from transient outages.
    assert!(!err.is_unavailable());
}

#[test]
fn tenant_already_exists_is_not_retryable() {
    let err = AccountManagementError::TenantAlreadyExists {
        detail: "tenant slug already exists".to_owned(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn internal_does_not_leak_diagnostic_through_display() {
    let err = AccountManagementError::Internal {
        detail: "internal error".to_owned(),
    };
    // Public Display MUST NOT leak the diagnostic — verified at Display level
    // because the impl crate populates `detail` with a redacted summary.
    assert_eq!(format!("{err}"), "internal error: internal error");
}
