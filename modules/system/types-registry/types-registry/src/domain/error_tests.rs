use super::*;

#[test]
fn test_error_constructors() {
    let err = DomainError::invalid_gts_id("missing vendor");
    assert!(matches!(err, DomainError::InvalidGtsId(_)));

    let err = DomainError::not_found("gts.acme.core.events.test.v1~");
    assert!(matches!(err, DomainError::NotFound(_)));

    let err = DomainError::already_exists("gts.acme.core.events.test.v1~");
    assert!(matches!(err, DomainError::AlreadyExists(_)));

    let err = DomainError::validation_failed("schema invalid");
    assert!(matches!(err, DomainError::ValidationFailed(_)));
}

#[test]
fn test_domain_to_sdk_error_conversion() {
    let domain_err = DomainError::not_found("gts.x.core.events.test.v1~");
    let sdk_err: TypesRegistryError = domain_err.into();
    assert!(sdk_err.is_not_found());

    let domain_err = DomainError::already_exists("gts.x.core.events.test.v1~");
    let sdk_err: TypesRegistryError = domain_err.into();
    assert!(sdk_err.is_already_exists());

    let domain_err = DomainError::validation_failed("bad schema");
    let sdk_err: TypesRegistryError = domain_err.into();
    assert!(sdk_err.is_validation_failed());

    let domain_err = DomainError::invalid_gts_id("bad format");
    let sdk_err: TypesRegistryError = domain_err.into();
    assert!(sdk_err.is_invalid_gts_id());
}

#[test]
fn test_domain_to_sdk_error_not_in_ready_mode() {
    let domain_err = DomainError::NotInReadyMode;
    let sdk_err: TypesRegistryError = domain_err.into();
    assert!(matches!(sdk_err, TypesRegistryError::NotInReadyMode));
}

#[test]
fn test_domain_to_sdk_error_ready_commit_failed() {
    let errors = vec![
        ValidationError::new("gts.test1~", "error1"),
        ValidationError::new("gts.test2~", "error2"),
    ];
    let domain_err = DomainError::ReadyCommitFailed(errors);
    let sdk_err: TypesRegistryError = domain_err.into();
    assert!(sdk_err.is_validation_failed());
}

#[test]
fn test_domain_to_sdk_error_internal() {
    let domain_err = DomainError::Internal(anyhow::anyhow!("test error"));
    let sdk_err: TypesRegistryError = domain_err.into();
    assert!(matches!(sdk_err, TypesRegistryError::Internal(_)));
}

#[test]
fn test_error_display() {
    let err = DomainError::InvalidGtsId("bad format".to_owned());
    assert_eq!(err.to_string(), "Invalid GTS ID: bad format");

    let err = DomainError::NotFound("gts.x.core.events.test.v1~".to_owned());
    assert_eq!(
        err.to_string(),
        "Entity not found: gts.x.core.events.test.v1~"
    );

    let err = DomainError::AlreadyExists("gts.x.core.events.test.v1~".to_owned());
    assert_eq!(
        err.to_string(),
        "Entity already exists: gts.x.core.events.test.v1~"
    );

    let err = DomainError::ValidationFailed("schema invalid".to_owned());
    assert_eq!(err.to_string(), "Validation failed: schema invalid");

    let err = DomainError::NotInReadyMode;
    assert_eq!(err.to_string(), "Not in ready mode");

    let err = DomainError::ReadyCommitFailed(vec![
        ValidationError::new("gts.test1~", "error1"),
        ValidationError::new("gts.test2~", "error2"),
        ValidationError::new("gts.test3~", "error3"),
    ]);
    assert_eq!(err.to_string(), "Ready commit failed with 3 errors");
}

#[test]
fn test_internal_error_from_anyhow() {
    let anyhow_err = anyhow::anyhow!("test error");
    let domain_err: DomainError = anyhow_err.into();
    assert!(matches!(domain_err, DomainError::Internal(_)));
}
