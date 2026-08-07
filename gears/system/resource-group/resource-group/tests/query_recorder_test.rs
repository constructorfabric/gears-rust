// Created: 2026-07-26 by Constructor Tech
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]
//! Live end-to-end wiring check for [`toolkit_db::test_support::QueryRecorder`].
//!
//! Pure-logic coverage of the recorder itself lives in the inline
//! `#[cfg(test)] mod tests` at the bottom of
//! `libs/toolkit-db/src/test_support.rs`, whose helpers are private there.
//!
//! This file only proves the wiring: attaching the recorder via
//! `common::test_db_with_recorder()` and driving it against a real SQLite
//! connection actually observes statements and tags transaction membership.

mod common;

use uuid::Uuid;

#[tokio::test]
async fn recorder_observes_statements_and_tags_tx_membership() {
    let (db, rec) = common::test_db_with_recorder().await;
    let type_svc = common::make_type_service(db);

    // A plain read: TypeService::get_type runs on `self.db.conn()?` --
    // outside any transaction.
    let code = format!(
        "gts.cf.core.rg.type.v1~x.test.recorder.i{}.v1~",
        Uuid::now_v7().as_simple()
    );
    // NotFound is fine -- we only care that a SELECT ran.
    type_svc.get_type(&code).await.ok();

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

    // A write wrapped in `db.transaction_with_retry(...)`: create_type's
    // INSERT statements must be tagged in_tx = true.
    let created = type_svc
        .create_type(resource_group_sdk::CreateTypeRequest {
            code: code.clone(),
            can_be_root: true,
            allowed_parent_types: vec![],
            allowed_membership_types: vec![],
            metadata_schema: None,
        })
        .await
        .expect("create_type should succeed");
    assert_eq!(created.code, code);

    let events = rec.events();
    assert!(
        events.len() > after_read,
        "create_type should have produced additional statements"
    );
    let inserts: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(e.kind, toolkit_db::test_support::QueryKind::Insert) && e.seq >= after_read
        })
        .collect();
    assert!(
        !inserts.is_empty(),
        "create_type must issue INSERT statements"
    );
    assert!(
        inserts.iter().all(|e| e.in_tx),
        "create_type's inserts must be tagged in_tx = true (it runs inside a transaction):\n{}",
        rec.dump()
    );

    // Stats grouping is non-empty and keyed by (kind, table).
    let stats = rec.stats();
    assert!(!stats.is_empty());
}
