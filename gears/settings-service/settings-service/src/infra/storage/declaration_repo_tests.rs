// Created: 2026-10-16 by Virtuozzo International GmbH
//! The declaration repository's hard delete over an in-memory database: the
//! version guard, and what the foreign keys' cascade takes with the row.

use serde_json::json;
use time::Duration;
use toolkit_security::AccessScope;

use super::DeclarationRepo;
use crate::domain::declaration::DeclarationRepository;
use crate::domain::error::DomainError;
use crate::domain::resolution::scope_class;
use crate::domain::secrets::pending::PendingSecretRepository;
use crate::domain::value::ValueRepository;
use crate::infra::storage::pending_secret_repo::PendingSecretRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::test_support::ResolutionHarness;

#[tokio::test]
async fn a_delete_at_a_stale_version_is_refused_and_removes_nothing() {
    let h = ResolutionHarness::new().await;
    let id = h.declare("one", scope_class::LOCAL, json!(true)).await;
    h.set(id, h.tree.a, json!(false)).await;
    let conn = h.db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let current = h.load_declaration(id).await;

    let refused = DeclarationRepo
        .delete(&conn, &scope, id, current.updated_at - Duration::seconds(1))
        .await;

    assert!(
        matches!(refused, Err(DomainError::PreconditionFailed { .. })),
        "{refused:?}"
    );
    assert_eq!(
        h.load_declaration(id).await.id,
        id,
        "the row is still there"
    );
    assert_eq!(
        ValueRepo
            .find_all(&conn, &scope, id)
            .await
            .expect("rows")
            .len(),
        1
    );
}

#[tokio::test]
async fn a_delete_at_the_current_version_removes_the_row_and_what_cascades() {
    let h = ResolutionHarness::new().await;
    let id = h.declare("one", scope_class::LOCAL, json!(true)).await;
    h.set(id, h.tree.a, json!(false)).await;
    h.stage_secret(id, h.tree.b, "staged-ref").await;
    let conn = h.db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let current = h.load_declaration(id).await;

    DeclarationRepo
        .delete(&conn, &scope, id, current.updated_at)
        .await
        .expect("deleted");

    assert!(
        DeclarationRepo
            .find_by_key(&conn, &scope, &current.key)
            .await
            .expect("lookup")
            .is_none()
    );
    assert!(
        ValueRepo
            .find_all(&conn, &scope, id)
            .await
            .expect("rows")
            .is_empty()
    );
    assert!(
        PendingSecretRepo
            .list_for_declaration(&conn, id)
            .await
            .expect("rows")
            .is_empty()
    );
}
