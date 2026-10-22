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

use resource_group_sdk::CreateGroupRequest;
use uuid::Uuid;

#[tokio::test]
async fn recorder_observes_statements_and_tags_tx_membership() {
    let (db, rec) = common::test_db_with_recorder().await;
    let types = common::TestTypes::new();
    let group_svc = common::make_group_service(db, &types);
    let tenant = Uuid::now_v7();
    let ctx = common::make_ctx(tenant);

    // A plain read: `get_group_unscoped` runs on `self.db.conn()?` --
    // outside any transaction.
    // NotFound is fine -- we only care that a SELECT ran.
    group_svc.get_group_unscoped(Uuid::now_v7()).await.ok();

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

    // A write wrapped in `db.transaction_with_retry(...)`: create_group's
    // INSERT statements must be tagged in_tx = true.
    let root_type = common::create_root_type(&types, "recorder");
    let created = group_svc
        .create_group(
            &ctx,
            CreateGroupRequest::new(root_type.code.clone(), "Recorded".to_owned()),
            tenant,
        )
        .await
        .expect("create_group should succeed");
    assert_eq!(created.code, root_type.code);

    let events = rec.events();
    assert!(
        events.len() > after_read,
        "create_group should have produced additional statements"
    );
    // Sliced by position, not by `seq`. Both agree -- `seq` is the row's
    // index in the trace -- but taking the slice by position keeps this test
    // from depending on that invariant to be correct about *which* statements
    // it is looking at.
    let inserts: Vec<_> = events
        .iter()
        .skip(after_read)
        .filter(|e| matches!(e.kind, toolkit_db::test_support::QueryKind::Insert))
        .collect();
    assert!(
        !inserts.is_empty(),
        "create_group must issue INSERT statements"
    );
    assert!(
        inserts.iter().all(|e| e.in_tx),
        "create_group's inserts must be tagged in_tx = true (it runs inside a transaction):\n{}",
        rec.dump()
    );

    // Stats grouping is non-empty and keyed by (kind, table).
    let stats = rec.stats();
    assert!(!stats.is_empty());

    // `seq` is the row's index in the trace. It is assigned under the same
    // lock that appends, so this holds by construction rather than by
    // convention -- and it is what makes a trace dump readable against
    // `events()`.
    assert!(
        events.iter().enumerate().all(|(i, e)| e.seq == i),
        "seq must be the position in the trace:\n{}",
        rec.dump()
    );

    // And it restarts with the trace: `clear()` needs no separate counter to
    // reset, because there is no separate counter.
    rec.clear();
    group_svc.get_group_unscoped(created.id).await.ok();
    let after_clear = rec.events();
    assert!(
        !after_clear.is_empty(),
        "expected a statement after clear()"
    );
    assert_eq!(
        after_clear[0].seq, 0,
        "the first statement of a new trace is numbered 0"
    );
}
