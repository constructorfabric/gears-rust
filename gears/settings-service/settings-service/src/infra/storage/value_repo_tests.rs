// Created: 2026-09-24 by Virtuozzo International GmbH
//! The value repository over an in-memory database: what a row-level
//! operation answers when the row it names is not there.

use serde_json::json;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::ValueRepo;
use crate::domain::error::DomainError;
use crate::domain::resolution::scope_class;
use crate::domain::value::ValueRepository;
use crate::test_support::ResolutionHarness;

#[tokio::test]
async fn flagging_a_row_that_is_not_there_is_not_found_not_success() {
    let h = ResolutionHarness::new().await;
    let declaration = h.declare("one", scope_class::LOCAL, json!(1)).await;
    h.set(declaration, h.tree.a, json!(2)).await;
    let conn = h.db.conn().expect("connection");
    let scope = AccessScope::allow_all();

    // A row that vanished under the caller — or was never in its scope —
    // matched nothing; the caller must not be told the flag was applied.
    let missing = ValueRepo
        .flag(&conn, &scope, Uuid::new_v4(), Some("stale".to_owned()))
        .await;
    assert!(
        matches!(missing, Err(DomainError::NotFound { resource: "value" })),
        "{missing:?}"
    );

    // The row that is there takes the flag, and says why.
    let row = ValueRepo
        .find_all(&conn, &scope, declaration)
        .await
        .expect("rows")
        .remove(0);
    ValueRepo
        .flag(
            &conn,
            &scope,
            row.id,
            Some("no longer validates".to_owned()),
        )
        .await
        .expect("flagged");
    let row = ValueRepo
        .find_all(&conn, &scope, declaration)
        .await
        .expect("rows")
        .remove(0);
    assert!(row.needs_review);
    assert_eq!(
        row.needs_review_detail.as_deref(),
        Some("no longer validates")
    );
}

#[tokio::test]
async fn the_flagged_listing_fetches_one_past_its_bound_and_no_more() {
    // The needs-review listing spans a page of declarations across a subtree;
    // unbounded, a broad migration makes it a million-row read. It fetches one
    // row past the bound so the caller can tell a full answer from a cut one.
    let h = ResolutionHarness::new().await;
    let declaration = h.declare("flagged", scope_class::LOCAL, json!(1)).await;
    let tenants = [h.tree.root, h.tree.a, h.tree.b, h.tree.c];
    for tenant in tenants {
        h.set_flagged(declaration, tenant, json!(2)).await;
    }
    let conn = h.db.conn().expect("connection");
    let rows = ValueRepo
        .list_flagged(
            &conn,
            &AccessScope::allow_all(),
            &[declaration],
            &tenants,
            2,
        )
        .await
        .expect("rows");
    assert_eq!(
        rows.len(),
        3,
        "the bound of two, plus the one that says there is more"
    );
}

#[tokio::test]
async fn the_secret_references_of_a_declaration_include_every_subject_and_tenant() {
    // The collect behind a purge: the cascade removes every row whatever the
    // caller's scope, so it must see the subject rows and the other tenants'.
    let h = ResolutionHarness::new().await;
    let id = h.declare("one", scope_class::LOCAL, json!(true)).await;
    let other = h.declare("other", scope_class::LOCAL, json!(true)).await;
    h.set(id, h.tree.root, json!(false)).await;
    h.set_secret(id, h.tree.a, "ref-a").await;
    h.set_secret(id, h.tree.b, "ref-b").await;
    h.set_subject_secret(id, h.tree.c, "ref-subject").await;
    h.set_secret(other, h.tree.a, "ref-other").await;
    let conn = h.db.conn().expect("connection");

    let mut held = ValueRepo.secret_refs(&conn, id).await.expect("refs");
    held.sort();

    let mut expected = vec![
        (h.tree.a, "ref-a".to_owned()),
        (h.tree.b, "ref-b".to_owned()),
        (h.tree.c, "ref-subject".to_owned()),
    ];
    expected.sort();
    assert_eq!(held, expected, "no inline row, no other declaration's");
}

#[tokio::test]
async fn the_references_another_declaration_holds_count_whether_it_holds_them_as_values_or_not_at_all()
 {
    // What keeps a purge from releasing an entry an evolution's successor
    // still names: the other declaration's rows are asked for, never the
    // declaration's own.
    let h = ResolutionHarness::new().await;
    let id = h.declare("one", scope_class::LOCAL, json!(true)).await;
    let other = h.declare("other", scope_class::LOCAL, json!(true)).await;
    h.set_secret(id, h.tree.a, "own-only").await;
    h.set_secret(id, h.tree.b, "shared").await;
    h.set_secret(other, h.tree.b, "shared").await;
    h.set_subject_secret(other, h.tree.c, "other-subject").await;
    h.set_secret(other, h.tree.a, "other-only").await;
    let conn = h.db.conn().expect("connection");
    let asked: Vec<String> = ["own-only", "shared", "other-subject", "nobody-has-it"]
        .into_iter()
        .map(str::to_owned)
        .collect();

    let held = ValueRepo
        .references_held_by_others(&conn, id, &asked)
        .await
        .expect("held");

    let mut held: Vec<_> = held.into_iter().collect();
    held.sort();
    assert_eq!(held, vec!["other-subject".to_owned(), "shared".to_owned()]);
    assert!(
        ValueRepo
            .references_held_by_others(&conn, id, &[])
            .await
            .expect("empty")
            .is_empty()
    );
}

#[tokio::test]
async fn a_set_of_references_larger_than_one_chunk_is_answered_in_full() {
    let h = ResolutionHarness::new().await;
    let id = h.declare("one", scope_class::LOCAL, json!(true)).await;
    let other = h.declare("other", scope_class::LOCAL, json!(true)).await;
    h.set_secret(other, h.tree.a, "held-by-the-other").await;
    let conn = h.db.conn().expect("connection");
    // The one that is held sits in the last chunk.
    let mut asked: Vec<String> = (0..1_200).map(|i| format!("nobody-{i}")).collect();
    asked.push("held-by-the-other".to_owned());

    let held = ValueRepo
        .references_held_by_others(&conn, id, &asked)
        .await
        .expect("held");

    assert_eq!(held.len(), 1);
    assert!(held.contains("held-by-the-other"));
}
