use super::*;

#[test]
fn test_error_constructors() {
    let err = TypesError::not_ready();
    assert!(err.is_not_ready());

    let err = TypesError::registration_failed("schema invalid");
    assert!(err.is_registration_failed());
    assert!(err.to_string().contains("schema invalid"));

    let err = TypesError::internal("unexpected");
    assert!(matches!(err, TypesError::Internal(_)));
}

#[test]
fn test_error_display() {
    let err = TypesError::NotReady;
    assert_eq!(err.to_string(), "Core types not ready");

    let err = TypesError::RegistrationFailed("failed to parse".to_owned());
    assert_eq!(err.to_string(), "Registration failed: failed to parse");

    let err = TypesError::Internal("database error".to_owned());
    assert_eq!(err.to_string(), "Internal error: database error");
}
