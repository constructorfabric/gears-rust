// Created: 2026-07-27 by Constructor Tech
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]
//! Live end-to-end wiring check for [`common::query_recorder::QueryRecorder`],
//! mirroring `resource_group`'s own `tests/query_recorder_test.rs`
//! (`audit/rg-db-behavior`).
//!
//! Pure-logic coverage of the recorder itself lives in the inline
//! `#[cfg(test)] mod tests` at the bottom of `tests/common/query_recorder.rs`.
//! This file only proves the wiring: attaching the recorder via
//! `common::test_db_with_recorder()` and driving it against a real SQLite
//! connection actually observes statements and tags transaction membership
//! correctly, for *this* gear's own service layer.

mod common;

use uuid::Uuid;

#[tokio::test]
async fn recorder_observes_statements_and_tags_tx_membership() {
    let (db, rec) = common::test_db_with_recorder().await;
    let (svc, _msvc) = common::make_services(&db);
    let tenant_id = Uuid::now_v7();
    let ctx = common::make_ctx(tenant_id);

    // A plain read: get_file on a nonexistent id runs on a bare connection --
    // NotFound is fine, we only care that a SELECT ran and nothing wrote.
    svc.get_file(&ctx, Uuid::now_v7()).await.ok();

    let after_read = rec.total();
    assert!(
        after_read > 0,
        "expected at least one captured statement after a read"
    );
    assert!(
        rec.writes_outside_tx().is_empty(),
        "a read-only call must not produce any write statements:\n{}",
        rec.dump()
    );

    // A write wrapped in `db.transaction_ref_mapped(...)`: create_file's
    // INSERT statements must be tagged in_tx = true.
    let ticket = svc
        .create_file(&ctx, common::new_file(), None, false)
        .await
        .expect("create_file should succeed");
    assert!(
        ticket.upload_url.starts_with("http://sidecar.test"),
        "sanity: the ticket should carry a signed upload URL"
    );

    let events = rec.events();
    assert!(
        events.len() > after_read,
        "create_file should have produced additional statements"
    );
    let inserts: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(e.kind, common::query_recorder::QueryKind::Insert) && e.seq >= after_read
        })
        .collect();
    assert!(
        !inserts.is_empty(),
        "create_file must issue INSERT statements"
    );
    assert!(
        inserts.iter().all(|e| e.in_tx),
        "create_file's inserts must be tagged in_tx = true (it runs inside a transaction):\n{}",
        rec.dump()
    );

    // Stats grouping is non-empty and keyed by (kind, table).
    let stats = rec.stats();
    assert!(!stats.is_empty());
}
