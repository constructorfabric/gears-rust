//! The declaration repository over an in-memory database: the narrowed-browse
//! batch, and the hard delete (the version guard, and what the foreign keys'
//! cascade takes with the row).

use serde_json::json;
use time::Duration;
use toolkit_odata::{CursorV1, ODataQuery};
use toolkit_security::AccessScope;

use super::DeclarationRepo;
use crate::domain::category::visibility::DomainVisibility;
use crate::domain::declaration::{BrowseNarrowing, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::resolution::scope_class;
use crate::domain::search::{Corpus, Needle};
use crate::domain::secrets::pending::PendingSecretRepository;
use crate::domain::value::ValueRepository;
use crate::infra::storage::pending_secret_repo::PendingSecretRepo;
use crate::infra::storage::value_repo::ValueRepo;
use crate::test_support::ResolutionHarness;

#[tokio::test]
async fn the_last_position_of_a_batch_is_the_cursor_the_toolkit_mints_for_the_same_page() {
    let h = ResolutionHarness::new().await;
    for i in 0..9 {
        h.declare(&format!("pos_{i}"), "cascading", json!(true))
            .await;
    }
    let conn = h.db.conn().expect("connection");
    let scope = AccessScope::allow_all();
    let needle = Needle::parse("pos").expect("needle");
    let narrowing = BrowseNarrowing {
        needle: &needle,
        corpus: Corpus::Public,
        postgres: false,
    };
    let query = |limit, cursor: Option<CursorV1>| ODataQuery {
        limit: Some(limit),
        cursor,
        filter_hash: Some("bound".to_owned()),
        ..ODataQuery::default()
    };
    let list = |limit, cursor| {
        let q = query(limit, cursor);
        let conn = &conn;
        let scope = &scope;
        async move {
            DeclarationRepo
                .list(conn, scope, &DomainVisibility::Unrestricted, &[], None, &q)
                .await
                .expect("list")
        }
    };
    let batch = |limit, cursor| {
        let q = query(limit, cursor);
        let conn = &conn;
        let scope = &scope;
        let narrowing = &narrowing;
        async move {
            DeclarationRepo
                .list_candidates(
                    conn,
                    scope,
                    &DomainVisibility::Unrestricted,
                    &[],
                    None,
                    &q,
                    narrowing,
                )
                .await
                .expect("batch")
        }
    };

    // Forward from the start, and again from a cursor, whose order the cursor
    // carries.
    let plain = list(4, None).await;
    let candidates = batch(4, None).await;
    assert_eq!(
        candidates
            .items
            .iter()
            .map(|c| &c.declaration.key)
            .collect::<Vec<_>>(),
        plain.items.iter().map(|d| &d.key).collect::<Vec<_>>()
    );
    let end = candidates.items.last().expect("rows").position.encode();
    assert_eq!(end.ok(), plain.page_info.next_cursor);
    assert!(candidates.items.iter().all(|c| c.text_matched));

    let cursor = CursorV1::decode(&plain.page_info.next_cursor.expect("next")).expect("decodes");
    let second = list(4, Some(cursor.clone())).await;
    let second_batch = batch(4, Some(cursor)).await;
    let second_end = second_batch.items.last().expect("rows").position.encode();
    assert_eq!(second_end.ok(), second.page_info.next_cursor);
    // The cursor before the first row is the toolkit's `prev_cursor`.
    let first = &second_batch.items[0].position;
    let before = CursorV1 {
        d: "bwd".to_owned(),
        ..first.clone()
    }
    .encode()
    .ok();
    assert_eq!(before, second.page_info.prev_cursor);
}

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
