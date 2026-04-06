use super::*;

#[test]
fn invalid_ref_constructor_sets_reason() {
    let e = CredStoreError::invalid_ref("must not be empty");
    assert_eq!(e.to_string(), "invalid secret reference: must not be empty");
}

#[test]
fn service_unavailable_constructor_sets_message() {
    let e = CredStoreError::service_unavailable("backend down");
    assert!(matches!(e, CredStoreError::ServiceUnavailable(ref m) if m == "backend down"));
    assert_eq!(e.to_string(), "service unavailable: backend down");
}

#[test]
fn internal_constructor_sets_message() {
    let e = CredStoreError::internal("unexpected state");
    assert!(matches!(e, CredStoreError::Internal(ref m) if m == "unexpected state"));
    assert_eq!(e.to_string(), "internal error: unexpected state");
}
