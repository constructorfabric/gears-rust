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
