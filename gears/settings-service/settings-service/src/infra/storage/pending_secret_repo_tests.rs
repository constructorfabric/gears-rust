//! The staged-secret repository's per-declaration read over an in-memory
//! database.

use serde_json::json;

use super::PendingSecretRepo;
use crate::domain::resolution::scope_class;
use crate::domain::secrets::pending::PendingSecretRepository;
use crate::test_support::ResolutionHarness;

#[tokio::test]
async fn every_staged_row_of_the_declaration_is_listed_and_no_other() {
    let h = ResolutionHarness::new().await;
    let one = h.declare("one", scope_class::LOCAL, json!(true)).await;
    let other = h.declare("other", scope_class::LOCAL, json!(true)).await;
    h.stage_secret(one, h.tree.a, "ref-a").await;
    h.stage_secret(one, h.tree.b, "ref-b").await;
    h.stage_secret(other, h.tree.a, "ref-other").await;
    let conn = h.db.conn().expect("connection");

    let mut refs: Vec<String> = PendingSecretRepo
        .list_for_declaration(&conn, one)
        .await
        .expect("rows")
        .into_iter()
        .map(|row| row.secret_ref)
        .collect();
    refs.sort();

    assert_eq!(refs, vec!["ref-a".to_owned(), "ref-b".to_owned()]);
}

#[tokio::test]
async fn the_references_another_declaration_stages_count_and_its_own_do_not() {
    let h = ResolutionHarness::new().await;
    let one = h.declare("one", scope_class::LOCAL, json!(true)).await;
    let other = h.declare("other", scope_class::LOCAL, json!(true)).await;
    h.stage_secret(one, h.tree.a, "own-only").await;
    h.stage_secret(one, h.tree.b, "shared").await;
    h.stage_secret(other, h.tree.b, "shared").await;
    h.stage_secret(other, h.tree.c, "other-only").await;
    let conn = h.db.conn().expect("connection");
    let asked: Vec<String> = ["own-only", "shared", "nobody-has-it"]
        .into_iter()
        .map(str::to_owned)
        .collect();

    let held = PendingSecretRepo
        .references_held_by_others(&conn, one, &asked)
        .await
        .expect("held");

    assert_eq!(
        held.into_iter().collect::<Vec<_>>(),
        vec!["shared".to_owned()]
    );
}

#[tokio::test]
async fn a_set_of_references_larger_than_one_chunk_is_answered_in_full() {
    let h = ResolutionHarness::new().await;
    let one = h.declare("one", scope_class::LOCAL, json!(true)).await;
    let other = h.declare("other", scope_class::LOCAL, json!(true)).await;
    h.stage_secret(other, h.tree.a, "held-by-the-other").await;
    let conn = h.db.conn().expect("connection");
    let mut asked: Vec<String> = (0..1_200).map(|i| format!("nobody-{i}")).collect();
    asked.push("held-by-the-other".to_owned());

    let held = PendingSecretRepo
        .references_held_by_others(&conn, one, &asked)
        .await
        .expect("held");

    assert_eq!(held.len(), 1);
}
