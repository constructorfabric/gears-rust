use std::sync::Arc;

use modkit_security::SecurityContext;
use modkit_security::access_scope::pep_properties;
use usage_collector_sdk::UsageCollectorError;
use usage_collector_sdk::authz::{USAGE_RECORD, actions};
use uuid::Uuid;

use super::authorize_and_compile_scope;
use crate::test_support::{
    DenyAuthZ, InternalErrorAuthZ, MultiConstraintAuthZ, NetworkErrorAuthZ, SingleConstraintAuthZ,
    TimeoutAuthZ,
};

fn test_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .token_scopes(vec!["*".to_owned()])
        .build()
        .expect("valid SecurityContext")
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// PDP returns `Err(Denied)` → `PermissionDenied`.
#[tokio::test]
async fn denied_pdp_returns_permission_denied() {
    let ctx = test_ctx();
    let authz = Arc::new(DenyAuthZ);

    let result = authorize_and_compile_scope(&ctx, authz, &USAGE_RECORD, actions::LIST).await;

    assert!(
        matches!(result, Err(UsageCollectorError::PermissionDenied { .. })),
        "expected PermissionDenied, got {result:?}"
    );
}

/// PDP `ServiceUnavailable` (network error) → `PermissionDenied` (fail-closed).
#[tokio::test]
async fn network_error_fails_closed_with_permission_denied() {
    let ctx = test_ctx();
    let authz = Arc::new(NetworkErrorAuthZ);

    let result = authorize_and_compile_scope(&ctx, authz, &USAGE_RECORD, actions::LIST).await;

    assert!(
        matches!(result, Err(UsageCollectorError::PermissionDenied { .. })),
        "expected PermissionDenied (fail-closed), got {result:?}"
    );
}

/// PDP `NoPluginAvailable` (timeout / not-ready) → `PermissionDenied` (fail-closed).
#[tokio::test]
async fn timeout_fails_closed_with_permission_denied() {
    let ctx = test_ctx();
    let authz = Arc::new(TimeoutAuthZ);

    let result = authorize_and_compile_scope(&ctx, authz, &USAGE_RECORD, actions::LIST).await;

    assert!(
        matches!(result, Err(UsageCollectorError::PermissionDenied { .. })),
        "expected PermissionDenied (fail-closed), got {result:?}"
    );
}

/// PDP `Internal` error → `PermissionDenied` (fail-closed).
#[tokio::test]
async fn internal_error_fails_closed_with_permission_denied() {
    let ctx = test_ctx();
    let authz = Arc::new(InternalErrorAuthZ);

    let result = authorize_and_compile_scope(&ctx, authz, &USAGE_RECORD, actions::LIST).await;

    assert!(
        matches!(result, Err(UsageCollectorError::PermissionDenied { .. })),
        "expected PermissionDenied (fail-closed), got {result:?}"
    );
}

/// Single constraint → `Ok(AccessScope)` compiled correctly.
#[tokio::test]
async fn single_constraint_compiles_into_one_group() {
    let tenant_id = Uuid::new_v4();
    let ctx = test_ctx();
    let authz = Arc::new(SingleConstraintAuthZ { tenant_id });

    let result = authorize_and_compile_scope(&ctx, authz, &USAGE_RECORD, actions::LIST).await;

    let scope = result.expect("expected Ok(AccessScope)");
    assert!(
        !scope.is_deny_all(),
        "scope must not be deny-all for a valid single constraint"
    );
    assert_eq!(
        scope.constraints().len(),
        1,
        "expected exactly one constraint group"
    );
    assert!(
        scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant_id),
        "scope must contain the tenant_id from the single PDP constraint"
    );
}

/// Multiple constraint groups → OR-of-ANDs preserved (no flattening).
///
/// `compile_to_access_scope` MUST NOT flatten multiple constraint groups into a
/// single AND list — flattening would widen the scope and violate
/// `cpt-cf-usage-collector-constraint-or-of-ands-preservation`.
#[tokio::test]
async fn two_constraint_groups_preserve_or_of_ands_disjunction() {
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let ctx = test_ctx();
    let authz = Arc::new(MultiConstraintAuthZ { tenant_a, tenant_b });

    let result = authorize_and_compile_scope(&ctx, authz, &USAGE_RECORD, actions::LIST).await;

    let scope = result.expect("expected Ok(AccessScope)");
    assert!(
        !scope.is_deny_all(),
        "scope must not be deny-all with two valid constraint groups"
    );
    assert_eq!(
        scope.constraints().len(),
        2,
        "both constraint groups must be preserved (OR-of-ANDs; flattening is a security violation)"
    );
    assert!(
        scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant_a),
        "scope must contain tenant_a"
    );
    assert!(
        scope.contains_uuid(pep_properties::OWNER_TENANT_ID, tenant_b),
        "scope must contain tenant_b"
    );
}
