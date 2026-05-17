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

// ── Query-API canonical-error contracts (Feature 3) ──────────────────────

#[test]
fn usage_record_error_permission_denied_maps_to_canonical_variant() {
    // §3 gateway authz flow: when the PDP denies a query (or returns no
    // constraints), the gateway emits the canonical `PermissionDenied`
    // variant via `UsageRecordError::permission_denied()`. This pins the
    // canonical mapping the spec calls "AuthorizationFailed" historically
    // (v3.1) and now refers to as `PermissionDenied` (v3.2+).
    let err = UsageRecordError::permission_denied()
        .with_reason("PDP denied query")
        .create();
    assert!(
        matches!(err, CanonicalError::PermissionDenied { .. }),
        "permission_denied() must produce CanonicalError::PermissionDenied, got: {err:?}"
    );
    assert_eq!(err.status_code(), 403);
    assert_eq!(
        err.resource_type(),
        Some("gts.cf.core.usage.record.v1~"),
        "PermissionDenied must carry the usage-record GTS prefix"
    );
}

#[test]
fn usage_record_error_resource_exhausted_query_result_too_large() {
    // §3 `inst-plugin-contract-3`: when `query_aggregated` would exceed
    // `MAX_AGG_ROWS`, the plugin emits the canonical `ResourceExhausted`
    // variant built via `UsageRecordError::resource_exhausted(detail)` with
    // the documented detail string. This pins both the canonical variant
    // (formerly named `QueryResultTooLarge` in v3.1) and the detail string
    // gateway operators see in logs.
    let err = UsageRecordError::resource_exhausted("query result too large")
        .with_resource("query")
        .with_quota_violation("query.rows", "result set exceeds MAX_AGG_ROWS")
        .create();
    assert!(
        matches!(err, CanonicalError::ResourceExhausted { .. }),
        "resource_exhausted() must produce CanonicalError::ResourceExhausted, got: {err:?}"
    );
    assert_eq!(err.status_code(), 429);
    assert_eq!(
        err.detail(),
        "query result too large",
        "resource_exhausted detail string must round-trip verbatim"
    );
    assert_eq!(
        err.resource_type(),
        Some("gts.cf.core.usage.record.v1~"),
        "ResourceExhausted must carry the usage-record GTS prefix"
    );
}

#[test]
fn usage_record_error_display_strings_distinct_for_authz_and_size_limit() {
    // Display strings carry no PDP detail or resource identifiers — only the
    // canonical-status tag and the explicit detail message — so they are safe
    // to log even though the underlying records carry sensitive identifiers.
    let permission = UsageRecordError::permission_denied()
        .with_reason("PDP denied query")
        .create();
    let too_large = UsageRecordError::resource_exhausted("query result too large")
        .with_resource("query")
        .with_quota_violation("query.rows", "result set exceeds MAX_AGG_ROWS")
        .create();
    let perm_str = permission.to_string();
    let too_large_str = too_large.to_string();
    assert_ne!(
        perm_str, too_large_str,
        "permission_denied and resource_exhausted must render distinct Display strings"
    );
    assert!(
        too_large_str.contains("query result too large"),
        "ResourceExhausted Display must include the configured detail, got: {too_large_str}"
    );
}
