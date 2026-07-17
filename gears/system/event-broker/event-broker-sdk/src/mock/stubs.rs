use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Build a pass-through `SecurityContext` for tests.
/// Uses `SecurityContext::anonymous()` as a base - sufficient for the mock
/// since it does no real authz/tenant resolution.
pub fn test_ctx_for_tenant(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap_or_else(|_| SecurityContext::anonymous())
}

/// Build a `SecurityContext` with a random tenant - useful for isolation between test cases.
pub fn random_ctx() -> SecurityContext {
    test_ctx_for_tenant(Uuid::new_v4())
}
