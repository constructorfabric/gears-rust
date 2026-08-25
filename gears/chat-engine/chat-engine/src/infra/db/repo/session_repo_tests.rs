use super::*;

use crate::domain::service::test_support::{inmem_db, session_repo};

fn new_session(tenant: Uuid, user: Uuid) -> NewSession {
    let now = OffsetDateTime::now_utc();
    NewSession {
        session_id: Uuid::new_v4(),
        tenant_id: tenant.to_string(),
        user_id: user.to_string(),
        client_id: None,
        session_type_id: None,
        metadata: None,
        created_at: now,
        updated_at: now,
    }
}

// insert_scoped must validate the row against the PDP-derived CREATE scope
// (scope_with_model), not skip validation (scope_unchecked). A constrained
// decision whose owner predicate the row does NOT satisfy is `Denied` →
// Forbidden, never a silent cross-scope write.
// @cpt-cf-chat-engine-interface-pep
#[tokio::test]
async fn insert_scoped_rejects_row_outside_constrained_scope() {
    let db = inmem_db().await;
    let repo = session_repo(&db);

    // PDP allows CREATE but constrains ownership to a DIFFERENT tenant than the
    // row being inserted.
    let scope = AccessScope::for_tenant(Uuid::new_v4());
    let err = repo
        .insert_scoped(&scope, new_session(Uuid::new_v4(), Uuid::new_v4()))
        .await
        .expect_err("insert must be denied when the row violates the scope");
    assert!(
        matches!(err, ChatEngineError::Forbidden { .. }),
        "Expected Forbidden for a row outside the constrained scope, got: {err:?}"
    );
}

// The validated path must not over-restrict: a row whose owner satisfies the
// scope's constraint is persisted normally (guards the Uuid-vs-Uuid predicate
// comparison the fix relies on).
// @cpt-cf-chat-engine-interface-pep
#[tokio::test]
async fn insert_scoped_persists_row_satisfying_constrained_scope() {
    let db = inmem_db().await;
    let repo = session_repo(&db);

    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let inserted = repo
        .insert_scoped(&scope, new_session(tenant, Uuid::new_v4()))
        .await
        .expect("insert must succeed when the row satisfies the scope");

    let read_back = repo
        .find_by_id_scoped(&scope, inserted.session_id)
        .await
        .expect("scoped read must not error")
        .expect("the in-scope row must be readable back");
    assert_eq!(read_back.session_id, inserted.session_id);
}

// An unconstrained scope (PDP allowed with no row filter) leaves the insert
// unrestricted — the fix is a no-op on this path.
// @cpt-cf-chat-engine-interface-pep
#[tokio::test]
async fn insert_scoped_allows_any_row_under_unconstrained_scope() {
    let db = inmem_db().await;
    let repo = session_repo(&db);

    let scope = AccessScope::allow_all();
    repo.insert_scoped(&scope, new_session(Uuid::new_v4(), Uuid::new_v4()))
        .await
        .expect("unconstrained scope must not block the insert");
}
