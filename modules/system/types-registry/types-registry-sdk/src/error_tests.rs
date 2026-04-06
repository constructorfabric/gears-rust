use super::*;

#[test]
fn test_error_constructors() {
    let err = TypesRegistryError::invalid_gts_id("missing vendor");
    assert!(err.is_invalid_gts_id());
    assert!(err.to_string().contains("missing vendor"));

    let err = TypesRegistryError::not_found("gts.acme.core.events.test.v1~");
    assert!(err.is_not_found());

    let err = TypesRegistryError::already_exists("gts.acme.core.events.test.v1~");
    assert!(err.is_already_exists());

    let err = TypesRegistryError::validation_failed("schema invalid");
    assert!(err.is_validation_failed());

    let err = TypesRegistryError::not_in_ready_mode();
    assert!(matches!(err, TypesRegistryError::NotInReadyMode));

    let err = TypesRegistryError::internal("database error");
    assert!(matches!(err, TypesRegistryError::Internal(_)));
}

#[test]
fn test_error_display() {
    let err = TypesRegistryError::InvalidGtsId("bad format".to_owned());
    assert_eq!(err.to_string(), "Invalid GTS ID: bad format");

    let err = TypesRegistryError::NotFound("gts.x.core.events.test.v1~".to_owned());
    assert_eq!(
        err.to_string(),
        "Entity not found: gts.x.core.events.test.v1~"
    );

    let err = TypesRegistryError::AlreadyExists("gts.x.core.events.test.v1~".to_owned());
    assert_eq!(
        err.to_string(),
        "Entity already exists: gts.x.core.events.test.v1~"
    );

    let err = TypesRegistryError::ValidationFailed("missing required field".to_owned());
    assert_eq!(err.to_string(), "Validation failed: missing required field");

    let err = TypesRegistryError::NotInReadyMode;
    assert_eq!(err.to_string(), "Not in ready mode");

    let err = TypesRegistryError::Internal("unexpected".to_owned());
    assert_eq!(err.to_string(), "Internal error: unexpected");
}
