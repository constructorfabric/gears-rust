// Most of the repo_impl logic is exercised through integration tests
// against a real DB (Phase 3 owns cross-backend coverage). These unit
// tests cover the pure helpers only.
use super::*;
use account_management_sdk::error::AccountManagementError;
use time::OffsetDateTime;

#[test]
fn entity_to_model_rejects_unknown_status() {
    let row = tenants::Model {
        id: Uuid::nil(),
        parent_id: None,
        name: "x".into(),
        status: 42,
        self_managed: false,
        tenant_type_uuid: Uuid::nil(),
        depth: 0,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
        deleted_at: None,
        retention_window_secs: None,
        claimed_by: None,
        claimed_at: None,
        terminal_failure_at: None,
    };
    let err = entity_to_model(row).expect_err("unknown status");
    assert!(matches!(err, DomainError::Internal { .. }));
}

#[test]
fn entity_to_model_rejects_negative_depth() {
    let row = tenants::Model {
        id: Uuid::nil(),
        parent_id: None,
        name: "x".into(),
        status: 1,
        self_managed: false,
        tenant_type_uuid: Uuid::nil(),
        depth: -1,
        created_at: OffsetDateTime::UNIX_EPOCH,
        updated_at: OffsetDateTime::UNIX_EPOCH,
        deleted_at: None,
        retention_window_secs: None,
        claimed_by: None,
        claimed_at: None,
        terminal_failure_at: None,
    };
    let err = entity_to_model(row).expect_err("negative depth");
    assert!(matches!(err, DomainError::Internal { .. }));
}

/// `ScopeError::Db` MUST be lifted into the retry-aware `TxError::Db`
/// variant so [`with_serializable_retry`]'s `extract_db_err` can hand
/// the raw `DbErr` to `is_retryable_contention`. After retry exhaustion,
/// the helper translates the surviving `DbErr` into a typed
/// `DomainError` via `classify_db_err_to_domain` — domain code never
/// sees a `sea_orm::DbErr`.
#[test]
fn map_scope_to_tx_lifts_db_err_into_tx_db_variant() {
    use modkit_db::secure::ScopeError;
    use sea_orm::{DbErr, RuntimeErr};
    let scope_err = ScopeError::Db(DbErr::Exec(RuntimeErr::Internal(
        "error returned from database: 40001: could not serialize access".to_owned(),
    )));
    let err = map_scope_to_tx(scope_err);
    assert!(matches!(err, TxError::Db(_)));
    assert!(err.db_err().is_some());
}

/// `ScopeError::TenantNotInScope` is a typed cross-tenant denial — it
/// MUST always map to `DomainError::CrossTenantDenied`, both inside
/// retry bodies (via `map_scope_to_tx`) and outside them (via
/// `map_scope_err`). The boundary mapping then converts that to
/// `CanonicalError::PermissionDenied` (HTTP 403).
#[test]
fn map_scope_err_preserves_tenant_not_in_scope_routing() {
    use modkit_db::secure::ScopeError;
    let scope_err = ScopeError::TenantNotInScope {
        tenant_id: Uuid::nil(),
    };
    let err = map_scope_err(scope_err);
    assert!(matches!(err, DomainError::CrossTenantDenied { .. }));
    let ame: AccountManagementError = err.into();
    assert!(ame.is_permission_denied());
}

#[test]
fn map_scope_to_tx_preserves_tenant_not_in_scope_routing() {
    use modkit_db::secure::ScopeError;
    let scope_err = ScopeError::TenantNotInScope {
        tenant_id: Uuid::nil(),
    };
    let err = map_scope_to_tx(scope_err);
    let TxError::Domain(domain) = err else {
        panic!("expected TxError::Domain");
    };
    assert!(matches!(domain, DomainError::CrossTenantDenied { .. }));
}
