use super::*;

#[test]
fn usage_record_error_has_correct_resource_type_and_kind() {
    let err = UsageRecordError::not_found("usage record not found")
        .with_resource("rec-1")
        .create();
    assert_eq!(
        err.resource_type(),
        Some("gts.cf.core.usage.record.v1~"),
        "UsageRecordError must carry the usage record GTS prefix"
    );
    assert!(
        matches!(err, CanonicalError::NotFound { .. }),
        "not_found() must produce CanonicalError::NotFound, got: {err:?}"
    );
}

#[test]
fn module_config_error_has_correct_resource_type_and_kind() {
    let err = ModuleConfigError::not_found("module config not found")
        .with_resource("test-module")
        .create();
    assert_eq!(
        err.resource_type(),
        Some("gts.cf.core.usage.module_config.v1~"),
        "ModuleConfigError must carry the module-config GTS prefix"
    );
    assert!(
        matches!(err, CanonicalError::NotFound { .. }),
        "not_found() must produce CanonicalError::NotFound, got: {err:?}"
    );
}
