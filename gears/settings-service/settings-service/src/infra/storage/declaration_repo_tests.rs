//! The declaration repository's narrowed-browse batch.

use serde_json::json;
use toolkit_odata::{CursorV1, ODataQuery};
use toolkit_security::AccessScope;

use super::DeclarationRepo;
use crate::domain::category::visibility::DomainVisibility;
use crate::domain::declaration::{BrowseNarrowing, DeclarationRepository};
use crate::domain::search::{Corpus, Needle};
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
