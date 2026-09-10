#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(all(feature = "integration", any(feature = "pg", feature = "mysql")))]

//! Native (PG/`MySQL`) integration test for traced-batch completion timing.
//!
//! A traced batch completes only when its **last** entity reaches a terminal
//! state. The countdown and the completion stamp ride one `UPDATE` on the trace
//! row, and that statement is dialect-specific: `Postgres` and `SQLite` evaluate
//! every `SET` right-hand side against the pre-update row, while `MySQL` evaluates
//! them left to right, so `pending` is already the post-ack value by the time
//! the completion `CASE`s run. A statement that ignores that difference stamps
//! completion a partial ack too early on `MySQL` - the batch is reported done
//! while an entity is still unprocessed.
//!
//! The in-process test suite runs on `SQLite` only and takes the `RETURNING`
//! branch, so it never executes the `MySQL` statement. This test drives the real
//! pipeline against a real server. A two-entity traced batch lands in one
//! partition; the handler acks the first entity and endlessly retries the
//! second, so the batch is durably half done - one entity terminal, one still
//! pending. While it is half done it must NOT be reported complete. On the buggy
//! `MySQL` statement `completed_at` is stamped on that first partial ack and this
//! test fails; the `Postgres` case is the control proving the test is sound on a
//! correct dialect.
//!
//! One partition keeps it deterministic: both entities arrive in one processor
//! batch, so the first-acked / second-retried split is a single, race-free
//! handler decision rather than a scheduling accident.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::{
    LeasedMessageHandler, MessageResult, Outbox, OutboxMessage, Partitions, Records, WorkerTuning,
    outbox_migrations,
};
use toolkit_db::{ConnectOpts, connect_db};

/// Payload of the entity held back. It is retried until the test lets it
/// through; the other entity is acked.
const HELD: &[u8] = b"hold";

/// Acks every entity except the held one, which is retried (this commits and
/// advances the cursor past the acked prefix, so the acked entity is terminal
/// while the held one stays pending) until `let_through` is set.
struct GatedHandler {
    let_through: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl LeasedMessageHandler for GatedHandler {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        if msg.payload == HELD && !self.let_through.load(Ordering::Acquire) {
            MessageResult::Retry
        } else {
            MessageResult::Ok
        }
    }
}

/// Enqueue a two-entity traced batch in one partition, ack the first and hold
/// the second, and assert the batch is not reported complete while half-done -
/// then release the held one and assert it completes exactly once.
async fn a_half_acked_batch_is_not_reported_complete(url: &str) {
    let db = connect_db(url, ConnectOpts::default())
        .await
        .expect("connect");
    run_migrations_for_testing(&db, outbox_migrations())
        .await
        .expect("migrations");

    let let_through = Arc::new(AtomicBool::new(false));

    // Short retry/idle so the held entity is retried promptly and, once let
    // through, acked promptly.
    let handle = Outbox::builder(db.clone())
        .processors(1)
        .maintenance(1, 1)
        .processor_tuning(
            WorkerTuning::processor()
                .idle_interval(Duration::from_millis(100))
                .retry_base(Duration::from_millis(50))
                .retry_max(Duration::from_millis(200)),
        )
        .queue("q", Partitions::of(1))
        .leased(GatedHandler {
            let_through: Arc::clone(&let_through),
        })
        .done()
        .start()
        .await
        .expect("start outbox");
    let outbox = handle.outbox();

    // Register interest before the enqueue commits.
    let mut sub = outbox.subscribe("split-1");

    // Enqueue inside a transaction so the trace insert and its id read share
    // one connection - on MySQL the id comes from `LAST_INSERT_ID()`, which is
    // per-connection, so a pooled standalone connection would read the wrong id.
    let o_inner = std::sync::Arc::clone(outbox);
    let (db, result) = outbox
        .transaction(db.clone(), move |tx| {
            let o2 = std::sync::Arc::clone(&o_inner);
            Box::pin(async move {
                let batch = Records::to("q")
                    .payload_type("test/plain")
                    .trace("split-1")
                    .push(0, b"free".to_vec())
                    .push(0, HELD.to_vec())
                    .build()
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                o2.enqueue_batch(tx, batch)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
        })
        .await;
    result.expect("enqueue");
    let conn = db.conn().expect("conn");

    // The first entity's ack drops pending from 2 to 1. Wait for that: the batch
    // is now provably half done - one entity terminal, one still retrying.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        let status = outbox
            .trace_status(&conn, "split-1")
            .await
            .expect("trace_status")
            .expect("trace row exists");
        if status.pending == 1 {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the first entity was never acked (pending stayed at {})",
            status.pending
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // The property under test: half-done is not done. On the buggy MySQL
    // statement `completed_at` is already stamped here.
    assert!(
        status.completed_at.is_none(),
        "batch reported complete with an entity still unprocessed (pending={})",
        status.pending
    );

    // And no completion has been delivered to the subscriber.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut sub)
            .await
            .is_err(),
        "a completion was delivered while an entity was still unprocessed"
    );

    // Let the held entity through; now the batch really completes and the
    // subscriber is told exactly once.
    let_through.store(true, Ordering::Release);
    let outcome = tokio::time::timeout(Duration::from_secs(20), sub)
        .await
        .expect("completion arrives after the last entity is acked")
        .expect("completion is not a dropped-registry None");
    assert_eq!(outcome.entities, 2, "both entities accounted for");
    assert!(outcome.is_clean(), "no entity should have dead-lettered");

    let final_status = outbox
        .trace_status(&conn, "split-1")
        .await
        .expect("trace_status")
        .expect("trace row exists");
    assert_eq!(final_status.pending, 0, "batch fully drained");
    assert!(
        final_status.completed_at.is_some(),
        "completion stamped once the batch really finished"
    );

    handle.stop().await;
}

#[cfg(feature = "pg")]
#[tokio::test]
async fn postgres_traced_batch_completes_only_when_every_entity_is_done() -> anyhow::Result<()> {
    let db = common::bring_up_postgres().await?;
    a_half_acked_batch_is_not_reported_complete(&db.url).await;
    Ok(())
}

#[cfg(feature = "mysql")]
#[tokio::test]
async fn mysql_traced_batch_completes_only_when_every_entity_is_done() -> anyhow::Result<()> {
    let db = common::bring_up_mysql().await?;
    a_half_acked_batch_is_not_reported_complete(&db.url).await;
    Ok(())
}

// -- Cross-instance completion delivery -------------------------------------
//
// A traced batch is owned by the instance that enqueued it (A), but any
// instance may process and ack it (B). Completion has two marks: whoever acks
// the last entity stamps `completed_at`; only the owner is ever handed the
// completion, and delivery is stamped by `notified_at`. When the acking
// instance is not the owner, the ack cannot stamp delivery, so the owner's
// notifier must do it.
//
// On Postgres/SQLite the owner's notifier claim is an `UPDATE ... RETURNING`
// that both stamps `notified_at` and returns the outcome. On MySQL the ack
// path folds the delivery stamp into the countdown guarded on
// `owner_instance = <acking instance>`, so a cross-instance ack never stamps
// it; the notifier's non-RETURNING claim therefore runs the stamping
// `claim_mail` UPDATE (guarded for exactly-once via `rows_affected`) before
// reading the outcome, so the owner still delivers. This test drives that path
// against a real server: it passes on every backend.

/// Acks every entity. Used by the processing instance B.
struct AckAllHandler;

#[async_trait::async_trait]
impl LeasedMessageHandler for AckAllHandler {
    async fn handle(&self, _msg: &OutboxMessage) -> MessageResult {
        MessageResult::Ok
    }
}

/// Instance A enqueues (and owns) a one-entity traced batch; instance B - a
/// distinct instance against the SAME database - is the only processor and
/// acks it. Assert A is told its batch completed.
///
/// A is started WITHOUT a queue declaration, so it spawns no processor and can
/// never terminalize the batch itself; only B acks, making this a genuine
/// cross-instance ack. A still runs its own notifier, the component under test.
async fn a_completion_reaches_the_submitting_instance(url: &str) {
    let db = connect_db(url, ConnectOpts::default())
        .await
        .expect("connect");
    run_migrations_for_testing(&db, outbox_migrations())
        .await
        .expect("migrations");

    // B: the only processor. Fast reconciler/sequencer so it discovers work
    // that A marked dirty in A's memory, not B's, through the cold reconciler.
    let handle_b = Outbox::builder(db.clone())
        .processors(1)
        .maintenance(1, 1)
        .processor_tuning(WorkerTuning::processor().idle_interval(Duration::from_millis(50)))
        .sequencer_tuning(WorkerTuning::sequencer().idle_interval(Duration::from_millis(20)))
        .reconciler_tuning(WorkerTuning::reconciler().idle_interval(Duration::from_millis(20)))
        .queue("shared-q", Partitions::of(1))
        .leased(AckAllHandler)
        .done()
        .start()
        .await
        .expect("start B");

    // A: owner/submitter. No queue -> no processor of its own.
    let handle_a = Outbox::builder(db.clone())
        .processors(1)
        .maintenance(1, 1)
        .start()
        .await
        .expect("start A");
    let outbox_a = handle_a.outbox();

    // Auto-generated ids: two started instances are distinct, which is all the
    // owner routing needs.
    assert_ne!(
        outbox_a.instance_id(),
        handle_b.outbox().instance_id(),
        "A and B must be distinct instances or the ack is same-instance and the bug is hidden"
    );

    // A must know the queue's partitions to enqueue into it; B created them.
    outbox_a
        .register_queue(&db, "shared-q", 1)
        .await
        .expect("A registers the shared queue");

    // Register interest before the enqueue commits.
    let sub = outbox_a.subscribe("cross-1");

    // Enqueue inside a transaction so the trace insert and its id read share
    // one connection - on MySQL the id comes from `LAST_INSERT_ID()`, which is
    // per-connection.
    let o_inner = std::sync::Arc::clone(outbox_a);
    let (_db, result) = outbox_a
        .transaction(db.clone(), move |tx| {
            let o2 = std::sync::Arc::clone(&o_inner);
            Box::pin(async move {
                let batch = Records::to("shared-q")
                    .payload_type("test/plain")
                    .trace("cross-1")
                    .push(0, b"work".to_vec())
                    .build()
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                o2.enqueue_batch(tx, batch)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
        })
        .await;
    result.expect("enqueue");

    // B processes and acks; A's notifier must deliver the completion to A.
    // On MySQL the cross-instance completion is never delivered, so this times
    // out - the bug. On Postgres it resolves.
    let outcome = tokio::time::timeout(Duration::from_secs(20), sub)
        .await
        .expect("A is told its batch completed (times out on the MySQL cross-instance bug)")
        .expect("completion is not a dropped-registry None");
    assert_eq!(outcome.trace, "cross-1");
    assert_eq!(outcome.entities, 1);
    assert_eq!(outcome.failures, 0);
    assert!(outcome.is_clean(), "no entity should have dead-lettered");

    handle_a.stop().await;
    handle_b.stop().await;
}

#[cfg(feature = "pg")]
#[tokio::test]
async fn postgres_cross_instance_completion_reaches_the_submitter() -> anyhow::Result<()> {
    let db = common::bring_up_postgres().await?;
    a_completion_reaches_the_submitting_instance(&db.url).await;
    Ok(())
}

#[cfg(feature = "mysql")]
#[tokio::test]
async fn mysql_cross_instance_completion_reaches_the_submitter() -> anyhow::Result<()> {
    let db = common::bring_up_mysql().await?;
    a_completion_reaches_the_submitting_instance(&db.url).await;
    Ok(())
}
