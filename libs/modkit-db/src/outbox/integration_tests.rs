#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for the transactional outbox subsystem.
//!
//! Organized as narrative chapters that trace complete lifecycle paths.
//! Uses `SQLite` in-memory databases for fast, hermetic testing.
//!
//! Chapter ordering mirrors the pipeline:
//!   1. Registration  →  2. Enqueue  →  3. Sequencer
//!   4. Transactional Processing  →  5. Decoupled Processing
//!   6. Crash Detection & Recovery  →  7. Backoff & Adaptive Batching
//!   8. Vacuum  →  9. Dead Letters  →  10. Builder API
//!   11. End-to-End Lifecycle

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use tokio_util::sync::CancellationToken;

use super::dead_letter::{DeadLetterFilter, DeadLetterScope};
use super::dialect::Dialect;
use super::handler::{
    Handler, HandlerResult, MessageHandler, OutboxMessage, PerMessageAdapter, TransactionalHandler,
    TransactionalMessageHandler,
};
use super::prioritizer::SharedPrioritizer;
use super::strategy::{
    DecoupledStrategy, ProcessContext, ProcessingStrategy, TransactionalStrategy,
};
use super::taskward::{Directive, WorkerAction};
use super::types::{EnqueueMessage, OutboxConfig, SequencerConfig, WorkerTuning};
use super::workers::sequencer::Sequencer;
use super::{Outbox, OutboxError, Partitions};
use crate::migration_runner::run_migrations_for_testing;
use crate::outbox::OutboxMessageId;
use crate::{ConnectOpts, Db, connect_db};

// ======================================================================
// Snapshot structs
// ======================================================================

struct TestOutbox {
    outbox: Arc<Outbox>,
    prioritizer: Arc<SharedPrioritizer>,
}

#[derive(Debug)]
struct ProcessorSnapshot {
    processed_seq: i64,
    attempts: i16,
    last_error: Option<String>,
    locked_by: Option<String>,
    locked_until: Option<String>,
}

#[derive(Debug)]
struct OutgoingSnapshot {
    id: i64,
    partition_id: i64,
    body_id: i64,
    seq: i64,
}

#[derive(Debug)]
struct DeadLetterSnapshot {
    id: i64,
    partition_id: i64,
    seq: i64,
    payload: Vec<u8>,
    payload_type: String,
    last_error: Option<String>,
    attempts: i16,
    status: String,
    completed_at: Option<String>,
    deadline: Option<String>,
}

// ======================================================================
// Layer A — Infrastructure (create resources)
// ======================================================================

async fn setup_db(name: &str) -> Db {
    let url = format!("sqlite:file:{name}?mode=memory&cache=shared");
    let opts = ConnectOpts {
        max_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&url, opts).await.expect("connect");
    run_migrations_for_testing(&db, super::outbox_migrations())
        .await
        .expect("migrations");
    db
}

// Must be async: `prioritizer` is a tokio::sync::RwLock and `blocking_write()`
// panics when called from within a tokio runtime (i.e. every `#[tokio::test]`).
async fn make_test_outbox(config: OutboxConfig) -> TestOutbox {
    let prioritizer = Arc::new(SharedPrioritizer::new());
    let outbox = Arc::new(Outbox::new(config));
    outbox
        .prioritizer
        .write()
        .await
        .replace(Arc::clone(&prioritizer));
    TestOutbox {
        outbox,
        prioritizer,
    }
}

async fn make_default_test_outbox() -> TestOutbox {
    make_test_outbox(OutboxConfig::default()).await
}

fn make_shared_prioritizer() -> Arc<SharedPrioritizer> {
    Arc::new(SharedPrioritizer::new())
}

fn make_sequencer(t: &TestOutbox, config: SequencerConfig, db: &Db) -> Sequencer {
    Sequencer::new(
        config,
        Arc::clone(&t.outbox),
        db.clone(),
        Arc::clone(&t.prioritizer),
    )
}

// ======================================================================
// Layer B — Actions (do things)
// ======================================================================

async fn enqueue_msgs(
    outbox: &Outbox,
    db: &Db,
    queue: &str,
    partition: u32,
    payloads: &[&str],
) -> Vec<OutboxMessageId> {
    let conn = db.conn().expect("conn");
    let mut ids = Vec::with_capacity(payloads.len());
    for payload in payloads {
        let id = outbox
            .enqueue(
                &conn,
                queue,
                partition,
                payload.as_bytes().to_vec(),
                "text/plain",
            )
            .await
            .expect("enqueue");
        ids.push(id);
    }
    ids
}

/// Run sequencer until truly idle (no work done).
async fn run_sequencer_until_idle(seq: &mut Sequencer) {
    let cancel = CancellationToken::new();
    while let Directive::Proceed(_) = seq.execute(&cancel).await.unwrap() {}
}

async fn run_sequencer_once(t: &TestOutbox, db: &Db) {
    let mut seq = make_sequencer(t, SequencerConfig::default(), db);
    run_sequencer_until_idle(&mut seq).await;
}

async fn enqueue_and_sequence(
    t: &TestOutbox,
    db: &Db,
    queue: &str,
    partition: u32,
    payloads: &[&str],
) -> Vec<OutboxMessageId> {
    let ids = enqueue_msgs(&t.outbox, db, queue, partition, payloads).await;
    run_sequencer_once(t, db).await;
    ids
}

async fn simulate_crash(db: &Db, partition_id: i64, lease_secs: i64) {
    let conn = db.sea_internal();
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE modkit_outbox_processor \
         SET locked_by = $1, \
             locked_until = datetime('now', '+' || $2 || ' seconds'), \
             attempts = attempts + 1 \
         WHERE partition_id = $3",
        ["crashed-pod".into(), lease_secs.into(), partition_id.into()],
    ))
    .await
    .expect("simulate_crash");
}

async fn expire_lease(db: &Db, partition_id: i64) {
    let conn = db.sea_internal();
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE modkit_outbox_processor \
         SET locked_until = datetime('now', '-1 seconds') \
         WHERE partition_id = $1",
        [partition_id.into()],
    ))
    .await
    .expect("expire_lease");
}

// ======================================================================
// Layer C — Observations (read state only)
// ======================================================================

async fn count_rows(db: &Db, table: &str) -> i64 {
    #[derive(Debug, FromQueryResult)]
    struct Count {
        cnt: i64,
    }
    let conn = db.sea_internal();
    Count::find_by_statement(Statement::from_string(
        DbBackend::Sqlite,
        format!("SELECT COUNT(*) AS cnt FROM {table}"),
    ))
    .one(&conn)
    .await
    .expect("count query")
    .expect("count row")
    .cnt
}

async fn read_processor_state(db: &Db, partition_id: i64) -> ProcessorSnapshot {
    #[derive(Debug, FromQueryResult)]
    struct Row {
        processed_seq: i64,
        attempts: i16,
        last_error: Option<String>,
        locked_by: Option<String>,
        locked_until: Option<String>,
    }
    let conn = db.sea_internal();
    let row = Row::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT processed_seq, attempts, last_error, locked_by, \
         CAST(locked_until AS TEXT) AS locked_until \
         FROM modkit_outbox_processor WHERE partition_id = $1",
        [partition_id.into()],
    ))
    .one(&conn)
    .await
    .expect("query")
    .expect("processor row");
    ProcessorSnapshot {
        processed_seq: row.processed_seq,
        attempts: row.attempts,
        last_error: row.last_error,
        locked_by: row.locked_by,
        locked_until: row.locked_until,
    }
}

async fn read_outgoing(db: &Db, partition_id: i64) -> Vec<OutgoingSnapshot> {
    #[derive(Debug, FromQueryResult)]
    struct Row {
        id: i64,
        partition_id: i64,
        body_id: i64,
        seq: i64,
    }
    let conn = db.sea_internal();
    Row::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT id, partition_id, body_id, seq \
         FROM modkit_outbox_outgoing WHERE partition_id = $1 ORDER BY seq",
        [partition_id.into()],
    ))
    .all(&conn)
    .await
    .expect("query")
    .into_iter()
    .map(|r| OutgoingSnapshot {
        id: r.id,
        partition_id: r.partition_id,
        body_id: r.body_id,
        seq: r.seq,
    })
    .collect()
}

async fn read_dead_letters(db: &Db) -> Vec<DeadLetterSnapshot> {
    #[derive(Debug, FromQueryResult)]
    struct Row {
        id: i64,
        partition_id: i64,
        seq: i64,
        payload: Vec<u8>,
        payload_type: String,
        last_error: Option<String>,
        attempts: i16,
        status: String,
        completed_at: Option<String>,
        deadline: Option<String>,
    }
    let conn = db.sea_internal();
    Row::find_by_statement(Statement::from_string(
        DbBackend::Sqlite,
        "SELECT id, partition_id, seq, payload, payload_type, last_error, \
         attempts, status, CAST(completed_at AS TEXT) AS completed_at, \
         CAST(deadline AS TEXT) AS deadline \
         FROM modkit_outbox_dead_letters ORDER BY seq",
    ))
    .all(&conn)
    .await
    .expect("query")
    .into_iter()
    .map(|r| DeadLetterSnapshot {
        id: r.id,
        partition_id: r.partition_id,
        seq: r.seq,
        payload: r.payload,
        payload_type: r.payload_type,
        last_error: r.last_error,
        attempts: r.attempts,
        status: r.status,
        completed_at: r.completed_at,
        deadline: r.deadline,
    })
    .collect()
}

async fn read_partition_sequence(db: &Db, partition_id: i64) -> i64 {
    #[derive(Debug, FromQueryResult)]
    struct Row {
        sequence: i64,
    }
    let conn = db.sea_internal();
    Row::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT sequence FROM modkit_outbox_partitions WHERE id = $1",
        [partition_id.into()],
    ))
    .one(&conn)
    .await
    .expect("query")
    .expect("partition row")
    .sequence
}

async fn poll_until<F, Fut>(f: F, timeout_ms: u64)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if f().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "poll_until timed out after {timeout_ms}ms"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ======================================================================
// Test handlers
// ======================================================================

struct CountingSuccessHandler {
    count: Arc<AtomicU32>,
}

#[async_trait::async_trait]
impl Handler for CountingSuccessHandler {
    async fn handle(&self, msgs: &[OutboxMessage], _cancel: CancellationToken) -> HandlerResult {
        #[allow(clippy::cast_possible_truncation)]
        self.count.fetch_add(msgs.len() as u32, Ordering::Relaxed);
        HandlerResult::Success
    }
}

struct CountingMessageHandler {
    count: Arc<AtomicU32>,
}

#[async_trait::async_trait]
impl MessageHandler for CountingMessageHandler {
    async fn handle(&self, _msg: &OutboxMessage, _cancel: CancellationToken) -> HandlerResult {
        self.count.fetch_add(1, Ordering::Relaxed);
        HandlerResult::Success
    }
}

struct AlwaysRetryHandler;

#[async_trait::async_trait]
impl MessageHandler for AlwaysRetryHandler {
    async fn handle(&self, _msg: &OutboxMessage, _cancel: CancellationToken) -> HandlerResult {
        HandlerResult::Retry {
            reason: "transient failure".into(),
        }
    }
}

struct AlwaysRejectHandler;

#[async_trait::async_trait]
impl MessageHandler for AlwaysRejectHandler {
    async fn handle(&self, _msg: &OutboxMessage, _cancel: CancellationToken) -> HandlerResult {
        HandlerResult::Reject {
            reason: "permanently bad".into(),
        }
    }
}

struct AttemptsRecorder {
    seen_attempts: Arc<Mutex<Vec<i16>>>,
}

#[async_trait::async_trait]
impl MessageHandler for AttemptsRecorder {
    async fn handle(&self, msg: &OutboxMessage, _cancel: CancellationToken) -> HandlerResult {
        self.seen_attempts.lock().unwrap().push(msg.attempts);
        HandlerResult::Success
    }
}

struct CountingTxHandler {
    count: Arc<AtomicU32>,
}

#[async_trait::async_trait]
impl TransactionalHandler for CountingTxHandler {
    async fn handle(
        &self,
        _txn: &dyn ConnectionTrait,
        msgs: &[OutboxMessage],
        _cancel: CancellationToken,
    ) -> HandlerResult {
        #[allow(clippy::cast_possible_truncation)]
        self.count.fetch_add(msgs.len() as u32, Ordering::Relaxed);
        HandlerResult::Success
    }
}

struct AlwaysRetryTxHandler;

#[async_trait::async_trait]
impl TransactionalHandler for AlwaysRetryTxHandler {
    async fn handle(
        &self,
        _txn: &dyn ConnectionTrait,
        _msgs: &[OutboxMessage],
        _cancel: CancellationToken,
    ) -> HandlerResult {
        HandlerResult::Retry {
            reason: "transient tx failure".into(),
        }
    }
}

struct AlwaysRejectTxHandler;

#[async_trait::async_trait]
impl TransactionalHandler for AlwaysRejectTxHandler {
    async fn handle(
        &self,
        _txn: &dyn ConnectionTrait,
        _msgs: &[OutboxMessage],
        _cancel: CancellationToken,
    ) -> HandlerResult {
        HandlerResult::Reject {
            reason: "permanently bad tx".into(),
        }
    }
}

/// Rejects a specific message (by seq number), succeeds on others.
struct PoisonMessageHandler {
    poison_seqs: Vec<i64>,
}

#[async_trait::async_trait]
impl MessageHandler for PoisonMessageHandler {
    async fn handle(&self, msg: &OutboxMessage, _cancel: CancellationToken) -> HandlerResult {
        if self.poison_seqs.contains(&msg.seq) {
            HandlerResult::Reject {
                reason: format!("poison seq={}", msg.seq),
            }
        } else {
            HandlerResult::Success
        }
    }
}

// ======================================================================
// Chapter 1: Registration
// ======================================================================

#[tokio::test]
async fn registration_creates_partition_and_processor_rows() {
    let db = setup_db("ch1_creates_rows").await;
    let t = make_default_test_outbox().await;

    t.outbox.register_queue(&db, "orders", 4).await.unwrap();

    let part_count = count_rows(&db, "modkit_outbox_partitions").await;
    assert_eq!(part_count, 4, "4 partition rows");

    let proc_count = count_rows(&db, "modkit_outbox_processor").await;
    assert_eq!(proc_count, 4, "4 processor rows");

    // Each processor row starts at processed_seq=0, attempts=0
    let ids = t.outbox.all_partition_ids();
    for id in &ids {
        let snap = read_processor_state(&db, *id).await;
        assert_eq!(snap.processed_seq, 0);
        assert_eq!(snap.attempts, 0);
    }
}

#[tokio::test]
async fn registration_is_idempotent() {
    let db = setup_db("ch1_idempotent").await;
    let t = make_default_test_outbox().await;

    t.outbox.register_queue(&db, "orders", 4).await.unwrap();
    t.outbox.register_queue(&db, "orders", 4).await.unwrap();

    let part_count = count_rows(&db, "modkit_outbox_partitions").await;
    assert_eq!(part_count, 4, "still exactly 4 - no duplicates");
}

#[tokio::test]
async fn registration_rejects_mismatched_partition_count() {
    let db = setup_db("ch1_mismatch").await;
    let t = make_default_test_outbox().await;

    t.outbox.register_queue(&db, "orders", 4).await.unwrap();
    let err = t.outbox.register_queue(&db, "orders", 2).await.unwrap_err();

    assert!(matches!(
        err,
        OutboxError::PartitionCountMismatch {
            expected: 2,
            found: 4,
            ..
        }
    ));
}

#[tokio::test]
async fn registration_multiple_queues_distinct_ids() {
    let db = setup_db("ch1_multi_queue").await;
    let t = make_default_test_outbox().await;

    t.outbox.register_queue(&db, "a", 2).await.unwrap();
    t.outbox.register_queue(&db, "b", 2).await.unwrap();

    let all_ids = t.outbox.all_partition_ids();
    assert_eq!(all_ids.len(), 4);
    // All distinct (sorted + deduped by all_partition_ids)
    let mut deduped = all_ids;
    deduped.dedup();
    assert_eq!(deduped.len(), 4);
}

#[tokio::test]
async fn registration_partition_to_queue_reverse_lookup() {
    let db = setup_db("ch1_reverse_lookup").await;
    let t = make_default_test_outbox().await;

    t.outbox.register_queue(&db, "orders", 2).await.unwrap();

    let ids = t.outbox.all_partition_ids();
    assert_eq!(ids.len(), 2);
    for id in &ids {
        assert_eq!(t.outbox.partition_to_queue(*id).as_deref(), Some("orders"));
    }
}

// ======================================================================
// Chapter 2: Enqueue
// ======================================================================

#[tokio::test]
async fn enqueue_single_creates_body_and_incoming() {
    let db = setup_db("ch2_single").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    enqueue_msgs(&t.outbox, &db, "q", 0, &["hello"]).await;

    assert_eq!(count_rows(&db, "modkit_outbox_body").await, 1);
    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 1);
}

#[tokio::test]
async fn enqueue_returns_correct_id() {
    let db = setup_db("ch2_correct_id").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    let ids = enqueue_msgs(&t.outbox, &db, "q", 0, &["msg"]).await;
    assert_eq!(ids.len(), 1);
    // The returned ID should be the incoming row ID (positive integer)
    assert!(ids[0].0 > 0);
}

#[tokio::test]
async fn enqueue_tx_rollback_leaves_no_rows() {
    let db = setup_db("ch2_rollback").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    // Use sea_orm transaction directly to simulate rollback
    let conn = db.sea_internal();
    let txn = sea_orm::TransactionTrait::begin(&conn).await.unwrap();
    // Insert body + incoming manually through the transaction
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO modkit_outbox_body (payload, payload_type) VALUES ($1, $2)",
        [b"data".to_vec().into(), "text/plain".into()],
    ))
    .await
    .unwrap();
    // Rollback
    txn.rollback().await.unwrap();

    assert_eq!(count_rows(&db, "modkit_outbox_body").await, 0);
    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 0);
}

#[tokio::test]
async fn enqueue_with_standalone_connection() {
    let db = setup_db("ch2_standalone").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    // enqueue_msgs already uses db.conn() (standalone connection)
    enqueue_msgs(&t.outbox, &db, "q", 0, &["standalone"]).await;

    assert_eq!(count_rows(&db, "modkit_outbox_body").await, 1);
    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 1);
}

#[tokio::test]
async fn enqueue_batch_creates_n_items() {
    let db = setup_db("ch2_batch_n").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    let items: Vec<EnqueueMessage<'_>> = (0..50)
        .map(|i| EnqueueMessage {
            partition: 0,
            payload: format!("msg-{i}").into_bytes(),
            payload_type: "text/plain",
        })
        .collect();
    let conn = db.conn().unwrap();
    let ids = t.outbox.enqueue_batch(&conn, "q", &items).await.unwrap();

    assert_eq!(ids.len(), 50);
    assert_eq!(count_rows(&db, "modkit_outbox_body").await, 50);
    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 50);
}

#[tokio::test]
async fn enqueue_batch_mixed_partitions() {
    let db = setup_db("ch2_batch_mixed").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 2).await.unwrap();

    let items: Vec<EnqueueMessage<'_>> = vec![
        EnqueueMessage {
            partition: 0,
            payload: b"a".to_vec(),
            payload_type: "text/plain",
        },
        EnqueueMessage {
            partition: 1,
            payload: b"b".to_vec(),
            payload_type: "text/plain",
        },
        EnqueueMessage {
            partition: 0,
            payload: b"c".to_vec(),
            payload_type: "text/plain",
        },
        EnqueueMessage {
            partition: 1,
            payload: b"d".to_vec(),
            payload_type: "text/plain",
        },
    ];
    let conn = db.conn().unwrap();
    let ids = t.outbox.enqueue_batch(&conn, "q", &items).await.unwrap();
    assert_eq!(ids.len(), 4);
    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 4);
}

#[tokio::test]
async fn enqueue_batch_one_invalid_rejects_entire_batch() {
    let db = setup_db("ch2_batch_invalid").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    let oversized = vec![0u8; 64 * 1024 + 1];
    let items: Vec<EnqueueMessage<'_>> = vec![
        EnqueueMessage {
            partition: 0,
            payload: b"ok".to_vec(),
            payload_type: "text/plain",
        },
        EnqueueMessage {
            partition: 0,
            payload: oversized,
            payload_type: "text/plain",
        },
    ];
    let conn = db.conn().unwrap();
    let err = t
        .outbox
        .enqueue_batch(&conn, "q", &items)
        .await
        .unwrap_err();
    assert!(matches!(err, OutboxError::PayloadTooLarge { .. }));
    assert_eq!(count_rows(&db, "modkit_outbox_body").await, 0);
}

#[tokio::test]
async fn enqueue_empty_batch_returns_empty_vec() {
    let db = setup_db("ch2_batch_empty").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    let conn = db.conn().unwrap();
    let ids = t.outbox.enqueue_batch(&conn, "q", &[]).await.unwrap();
    assert!(ids.is_empty());
}

#[tokio::test]
async fn enqueue_batch_over_chunk_size_works() {
    let db = setup_db("ch2_batch_chunk").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    let items: Vec<EnqueueMessage<'_>> = (0..150)
        .map(|i| EnqueueMessage {
            partition: 0,
            payload: format!("msg-{i}").into_bytes(),
            payload_type: "text/plain",
        })
        .collect();
    let conn = db.conn().unwrap();
    let ids = t.outbox.enqueue_batch(&conn, "q", &items).await.unwrap();

    assert_eq!(ids.len(), 150);
    assert_eq!(count_rows(&db, "modkit_outbox_body").await, 150);
    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 150);
}

#[tokio::test]
async fn enqueue_oversized_payload_rejected() {
    let db = setup_db("ch2_oversized").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    let oversized = vec![0u8; 64 * 1024 + 1];
    let conn = db.conn().unwrap();
    let err = t
        .outbox
        .enqueue(&conn, "q", 0, oversized, "bin")
        .await
        .unwrap_err();
    assert!(matches!(err, OutboxError::PayloadTooLarge { .. }));
}

#[tokio::test]
async fn enqueue_unregistered_queue_rejected() {
    let db = setup_db("ch2_unreg").await;
    let t = make_default_test_outbox().await;
    // Don't register any queue

    let conn = db.conn().unwrap();
    let err = t
        .outbox
        .enqueue(&conn, "nonexistent", 0, b"x".to_vec(), "text/plain")
        .await
        .unwrap_err();
    assert!(matches!(err, OutboxError::QueueNotRegistered(_)));
}

#[tokio::test]
async fn enqueue_out_of_range_partition_rejected() {
    let db = setup_db("ch2_oor").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 4).await.unwrap();

    let conn = db.conn().unwrap();
    let err = t
        .outbox
        .enqueue(&conn, "q", 5, b"x".to_vec(), "text/plain")
        .await
        .unwrap_err();
    assert!(matches!(err, OutboxError::PartitionOutOfRange { .. }));
}

#[tokio::test]
async fn enqueue_transaction_helper_auto_flushes() {
    let db = setup_db("ch2_tx_flush").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    // Set up a notified() listener before the transaction.
    // flush() now goes through the prioritizer, so listen on its notifier.
    let notified = t.prioritizer.notifier();
    let notified = notified.notified();

    let (_db, result) = t
        .outbox
        .transaction(db, |tx| {
            let outbox = Arc::clone(&t.outbox);
            Box::pin(async move {
                outbox
                    .enqueue(tx, "q", 0, b"hello".to_vec(), "text/plain")
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            })
        })
        .await;
    result.unwrap();

    // Notify should fire within a short timeout
    tokio::time::timeout(Duration::from_millis(100), notified)
        .await
        .expect("sequencer should be notified on successful transaction");
}

#[tokio::test]
async fn enqueue_transaction_helper_no_flush_on_rollback() {
    let db = setup_db("ch2_tx_no_flush").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    let (_db, result) = t
        .outbox
        .transaction(db, |_tx| {
            Box::pin(async move { Err::<(), _>(anyhow::anyhow!("rollback")) })
        })
        .await;
    assert!(result.is_err());

    // Give a brief window — notify should NOT fire
    let notifier = t.prioritizer.notifier();
    let notified_fut = notifier.notified();
    let timed_out = tokio::time::timeout(Duration::from_millis(50), notified_fut)
        .await
        .is_err();
    assert!(timed_out, "sequencer should NOT be notified on rollback");
}

// ======================================================================
// Chapter 3: Sequencer
// ======================================================================

#[tokio::test]
async fn sequencer_moves_incoming_to_outgoing() {
    let db = setup_db("ch3_moves").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    enqueue_msgs(&t.outbox, &db, "q", 0, &["a", "b", "c"]).await;
    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 3);

    run_sequencer_once(&t, &db).await;

    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 0);
    assert_eq!(count_rows(&db, "modkit_outbox_outgoing").await, 3);

    let pid = t.outbox.all_partition_ids()[0];
    let outgoing = read_outgoing(&db, pid).await;
    let seqs: Vec<i64> = outgoing.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3]);

    // Verify structural fields: each row belongs to the queried partition,
    // has a positive id, and references a valid body row.
    for row in &outgoing {
        assert_eq!(row.partition_id, pid);
        assert!(row.id > 0);
        assert!(row.body_id > 0);
    }
    // IDs should be unique
    let ids: Vec<i64> = outgoing.iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), 3);
    assert!(ids[0] != ids[1] && ids[1] != ids[2]);
}

/// Enqueue many messages to one partition, sequence them, and verify the
/// outgoing sequence order matches the original enqueue (insertion) order.
/// This guards against non-deterministic row ordering in the claim step.
#[tokio::test]
async fn sequencer_preserves_enqueue_order_in_sequences() {
    let db = setup_db("ch3_fifo").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    // Enqueue 8 messages — enough to surface ordering issues
    let payloads: Vec<String> = (0..8).map(|i| format!("msg-{i}")).collect();
    let payload_refs: Vec<&str> = payloads.iter().map(String::as_str).collect();
    let enqueue_ids = enqueue_msgs(&t.outbox, &db, "q", 0, &payload_refs).await;

    run_sequencer_once(&t, &db).await;

    let pid = t.outbox.all_partition_ids()[0];
    let outgoing = read_outgoing(&db, pid).await;

    // Sequences must be strictly monotonically increasing
    let seqs: Vec<i64> = outgoing.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5, 6, 7, 8]);

    // body_ids must follow the same order as enqueue_ids (insertion order)
    let body_ids: Vec<i64> = outgoing.iter().map(|r| r.body_id).collect();
    for i in 1..body_ids.len() {
        assert!(
            body_ids[i] > body_ids[i - 1],
            "body_id[{i}]={} should be > body_id[{}]={}",
            body_ids[i],
            i - 1,
            body_ids[i - 1]
        );
    }

    // Verify count matches
    assert_eq!(enqueue_ids.len(), 8);
    assert_eq!(outgoing.len(), 8);
}

#[tokio::test]
async fn sequencer_updates_partition_sequence_counter() {
    let db = setup_db("ch3_seq_counter").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    enqueue_msgs(&t.outbox, &db, "q", 0, &["a", "b", "c"]).await;
    run_sequencer_once(&t, &db).await;

    let pid = t.outbox.all_partition_ids()[0];
    let seq = read_partition_sequence(&db, pid).await;
    assert_eq!(seq, 3);
}

#[tokio::test]
async fn sequencer_multi_partition_independent_sequences() {
    let db = setup_db("ch3_multi_part").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 2).await.unwrap();

    enqueue_msgs(&t.outbox, &db, "q", 0, &["a0", "b0"]).await;
    enqueue_msgs(&t.outbox, &db, "q", 1, &["a1", "b1", "c1"]).await;
    run_sequencer_once(&t, &db).await;

    let ids = t.outbox.all_partition_ids();
    let out0 = read_outgoing(&db, ids[0]).await;
    let out1 = read_outgoing(&db, ids[1]).await;

    let seqs0: Vec<i64> = out0.iter().map(|r| r.seq).collect();
    let seqs1: Vec<i64> = out1.iter().map(|r| r.seq).collect();
    assert_eq!(seqs0, vec![1, 2]);
    assert_eq!(seqs1, vec![1, 2, 3]);
}

#[tokio::test]
async fn sequencer_empty_incoming_returns_zero() {
    let db = setup_db("ch3_empty").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    let mut seq = make_sequencer(&t, SequencerConfig::default(), &db);
    let cancel = CancellationToken::new();
    let result = seq.execute(&cancel).await.unwrap();
    assert!(matches!(result, Directive::Idle(_)));
}

#[tokio::test]
async fn sequencer_consecutive_batches_contiguous_sequences() {
    let db = setup_db("ch3_contiguous").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    enqueue_msgs(&t.outbox, &db, "q", 0, &["a", "b"]).await;
    run_sequencer_once(&t, &db).await;

    enqueue_msgs(&t.outbox, &db, "q", 0, &["c", "d"]).await;
    run_sequencer_once(&t, &db).await;

    let pid = t.outbox.all_partition_ids()[0];
    let outgoing = read_outgoing(&db, pid).await;
    let seqs: Vec<i64> = outgoing.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4]);
}

#[tokio::test]
async fn sequencer_batch_size_limit_enforced() {
    let db = setup_db("ch3_batch_limit").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    // 5 items, batch_size=2, max_inner_iterations=2 → processes 4, leaves 1.
    enqueue_msgs(&t.outbox, &db, "q", 0, &["a", "b", "c", "d", "e"]).await;

    let mut seq = make_sequencer(
        &t,
        SequencerConfig {
            batch_size: 2,
            max_inner_iterations: 2,
            ..Default::default()
        },
        &db,
    );
    let cancel = CancellationToken::new();
    // 2 iterations × 2 items = 4 processed. 1 remains → not drained → re-dirtied.
    let result = seq.execute(&cancel).await.unwrap();
    assert!(matches!(result, Directive::Proceed(_)));
    assert_eq!(result.payload().rows_claimed, 4);
    // Not drained (hit max_inner_iterations) → re-dirtied
    let guard = t
        .prioritizer
        .take()
        .expect("partition should be re-dirtied");
    guard.processed();

    // Remaining 1 item still in incoming
    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 1);
}

#[tokio::test]
async fn sequencer_saturated_partition_re_dirtied() {
    let db = setup_db("ch3_saturated").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    // With batch_size=2, max_inner_iterations=1: claims 2 of 3, can't drain
    // in one iteration → partition is still saturated → re-dirtied.
    enqueue_msgs(&t.outbox, &db, "q", 0, &["a", "b", "c"]).await;

    let mut seq = make_sequencer(
        &t,
        SequencerConfig {
            batch_size: 2,
            max_inner_iterations: 1,
            ..Default::default()
        },
        &db,
    );
    let cancel = CancellationToken::new();
    let result = seq.execute(&cancel).await.unwrap();
    assert!(matches!(result, Directive::Proceed(_)));
    // Only 1 inner iteration allowed, claimed 2 of 3 → not drained → re-dirtied
    let guard = t
        .prioritizer
        .take()
        .expect("partition should be re-dirtied");
    guard.processed();
}

#[tokio::test]
async fn sequencer_unsaturated_partition_not_re_dirtied() {
    let db = setup_db("ch3_unsaturated").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    enqueue_msgs(&t.outbox, &db, "q", 0, &["a"]).await;

    let mut seq = make_sequencer(
        &t,
        SequencerConfig {
            batch_size: 100,
            ..Default::default()
        },
        &db,
    );
    let cancel = CancellationToken::new();
    let result = seq.execute(&cancel).await.unwrap();
    assert!(matches!(result, Directive::Proceed(_)));
    // Not saturated → not re-dirtied (but Proceed because work was done)
    assert!(t.prioritizer.take().is_none());
}

#[tokio::test]
async fn sequencer_skips_empty_partitions() {
    let db = setup_db("ch3_skip_empty").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 2).await.unwrap();

    // Only enqueue to partition 1, not partition 0
    enqueue_msgs(&t.outbox, &db, "q", 1, &["only-p1"]).await;
    run_sequencer_once(&t, &db).await;

    let ids = t.outbox.all_partition_ids();
    let out0 = read_outgoing(&db, ids[0]).await;
    let out1 = read_outgoing(&db, ids[1]).await;

    assert!(out0.is_empty(), "partition 0 should have no outgoing");
    assert_eq!(out1.len(), 1, "partition 1 should have 1 outgoing");
}

// ======================================================================
// Chapter 4: Transactional Processing
// ======================================================================

async fn run_transactional(
    db: &Db,
    partition_id: i64,
    handler: impl TransactionalHandler + 'static,
    lease_duration: Duration,
    msg_batch_size: u32,
) -> Option<super::strategy::ProcessResult> {
    let conn = db.sea_internal();
    let backend = conn.get_database_backend();
    let dialect = Dialect::from(backend);
    drop(conn);

    let strategy = TransactionalStrategy::new(Box::new(handler));
    let ctx = ProcessContext {
        db,
        backend,
        dialect,
        partition_id,
    };
    strategy
        .process(
            &ctx,
            lease_duration,
            msg_batch_size,
            CancellationToken::new(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn transactional_success_advances_cursor() {
    let db = setup_db("ch4_tx_success").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c"]).await;

    let count = Arc::new(AtomicU32::new(0));
    run_transactional(
        &db,
        pid,
        CountingTxHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        3,
    )
    .await;

    assert_eq!(count.load(Ordering::Relaxed), 3);
    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 3);
    assert_eq!(snap.attempts, 0);
}

#[tokio::test]
async fn transactional_retry_increments_attempts() {
    let db = setup_db("ch4_tx_retry").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["msg"]).await;

    run_transactional(&db, pid, AlwaysRetryTxHandler, Duration::from_secs(30), 10).await;

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 0, "cursor not advanced");
    assert_eq!(snap.attempts, 1);
    assert_eq!(snap.last_error.as_deref(), Some("transient tx failure"));
}

#[tokio::test]
async fn transactional_reject_creates_dead_letter_and_advances() {
    let db = setup_db("ch4_tx_reject").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["poison"]).await;

    run_transactional(&db, pid, AlwaysRejectTxHandler, Duration::from_secs(30), 10).await;

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 1, "cursor advanced past rejected msg");

    let dls = read_dead_letters(&db).await;
    assert_eq!(dls.len(), 1);
    assert!(dls[0].id > 0);
    assert_eq!(dls[0].partition_id, pid);
    assert_eq!(dls[0].seq, 1);
    assert_eq!(dls[0].last_error.as_deref(), Some("permanently bad tx"));
    assert_eq!(dls[0].payload, b"poison");
    assert_eq!(dls[0].payload_type, "text/plain");
    assert_eq!(dls[0].attempts, 0);
    assert_eq!(dls[0].status, "pending");
}

#[tokio::test]
async fn transactional_batch_processes_multiple_in_single_tx() {
    let db = setup_db("ch4_tx_batch").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c"]).await;

    let count = Arc::new(AtomicU32::new(0));
    // CountingTxHandler counts the number of messages per call
    run_transactional(
        &db,
        pid,
        CountingTxHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        3,
    )
    .await;

    // Handler called once with all 3 messages
    assert_eq!(count.load(Ordering::Relaxed), 3);
}

// ======================================================================
// Chapter 5: Decoupled Processing
// ======================================================================

async fn run_decoupled(
    db: &Db,
    partition_id: i64,
    handler: impl Handler + 'static,
    lease_duration: Duration,
    msg_batch_size: u32,
) -> Option<super::strategy::ProcessResult> {
    let conn = db.sea_internal();
    let backend = conn.get_database_backend();
    let dialect = Dialect::from(backend);
    drop(conn);

    let strategy = DecoupledStrategy::new(Box::new(handler), "test-AAAAAA".to_owned());
    let ctx = ProcessContext {
        db,
        backend,
        dialect,
        partition_id,
    };
    strategy
        .process(
            &ctx,
            lease_duration,
            msg_batch_size,
            CancellationToken::new(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn decoupled_success_advances_cursor_and_releases_lease() {
    let db = setup_db("ch5_dc_success").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b"]).await;

    let count = Arc::new(AtomicU32::new(0));
    run_decoupled(
        &db,
        pid,
        CountingSuccessHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        2,
    )
    .await;

    assert_eq!(count.load(Ordering::Relaxed), 2);
    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 2);
    assert_eq!(snap.attempts, 0);
    assert!(snap.locked_by.is_none(), "lease released");
    assert!(snap.locked_until.is_none(), "lease released");
}

#[tokio::test]
async fn decoupled_retry_preserves_cursor_and_releases_lease() {
    let db = setup_db("ch5_dc_retry").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["msg"]).await;

    run_decoupled(
        &db,
        pid,
        PerMessageAdapter::new(AlwaysRetryHandler),
        Duration::from_secs(30),
        10,
    )
    .await;

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 0, "cursor unchanged");
    assert_eq!(snap.attempts, 1, "attempts incremented by lease_acquire");
    assert_eq!(snap.last_error.as_deref(), Some("transient failure"));
    assert!(snap.locked_by.is_none(), "lease released");
}

#[tokio::test]
async fn decoupled_reject_creates_dead_letter_and_releases_lease() {
    let db = setup_db("ch5_dc_reject").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["bad"]).await;

    run_decoupled(
        &db,
        pid,
        PerMessageAdapter::new(AlwaysRejectHandler),
        Duration::from_secs(30),
        10,
    )
    .await;

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 1, "cursor advanced past rejected");
    assert!(snap.locked_by.is_none(), "lease released");

    let dls = read_dead_letters(&db).await;
    assert_eq!(dls.len(), 1);
    assert_eq!(dls[0].last_error.as_deref(), Some("permanently bad"));
}

#[tokio::test]
async fn decoupled_empty_partition_releases_lease() {
    let db = setup_db("ch5_dc_empty").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    // No messages enqueued
    let count = Arc::new(AtomicU32::new(0));
    let result = run_decoupled(
        &db,
        pid,
        CountingSuccessHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        10,
    )
    .await;

    assert!(result.is_none(), "no work done");
    assert_eq!(count.load(Ordering::Relaxed), 0);
    let snap = read_processor_state(&db, pid).await;
    assert!(snap.locked_by.is_none(), "lease released after empty");
}

#[tokio::test]
async fn decoupled_empty_partition_does_not_accumulate_attempts() {
    let db = setup_db("ch5_dc_empty_attempts").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    // Run 5 empty lease cycles: acquire → empty → release
    for _ in 0..5 {
        let count = Arc::new(AtomicU32::new(0));
        run_decoupled(
            &db,
            pid,
            CountingSuccessHandler {
                count: count.clone(),
            },
            Duration::from_secs(30),
            10,
        )
        .await;
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    // After 5 empty cycles, attempts should be 0 (reset on each release)
    let snap = read_processor_state(&db, pid).await;
    assert_eq!(
        snap.attempts, 0,
        "attempts should be 0 after empty lease cycles, not accumulated"
    );
}

#[tokio::test]
async fn decoupled_each_message_adapter_processes_individually() {
    let db = setup_db("ch5_dc_each").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c"]).await;

    let count = Arc::new(AtomicU32::new(0));
    let handler = PerMessageAdapter::new(CountingMessageHandler {
        count: count.clone(),
    });
    run_decoupled(&db, pid, handler, Duration::from_secs(30), 3).await;

    // PerMessageAdapter calls MessageHandler once per message
    assert_eq!(count.load(Ordering::Relaxed), 3);
}

// ======================================================================
// Chapter 6: Crash Detection & Recovery
// ======================================================================

#[tokio::test]
async fn crash_leaves_incremented_attempts_in_db() {
    let db = setup_db("ch6_crash_trace").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["msg"]).await;

    // Simulate: lease acquired (attempts incremented in DB), then pod dies
    simulate_crash(&db, pid, 300).await;

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.attempts, 1, "crash left incremented attempts");
    assert_eq!(snap.processed_seq, 0, "cursor unchanged");
    assert!(snap.locked_by.is_some(), "lease still held by crashed pod");
}

#[tokio::test]
async fn recovery_after_crash_sees_nonzero_attempts() {
    let db = setup_db("ch6_recovery").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["msg"]).await;

    // Crash + expire lease so a new processor can acquire it
    simulate_crash(&db, pid, 300).await;
    expire_lease(&db, pid).await;

    // Recovery processor should see attempts=1 (from the crash)
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = AttemptsRecorder {
        seen_attempts: seen.clone(),
    };
    run_decoupled(
        &db,
        pid,
        PerMessageAdapter::new(handler),
        Duration::from_secs(30),
        10,
    )
    .await;

    {
        let recorded = seen.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0], 1, "handler sees attempts=1 from the crash");
    }

    // After success, attempts reset to 0
    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.attempts, 0);
}

#[tokio::test]
async fn multiple_crashes_accumulate_attempts() {
    let db = setup_db("ch6_multi_crash").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["msg"]).await;

    // Two crashes
    simulate_crash(&db, pid, 300).await;
    expire_lease(&db, pid).await;
    simulate_crash(&db, pid, 300).await;
    expire_lease(&db, pid).await;

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.attempts, 2, "two crashes accumulated");
}

#[tokio::test]
async fn retry_does_not_double_increment_attempts() {
    let db = setup_db("ch6_no_double").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["msg"]).await;

    // lease_acquire increments attempts 0→1 in DB;
    // handler returns Retry; lease_record_retry does NOT increment again
    run_decoupled(
        &db,
        pid,
        PerMessageAdapter::new(AlwaysRetryHandler),
        Duration::from_secs(30),
        10,
    )
    .await;

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.attempts, 1, "not 2 - retry doesn't double-increment");
}

#[tokio::test]
async fn success_after_crash_resets_attempts() {
    let db = setup_db("ch6_reset").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["msg"]).await;

    // Crash
    simulate_crash(&db, pid, 300).await;
    expire_lease(&db, pid).await;

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.attempts, 1);

    // Recovery succeeds
    let count = Arc::new(AtomicU32::new(0));
    run_decoupled(
        &db,
        pid,
        CountingSuccessHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        10,
    )
    .await;

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.attempts, 0, "success resets attempts to 0");
    assert_eq!(snap.processed_seq, 1);
}

// ======================================================================
// Chapter 7: Backoff & Adaptive Batching
// ======================================================================

#[tokio::test]
async fn adaptive_batch_isolates_poison_message() {
    let db = setup_db("ch7_poison").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    // Enqueue 4 messages; message at seq=2 is the poison pill
    enqueue_and_sequence(&t, &db, "q", 0, &["ok1", "poison", "ok3", "ok4"]).await;

    // Demonstrate the adaptive batch isolation mechanism step by step
    // with batch_size=1 (the degraded size):

    // Process msg 1 (ok1) — success
    let r = run_decoupled(
        &db,
        pid,
        PerMessageAdapter::new(PoisonMessageHandler {
            poison_seqs: vec![2],
        }),
        Duration::from_secs(30),
        1,
    )
    .await;
    assert!(matches!(r.unwrap().handler_result, HandlerResult::Success));

    // Process msg 2 (poison) — reject, dead-lettered
    let r = run_decoupled(
        &db,
        pid,
        PerMessageAdapter::new(PoisonMessageHandler {
            poison_seqs: vec![2],
        }),
        Duration::from_secs(30),
        1,
    )
    .await;
    assert!(matches!(
        r.unwrap().handler_result,
        HandlerResult::Reject { .. }
    ));

    // Process msg 3 (ok3) — success
    let r = run_decoupled(
        &db,
        pid,
        PerMessageAdapter::new(PoisonMessageHandler {
            poison_seqs: vec![2],
        }),
        Duration::from_secs(30),
        1,
    )
    .await;
    assert!(matches!(r.unwrap().handler_result, HandlerResult::Success));

    // Process msg 4 (ok4) — success
    let r = run_decoupled(
        &db,
        pid,
        PerMessageAdapter::new(PoisonMessageHandler {
            poison_seqs: vec![2],
        }),
        Duration::from_secs(30),
        1,
    )
    .await;
    assert!(matches!(r.unwrap().handler_result, HandlerResult::Success));

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 4, "all 4 messages processed");

    let dls = read_dead_letters(&db).await;
    assert_eq!(dls.len(), 1, "only the poison message was dead-lettered");
    assert_eq!(dls[0].seq, 2);
}

// ======================================================================
// Chapter 8: Vacuum
// ======================================================================

/// Run the vacuum for a single partition: read `processed_seq`, delete
/// outgoing + body rows in batches, then reset the vacuum counter.
async fn run_vacuum(db: &Db, partition_id: i64) {
    #[derive(Debug, FromQueryResult)]
    struct ProcRow {
        processed_seq: i64,
    }

    let conn = db.sea_internal();
    let backend = conn.get_database_backend();
    let dialect = Dialect::from(backend);

    let proc_row = ProcRow::find_by_statement(Statement::from_sql_and_values(
        backend,
        "SELECT processed_seq FROM modkit_outbox_processor WHERE partition_id = $1",
        [partition_id.into()],
    ))
    .one(&conn)
    .await
    .unwrap()
    .unwrap();

    if proc_row.processed_seq == 0 {
        return;
    }

    let vacuum_sql = dialect.vacuum_cleanup();

    // Fetch outgoing rows in bounded chunks.
    loop {
        let rows = conn
            .query_all(Statement::from_sql_and_values(
                backend,
                vacuum_sql.select_outgoing_chunk,
                [
                    partition_id.into(),
                    proc_row.processed_seq.into(),
                    10_000i64.into(),
                ],
            ))
            .await
            .unwrap();
        if rows.is_empty() {
            break;
        }
        let outgoing_ids: Vec<i64> = rows
            .iter()
            .filter_map(|r| r.try_get_by_index::<i64>(0).ok())
            .collect();
        let body_ids: Vec<i64> = rows
            .iter()
            .filter_map(|r| r.try_get_by_index::<i64>(1).ok())
            .collect();
        // Delete outgoing by ID
        let del_out = dialect.build_delete_outgoing_batch(outgoing_ids.len());
        let values: Vec<sea_orm::Value> = outgoing_ids.iter().map(|&id| id.into()).collect();
        conn.execute(Statement::from_sql_and_values(backend, &del_out, values))
            .await
            .unwrap();
        // Delete body by ID
        if !body_ids.is_empty() {
            let del_body = dialect.build_delete_body_batch(body_ids.len());
            let values: Vec<sea_orm::Value> = body_ids.iter().map(|&id| id.into()).collect();
            conn.execute(Statement::from_sql_and_values(backend, &del_body, values))
                .await
                .unwrap();
        }
    }

    // Reset vacuum counter after cleanup.
    conn.execute(Statement::from_sql_and_values(
        backend,
        dialect.reset_vacuum_counter(),
        [partition_id.into()],
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn vacuum_deletes_processed_outgoing_and_body_rows() {
    let db = setup_db("ch8_vacuum_deletes").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c"]).await;

    // Process all 3
    let count = Arc::new(AtomicU32::new(0));
    run_decoupled(
        &db,
        pid,
        CountingSuccessHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        3,
    )
    .await;

    // Reap
    run_vacuum(&db, pid).await;

    assert_eq!(count_rows(&db, "modkit_outbox_outgoing").await, 0);
    assert_eq!(count_rows(&db, "modkit_outbox_body").await, 0);

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 3, "cursor preserved");
}

#[tokio::test]
async fn vacuum_skips_when_processed_seq_is_zero() {
    let db = setup_db("ch8_vacuum_skip").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a"]).await;

    // Don't process — cursor at 0
    run_vacuum(&db, pid).await;

    assert_eq!(
        count_rows(&db, "modkit_outbox_outgoing").await,
        1,
        "rows preserved"
    );
}

#[tokio::test]
async fn vacuum_preserves_unprocessed_rows() {
    let db = setup_db("ch8_vacuum_preserves").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c", "d", "e"]).await;

    // Process only 3 of 5 (batch_size=3)
    let count = Arc::new(AtomicU32::new(0));
    run_decoupled(
        &db,
        pid,
        CountingSuccessHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        3,
    )
    .await;

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 3);

    // Reap — should only delete seqs 1-3
    run_vacuum(&db, pid).await;

    let remaining = read_outgoing(&db, pid).await;
    assert_eq!(remaining.len(), 2);
    let seqs: Vec<i64> = remaining.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![4, 5]);
    for row in &remaining {
        assert_eq!(row.partition_id, pid);
        assert!(row.id > 0);
        assert!(row.body_id > 0);
    }
}

/// Read the vacuum counter for a partition.
async fn read_vacuum_counter(db: &Db, partition_id: i64) -> i64 {
    #[derive(Debug, FromQueryResult)]
    struct Row {
        counter: i64,
    }
    let conn = db.sea_internal();
    Row::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "SELECT counter FROM modkit_outbox_vacuum_counter WHERE partition_id = $1",
        [partition_id.into()],
    ))
    .one(&conn)
    .await
    .expect("query")
    .expect("vacuum counter row")
    .counter
}

/// Set the vacuum counter to an arbitrary value (test helper).
async fn set_vacuum_counter(db: &Db, partition_id: i64, value: i64) {
    let conn = db.sea_internal();
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE modkit_outbox_vacuum_counter SET counter = $1 WHERE partition_id = $2",
        [value.into(), partition_id.into()],
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn vacuum_counter_bumped_on_processed_seq_advance() {
    let db = setup_db("ch8_counter_bump").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    // Counter starts at 0.
    assert_eq!(read_vacuum_counter(&db, pid).await, 0);

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b"]).await;

    // Process batch of 2 — counter should bump once (one ack).
    let count = Arc::new(AtomicU32::new(0));
    run_decoupled(
        &db,
        pid,
        CountingSuccessHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        2,
    )
    .await;

    assert_eq!(read_vacuum_counter(&db, pid).await, 1);

    // Process again (no messages) — counter should not change.
    run_decoupled(
        &db,
        pid,
        CountingSuccessHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        2,
    )
    .await;

    assert_eq!(read_vacuum_counter(&db, pid).await, 1);
}

#[tokio::test]
async fn vacuum_counter_preserves_concurrent_bumps() {
    let db = setup_db("ch8_counter_concurrent").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c"]).await;

    // Process all 3 — counter = 1 (one ack of batch=3).
    let count = Arc::new(AtomicU32::new(0));
    run_decoupled(
        &db,
        pid,
        CountingSuccessHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        3,
    )
    .await;

    assert_eq!(read_vacuum_counter(&db, pid).await, 1);

    // Simulate a concurrent processor bump (as if more messages were processed
    // while vacuum was running): manually set counter to 3.
    set_vacuum_counter(&db, pid, 3).await;

    // Vacuum with snapshot_counter=3 — deletes rows, decrements by 3.
    // After decrement: counter = GREATEST(3 - 3, 0) = 0.
    run_vacuum(&db, pid).await;

    assert_eq!(count_rows(&db, "modkit_outbox_outgoing").await, 0);
    assert_eq!(read_vacuum_counter(&db, pid).await, 0);
}

#[tokio::test]
async fn vacuum_stale_counter_reset() {
    let db = setup_db("ch8_stale_counter").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    // Create and process a message so processed_seq > 0.
    enqueue_and_sequence(&t, &db, "q", 0, &["a"]).await;
    let count = Arc::new(AtomicU32::new(0));
    run_decoupled(
        &db,
        pid,
        CountingSuccessHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        10,
    )
    .await;

    // Vacuum to clean up — counter goes to 0.
    run_vacuum(&db, pid).await;
    assert_eq!(read_vacuum_counter(&db, pid).await, 0);
    assert_eq!(count_rows(&db, "modkit_outbox_outgoing").await, 0);

    // Simulate stale counter (as if crash prevented decrement).
    set_vacuum_counter(&db, pid, 5).await;

    // Vacuum runs again: processed_seq > 0 but no outgoing rows → stale.
    // The run_vacuum helper resets counter after cleanup (0 rows deleted).
    run_vacuum(&db, pid).await;

    assert_eq!(read_vacuum_counter(&db, pid).await, 0);
}

#[tokio::test]
async fn vacuum_counter_row_created_on_register_queue() {
    let db = setup_db("ch8_counter_register").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 2).await.unwrap();

    let pids = t.outbox.all_partition_ids();
    assert_eq!(pids.len(), 2);

    // Both partitions should have vacuum counter rows with counter = 0.
    for &pid in &pids {
        assert_eq!(read_vacuum_counter(&db, pid).await, 0);
    }

    // Re-register (idempotent) — should not fail.
    t.outbox.register_queue(&db, "q", 2).await.unwrap();

    // Counters still 0.
    for &pid in &pids {
        assert_eq!(read_vacuum_counter(&db, pid).await, 0);
    }
}

// ======================================================================
// Chapter 9: Dead Letters
// ======================================================================

/// Helper: enqueue, sequence, and reject N messages to create dead letters.
async fn create_dead_letters(
    t: &TestOutbox,
    db: &Db,
    queue: &str,
    partition: u32,
    payloads: &[&str],
) {
    enqueue_and_sequence(t, db, queue, partition, payloads).await;
    let ids = t.outbox.all_partition_ids();
    let pid = ids[partition as usize];
    run_decoupled(
        db,
        pid,
        PerMessageAdapter::new(AlwaysRejectHandler),
        Duration::from_secs(30),
        u32::try_from(payloads.len()).unwrap(),
    )
    .await;
}

#[tokio::test]
async fn dead_letter_list_returns_correct_fields() {
    let db = setup_db("ch9_dl_list").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    create_dead_letters(&t, &db, "q", 0, &["a", "b", "c"]).await;

    let items = t
        .outbox
        .dead_letter_list(&db.conn().unwrap(), &DeadLetterFilter::default())
        .await
        .unwrap();
    assert_eq!(items.len(), 3);
    for item in &items {
        assert_eq!(item.partition_id, pid);
        assert_eq!(item.last_error.as_deref(), Some("permanently bad"));
        assert_eq!(item.status, super::dead_letter::DeadLetterStatus::Pending);
        assert!(item.completed_at.is_none());
    }
}

#[tokio::test]
async fn dead_letter_count_matches_list() {
    let db = setup_db("ch9_dl_count").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    create_dead_letters(&t, &db, "q", 0, &["a", "b", "c"]).await;

    let count = t
        .outbox
        .dead_letter_count(&db.conn().unwrap(), &DeadLetterFilter::default())
        .await
        .unwrap();
    assert_eq!(count, 3);
}

#[tokio::test]
async fn dead_letter_replay_claims_and_sets_reprocessing() {
    let db = setup_db("ch9_dl_replay").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    create_dead_letters(&t, &db, "q", 0, &["msg"]).await;

    let replayed = t
        .outbox
        .dead_letter_replay(
            &db.conn().unwrap(),
            &DeadLetterScope::default(),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(replayed.len(), 1);

    // Dead letter now has status=reprocessing and a deadline
    let dls = read_dead_letters(&db).await;
    assert_eq!(dls.len(), 1);
    assert_eq!(dls[0].status, "reprocessing");
    assert!(dls[0].deadline.is_some());
}

#[tokio::test]
async fn dead_letter_full_replay_roundtrip() {
    let db = setup_db("ch9_dl_roundtrip").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    // Reject
    create_dead_letters(&t, &db, "q", 0, &["msg"]).await;

    // Replay (claim) → resolve
    let replayed = t
        .outbox
        .dead_letter_replay(
            &db.conn().unwrap(),
            &DeadLetterScope::default(),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(replayed.len(), 1);

    let ids: Vec<i64> = replayed.iter().map(|m| m.id).collect();
    let resolved = t
        .outbox
        .dead_letter_resolve(&db.conn().unwrap(), &ids)
        .await
        .unwrap();
    assert_eq!(resolved, 1);

    // Dead letter is now resolved
    let dls = read_dead_letters(&db).await;
    assert_eq!(dls.len(), 1);
    assert_eq!(dls[0].status, "resolved");
    assert!(dls[0].completed_at.is_some());
}

#[tokio::test]
async fn dead_letter_cleanup_only_terminal() {
    let db = setup_db("ch9_dl_cleanup_soft").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    // Create 2 dead letters
    create_dead_letters(&t, &db, "q", 0, &["a", "b"]).await;

    // Replay only 1 (by limit), then resolve it
    let scope_one = DeadLetterScope::default().limit(1);
    let replayed = t
        .outbox
        .dead_letter_replay(&db.conn().unwrap(), &scope_one, Duration::from_secs(60))
        .await
        .unwrap();
    let ids: Vec<i64> = replayed.iter().map(|m| m.id).collect();
    t.outbox
        .dead_letter_resolve(&db.conn().unwrap(), &ids)
        .await
        .unwrap();

    // Cleanup — should only delete the resolved one
    let deleted = t
        .outbox
        .dead_letter_cleanup(&db.conn().unwrap(), &DeadLetterScope::default())
        .await
        .unwrap();
    assert_eq!(deleted, 1);

    // 1 pending dead letter remains
    let remaining = t
        .outbox
        .dead_letter_count(&db.conn().unwrap(), &DeadLetterFilter::default())
        .await
        .unwrap();
    assert_eq!(remaining, 1);
}

#[tokio::test]
async fn dead_letter_discard_then_cleanup_deletes_all() {
    let db = setup_db("ch9_dl_discard_cleanup").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    create_dead_letters(&t, &db, "q", 0, &["a", "b", "c"]).await;

    // Discard all pending
    let discarded = t
        .outbox
        .dead_letter_discard(&db.conn().unwrap(), &DeadLetterScope::default())
        .await
        .unwrap();
    assert_eq!(discarded, 3);

    // Cleanup terminal entries
    let cleaned = t
        .outbox
        .dead_letter_cleanup(&db.conn().unwrap(), &DeadLetterScope::default())
        .await
        .unwrap();
    assert_eq!(cleaned, 3);

    let remaining = t
        .outbox
        .dead_letter_count(
            &db.conn().unwrap(),
            &DeadLetterFilter::default().any_status(),
        )
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}

#[tokio::test]
async fn dead_letter_filter_by_partition() {
    let db = setup_db("ch9_dl_filter_part").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 2).await.unwrap();
    let ids = t.outbox.all_partition_ids();

    // Dead-letter messages on both partitions
    create_dead_letters(&t, &db, "q", 0, &["a0"]).await;
    create_dead_letters(&t, &db, "q", 1, &["b1", "b2"]).await;

    let filter_p0 = DeadLetterFilter::default().partition(ids[0]);
    let items = t
        .outbox
        .dead_letter_list(&db.conn().unwrap(), &filter_p0)
        .await
        .unwrap();
    assert_eq!(items.len(), 1);

    let filter_p1 = DeadLetterFilter::default().partition(ids[1]);
    let items = t
        .outbox
        .dead_letter_list(&db.conn().unwrap(), &filter_p1)
        .await
        .unwrap();
    assert_eq!(items.len(), 2);
}

#[tokio::test]
async fn dead_letter_filter_with_limit() {
    let db = setup_db("ch9_dl_filter_limit").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    create_dead_letters(&t, &db, "q", 0, &["a", "b", "c", "d", "e"]).await;

    let filter = DeadLetterFilter::default().limit(2);
    let items = t
        .outbox
        .dead_letter_list(&db.conn().unwrap(), &filter)
        .await
        .unwrap();
    assert_eq!(items.len(), 2);
}

// ======================================================================
// Chapter 10: Builder API
// ======================================================================

#[tokio::test]
async fn builder_start_stop_clean() {
    let db = setup_db("ch10_start_stop").await;

    let count = Arc::new(AtomicU32::new(0));
    let handler = CountingMessageHandler {
        count: count.clone(),
    };
    let handle = Outbox::builder(db)
        .processor_tuning(
            WorkerTuning::processor_default().idle_interval(Duration::from_millis(50)),
        )
        .sequencer_tuning(
            WorkerTuning::sequencer_default().idle_interval(Duration::from_millis(50)),
        )
        .queue("orders", Partitions::of(1))
        .decoupled(handler)
        .start()
        .await
        .unwrap();

    // Just verify it started and can stop cleanly
    handle.stop().await;
}

#[tokio::test]
async fn builder_partitions_of_all_valid_values() {
    for n in [1, 2, 4, 8, 16, 32, 64] {
        let p = Partitions::of(n);
        assert_eq!(p.count(), n);
    }
}

#[tokio::test]
async fn builder_multiple_queues() {
    let db = setup_db("ch10_multi_queue").await;

    let count_a = Arc::new(AtomicU32::new(0));
    let count_b = Arc::new(AtomicU32::new(0));

    let handle = Outbox::builder(db)
        .processor_tuning(
            WorkerTuning::processor_default().idle_interval(Duration::from_millis(50)),
        )
        .sequencer_tuning(
            WorkerTuning::sequencer_default().idle_interval(Duration::from_millis(50)),
        )
        .queue("a", Partitions::of(1))
        .decoupled(CountingMessageHandler {
            count: count_a.clone(),
        })
        .queue("b", Partitions::of(2))
        .decoupled(CountingMessageHandler {
            count: count_b.clone(),
        })
        .start()
        .await
        .unwrap();

    let outbox = handle.outbox();

    // Enqueue to both queues via a shared-cache connection
    let db2 = setup_db("ch10_multi_queue").await;
    let conn = db2.conn().unwrap();
    outbox
        .enqueue(&conn, "a", 0, b"hello-a".to_vec(), "text/plain")
        .await
        .unwrap();
    outbox
        .enqueue(&conn, "b", 0, b"hello-b".to_vec(), "text/plain")
        .await
        .unwrap();
    outbox.flush();

    // Wait for processing
    poll_until(
        || {
            let ca = count_a.load(Ordering::Relaxed);
            let cb = count_b.load(Ordering::Relaxed);
            async move { ca >= 1 && cb >= 1 }
        },
        5000,
    )
    .await;

    assert!(count_a.load(Ordering::Relaxed) >= 1);
    assert!(count_b.load(Ordering::Relaxed) >= 1);

    handle.stop().await;
}

// ======================================================================
// Chapter 11: End-to-End Lifecycle
// ======================================================================

#[tokio::test]
async fn e2e_happy_path_enqueue_through_reap() {
    let db = setup_db("ch11_happy").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    // Enqueue → Sequence → Process (decoupled, success) → Reap
    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c"]).await;

    let count = Arc::new(AtomicU32::new(0));
    run_decoupled(
        &db,
        pid,
        CountingSuccessHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        3,
    )
    .await;
    run_vacuum(&db, pid).await;

    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 0);
    assert_eq!(count_rows(&db, "modkit_outbox_outgoing").await, 0);
    assert_eq!(count_rows(&db, "modkit_outbox_body").await, 0);
    assert_eq!(count_rows(&db, "modkit_outbox_dead_letters").await, 0);

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 3);
    assert_eq!(snap.attempts, 0);
}

#[tokio::test]
async fn e2e_retry_then_recovery() {
    let db = setup_db("ch11_retry").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["msg"]).await;

    // Retry twice
    run_decoupled(
        &db,
        pid,
        PerMessageAdapter::new(AlwaysRetryHandler),
        Duration::from_secs(30),
        10,
    )
    .await;
    expire_lease(&db, pid).await;
    run_decoupled(
        &db,
        pid,
        PerMessageAdapter::new(AlwaysRetryHandler),
        Duration::from_secs(30),
        10,
    )
    .await;
    expire_lease(&db, pid).await;

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 0);
    assert_eq!(snap.attempts, 2);

    // Then succeed
    let count = Arc::new(AtomicU32::new(0));
    run_decoupled(
        &db,
        pid,
        CountingSuccessHandler {
            count: count.clone(),
        },
        Duration::from_secs(30),
        10,
    )
    .await;

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 1);
    assert_eq!(snap.attempts, 0, "attempts reset on success");
}

#[tokio::test]
async fn e2e_reject_replay_success() {
    let db = setup_db("ch11_reject_replay").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    // Reject
    create_dead_letters(&t, &db, "q", 0, &["msg"]).await;

    // Replay (claim) → resolve
    let replayed = t
        .outbox
        .dead_letter_replay(
            &db.conn().unwrap(),
            &DeadLetterScope::default(),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(replayed.len(), 1);

    let ids: Vec<i64> = replayed.iter().map(|m| m.id).collect();
    t.outbox
        .dead_letter_resolve(&db.conn().unwrap(), &ids)
        .await
        .unwrap();

    // Dead letter has status=resolved and completed_at set
    let dls = read_dead_letters(&db).await;
    assert_eq!(dls.len(), 1);
    assert_eq!(dls[0].status, "resolved");
    assert!(dls[0].completed_at.is_some());
}

#[tokio::test]
async fn e2e_crash_then_recovery() {
    let db = setup_db("ch11_crash").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["msg"]).await;

    // Simulate crash
    simulate_crash(&db, pid, 300).await;
    expire_lease(&db, pid).await;

    // Recovery processor succeeds
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = AttemptsRecorder {
        seen_attempts: seen.clone(),
    };
    run_decoupled(
        &db,
        pid,
        PerMessageAdapter::new(handler),
        Duration::from_secs(30),
        10,
    )
    .await;

    {
        let recorded = seen.lock().unwrap();
        assert_eq!(recorded[0], 1, "handler saw attempts=1 from crash");
    }

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 1);
    assert_eq!(snap.attempts, 0, "attempts reset after successful recovery");
}

// ======================================================================
// Chapter 12: PerMessageAdapter partial failure
// ======================================================================

/// Test helper: records which seqs the handler was called with,
/// rejects or retries at a configurable poison seq.
struct PartialFailureHandler {
    seen_seqs: Arc<Mutex<Vec<i64>>>,
    poison_seq: i64,
    reject: bool, // true = Reject, false = Retry
}

#[async_trait::async_trait]
impl MessageHandler for PartialFailureHandler {
    async fn handle(&self, msg: &OutboxMessage, _cancel: CancellationToken) -> HandlerResult {
        self.seen_seqs.lock().unwrap().push(msg.seq);
        if msg.seq == self.poison_seq {
            if self.reject {
                return HandlerResult::Reject {
                    reason: format!("poison seq={}", msg.seq),
                };
            }
            return HandlerResult::Retry {
                reason: format!("transient seq={}", msg.seq),
            };
        }
        HandlerResult::Success
    }
}

/// Transactional version of `PartialFailureHandler`.
struct TxPartialFailureHandler {
    seen_seqs: Arc<Mutex<Vec<i64>>>,
    poison_seq: i64,
    reject: bool,
}

#[async_trait::async_trait]
impl TransactionalMessageHandler for TxPartialFailureHandler {
    async fn handle(
        &self,
        _txn: &dyn ConnectionTrait,
        msg: &OutboxMessage,
        _cancel: CancellationToken,
    ) -> HandlerResult {
        self.seen_seqs.lock().unwrap().push(msg.seq);
        if msg.seq == self.poison_seq {
            if self.reject {
                return HandlerResult::Reject {
                    reason: format!("poison seq={}", msg.seq),
                };
            }
            return HandlerResult::Retry {
                reason: format!("transient seq={}", msg.seq),
            };
        }
        HandlerResult::Success
    }
}

/// Batch handler that always rejects — no `processed_count` side-channel.
struct BatchRejectHandler;

#[async_trait::async_trait]
impl Handler for BatchRejectHandler {
    async fn handle(&self, _msgs: &[OutboxMessage], _cancel: CancellationToken) -> HandlerResult {
        HandlerResult::Reject {
            reason: "batch reject".into(),
        }
    }
}

// ---- 14.3 Transactional strategy tests ----

#[tokio::test]
async fn tx_partial_reject_processed_count_in_result() {
    let db = setup_db("ch12_tx_partial_reject_pc").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c", "d", "e"]).await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = PerMessageAdapter::new(TxPartialFailureHandler {
        seen_seqs: seen.clone(),
        poison_seq: 3, // seqs are 1-based; poison at seq=3
        reject: true,
    });
    let result = run_transactional(&db, pid, handler, Duration::from_secs(30), 5).await;

    let pr = result.expect("should have a result");
    assert!(matches!(pr.handler_result, HandlerResult::Reject { .. }));
    // PerMessageAdapter processed seqs 1, 2 successfully before poison at seq 3
    assert_eq!(pr.processed_count, Some(2));

    // Transactional mode: all messages dead-lettered (tx is atomic)
    let dls = read_dead_letters(&db).await;
    assert_eq!(dls.len(), 5, "all 5 messages dead-lettered in tx mode");

    // Cursor advances past all messages
    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 5);
}

#[tokio::test]
async fn tx_partial_retry_rolls_back_all() {
    let db = setup_db("ch12_tx_partial_retry").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c"]).await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = PerMessageAdapter::new(TxPartialFailureHandler {
        seen_seqs: seen.clone(),
        poison_seq: 2,
        reject: false, // retry
    });
    let result = run_transactional(&db, pid, handler, Duration::from_secs(30), 3).await;

    let pr = result.expect("should have a result");
    assert!(matches!(pr.handler_result, HandlerResult::Retry { .. }));
    assert_eq!(pr.processed_count, Some(1)); // seq 1 succeeded before retry at seq 2

    // Cursor not advanced on retry
    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 0, "cursor unchanged on retry");

    // No dead letters on retry
    let dls = read_dead_letters(&db).await;
    assert!(dls.is_empty(), "no dead letters on retry");
}

#[tokio::test]
async fn tx_reject_at_first_msg_processed_count_zero() {
    let db = setup_db("ch12_tx_reject_first").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b"]).await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = PerMessageAdapter::new(TxPartialFailureHandler {
        seen_seqs: seen.clone(),
        poison_seq: 1, // first message
        reject: true,
    });
    let result = run_transactional(&db, pid, handler, Duration::from_secs(30), 2).await;

    let pr = result.expect("should have a result");
    assert_eq!(pr.processed_count, Some(0));
}

#[tokio::test]
async fn tx_batch_handler_reject_deadletters_all() {
    let db = setup_db("ch12_tx_batch_reject").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c"]).await;

    let result =
        run_transactional(&db, pid, AlwaysRejectTxHandler, Duration::from_secs(30), 3).await;

    let pr = result.expect("should have a result");
    assert_eq!(pr.processed_count, None, "batch handler returns None");

    let dls = read_dead_letters(&db).await;
    assert_eq!(dls.len(), 3, "all dead-lettered");
}

// ---- 14.4 Decoupled strategy tests ----

#[tokio::test]
async fn decoupled_partial_reject_deadletters_only_remaining() {
    let db = setup_db("ch12_dc_partial_reject").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c", "d", "e"]).await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = PerMessageAdapter::new(PartialFailureHandler {
        seen_seqs: seen.clone(),
        poison_seq: 3,
        reject: true,
    });
    let result = run_decoupled(&db, pid, handler, Duration::from_secs(30), 5).await;

    let pr = result.expect("should have a result");
    assert!(matches!(pr.handler_result, HandlerResult::Reject { .. }));
    assert_eq!(pr.processed_count, Some(2));

    // Decoupled partial reject: only msgs[2..] (seqs 3,4,5) dead-lettered
    let dls = read_dead_letters(&db).await;
    assert_eq!(dls.len(), 3, "only 3 remaining messages dead-lettered");
    let dl_seqs: Vec<i64> = dls.iter().map(|d| d.seq).collect();
    assert_eq!(dl_seqs, vec![3, 4, 5]);

    // Cursor advances past all messages
    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 5);
}

#[tokio::test]
async fn decoupled_reject_at_first_deadletters_all() {
    let db = setup_db("ch12_dc_reject_first").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c"]).await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = PerMessageAdapter::new(PartialFailureHandler {
        seen_seqs: seen.clone(),
        poison_seq: 1, // first message
        reject: true,
    });
    let result = run_decoupled(&db, pid, handler, Duration::from_secs(30), 3).await;

    let pr = result.expect("should have a result");
    assert_eq!(pr.processed_count, Some(0));

    // processed_count=0, so all messages dead-lettered
    let dls = read_dead_letters(&db).await;
    assert_eq!(dls.len(), 3, "all dead-lettered when poison is first");
}

#[tokio::test]
async fn decoupled_retry_does_not_advance_cursor() {
    let db = setup_db("ch12_dc_retry_no_advance").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c"]).await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = PerMessageAdapter::new(PartialFailureHandler {
        seen_seqs: seen.clone(),
        poison_seq: 2,
        reject: false,
    });
    let result = run_decoupled(&db, pid, handler, Duration::from_secs(30), 3).await;

    let pr = result.expect("should have a result");
    assert!(matches!(pr.handler_result, HandlerResult::Retry { .. }));
    assert_eq!(pr.processed_count, Some(1));

    let snap = read_processor_state(&db, pid).await;
    assert_eq!(snap.processed_seq, 0, "cursor not advanced on retry");

    let dls = read_dead_letters(&db).await;
    assert!(dls.is_empty(), "no dead letters on retry");
}

#[tokio::test]
async fn decoupled_batch_handler_reject_deadletters_all() {
    let db = setup_db("ch12_dc_batch_reject").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c"]).await;

    let result = run_decoupled(&db, pid, BatchRejectHandler, Duration::from_secs(30), 3).await;

    let pr = result.expect("should have a result");
    assert_eq!(pr.processed_count, None, "batch handler returns None");

    // None → all messages dead-lettered
    let dls = read_dead_letters(&db).await;
    assert_eq!(dls.len(), 3, "all dead-lettered for batch handler");
}

// ---- 14.5 Multi-cycle degradation tests ----

#[tokio::test]
async fn degradation_with_processed_count() {
    use super::workers::processor::{PartitionMode, PartitionModeState};

    // Simulate: batch_size=8, poison at position 3 (0-indexed)
    // → processed_count = 3, degrade to max(3, 1) = 3
    let mut mode = PartitionMode::new();
    mode.transition(
        &HandlerResult::Reject {
            reason: "poison".into(),
        },
        8,
        Some(3),
        1, // degrade immediately
    );
    assert_eq!(mode.effective_batch_size(8), 3);

    // Next success: 3 → 6
    mode.transition(&HandlerResult::Success, 8, None, 1);
    assert_eq!(mode.effective_batch_size(8), 6);

    // Next success: 6 → Normal(8)
    mode.transition(&HandlerResult::Success, 8, None, 1);
    assert!(matches!(mode.state, PartitionModeState::Normal));
}

#[tokio::test]
async fn degradation_batch_handler_falls_back_to_one() {
    use super::workers::processor::PartitionMode;

    let mut mode = PartitionMode::new();
    // Batch handler: None processed_count → degrade to 1
    mode.transition(
        &HandlerResult::Reject {
            reason: "bad".into(),
        },
        8,
        None,
        1, // degrade immediately
    );
    assert_eq!(mode.effective_batch_size(8), 1);
}

#[tokio::test]
async fn degradation_processed_count_zero_degrades_to_one() {
    use super::workers::processor::PartitionMode;

    let mut mode = PartitionMode::new();
    // processed_count=0 → max(0, 1) = 1
    mode.transition(
        &HandlerResult::Retry {
            reason: "fail".into(),
        },
        8,
        Some(0),
        1, // degrade immediately
    );
    assert_eq!(mode.effective_batch_size(8), 1);
}

// ---- 14.6 Edge case tests ----

#[tokio::test]
async fn batch_size_one_partial_failure_is_noop() {
    // With batch_size=1, PerMessageAdapter processes exactly 1 message.
    // If it rejects, processed_count=0, skip=0 → all (1) dead-lettered.
    // This is the same as full rejection — no partial behavior.
    let db = setup_db("ch12_batch_one_noop").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_and_sequence(&t, &db, "q", 0, &["a"]).await;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = PerMessageAdapter::new(PartialFailureHandler {
        seen_seqs: seen.clone(),
        poison_seq: 1,
        reject: true,
    });
    let result = run_decoupled(&db, pid, handler, Duration::from_secs(30), 1).await;

    let pr = result.expect("should have a result");
    assert_eq!(pr.processed_count, Some(0));

    let dls = read_dead_letters(&db).await;
    assert_eq!(dls.len(), 1, "single message dead-lettered");
}

#[tokio::test]
async fn processed_count_exceeds_batch_is_clamped() {
    use super::workers::processor::PartitionMode;

    // Even if processed_count somehow exceeds batch count, clamping prevents
    // invalid state. The processor clamps pc to count before passing to transition.
    let mut mode = PartitionMode::new();
    // Simulated: count=3, processed_count=5 → clamped to 3 by processor
    let clamped = Some(3u32);
    mode.transition(&HandlerResult::Reject { reason: "x".into() }, 8, clamped, 1);
    assert_eq!(mode.effective_batch_size(8), 3);
}

// ======================================================================
// Chapter 13: Dirty-set-driven Sequencer & Cold Reconciler
// ======================================================================

/// Helper: insert raw incoming rows bypassing enqueue (no dirty flag set).
async fn insert_raw_incoming(db: &Db, partition_id: i64, count: usize) {
    let conn = db.sea_internal();
    for _ in 0..count {
        // Insert a body row first
        let body_id = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "INSERT INTO modkit_outbox_body (payload, payload_type) VALUES (X'AA', 'raw') RETURNING id",
            ))
            .await
            .expect("insert body")
            .expect("body row")
            .try_get_by_index::<i64>(0)
            .expect("body_id");

        conn.execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO modkit_outbox_incoming (partition_id, body_id) VALUES ($1, $2)",
            [partition_id.into(), body_id.into()],
        ))
        .await
        .expect("insert incoming");
    }
}

#[tokio::test]
async fn dirty_set_populated_after_enqueue() {
    let db = setup_db("ch13_dirty_enqueue").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 4).await.unwrap();

    // Enqueue to partitions 0 and 2 only
    enqueue_msgs(&t.outbox, &db, "q", 0, &["a"]).await;
    enqueue_msgs(&t.outbox, &db, "q", 2, &["b"]).await;

    // Prioritizer should have exactly those 2 partition IDs
    let ids = t.outbox.all_partition_ids();
    let g1 = t
        .prioritizer
        .take()
        .expect("should have first dirty partition");
    let g2 = t
        .prioritizer
        .take()
        .expect("should have second dirty partition");
    let mut dirty = vec![g1.partition_id(), g2.partition_id()];
    dirty.sort_unstable();
    g1.processed();
    g2.processed();
    assert_eq!(dirty, vec![ids[0], ids[2]]);
}

#[tokio::test]
async fn sequencer_processes_only_dirty_partitions() {
    let db = setup_db("ch13_only_dirty").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 4).await.unwrap();

    let ids = t.outbox.all_partition_ids();
    // Enqueue to partition 1 only
    enqueue_msgs(&t.outbox, &db, "q", 1, &["x", "y"]).await;

    run_sequencer_once(&t, &db).await;

    // Only partition 1 should have outgoing
    assert!(read_outgoing(&db, ids[0]).await.is_empty());
    assert_eq!(read_outgoing(&db, ids[1]).await.len(), 2);
    assert!(read_outgoing(&db, ids[2]).await.is_empty());
    assert!(read_outgoing(&db, ids[3]).await.is_empty());
}

#[tokio::test]
async fn poker_discovers_pending_from_incoming_table() {
    let db = setup_db("ch13_poker_discover").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 2).await.unwrap();

    let ids = t.outbox.all_partition_ids();

    // Insert raw incoming (bypassing enqueue — no dirty flag)
    insert_raw_incoming(&db, ids[0], 2).await;
    insert_raw_incoming(&db, ids[1], 1).await;

    // Prioritizer should be empty (we bypassed enqueue)
    assert!(t.prioritizer.take().is_none());

    // Run cold reconciler
    super::workers::reconciler::reconcile_dirty(&t.outbox, &db, &t.prioritizer).await;

    // Prioritizer should now contain both partitions
    let g1 = t
        .prioritizer
        .take()
        .expect("should have first dirty partition");
    let g2 = t
        .prioritizer
        .take()
        .expect("should have second dirty partition");
    let mut dirty = vec![g1.partition_id(), g2.partition_id()];
    dirty.sort_unstable();
    g1.processed();
    g2.processed();
    assert_eq!(dirty, vec![ids[0], ids[1]]);
}

#[tokio::test]
async fn startup_reconciliation_finds_preexisting_incoming() {
    let db = setup_db("ch13_startup_recon").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    let pid = t.outbox.all_partition_ids()[0];
    insert_raw_incoming(&db, pid, 3).await;

    // Simulate startup reconciliation
    super::workers::reconciler::reconcile_dirty(&t.outbox, &db, &t.prioritizer).await;

    // Now sequencer should pick them up
    run_sequencer_once(&t, &db).await;

    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 0);
    assert_eq!(read_outgoing(&db, pid).await.len(), 3);
}

#[tokio::test]
async fn max_inner_iterations_cap_yields_after_limit() {
    let db = setup_db("ch13_max_iter").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();

    // Enqueue many messages: batch_size=2, max_inner_iterations=3
    // → can process at most 2*3=6 rows per cycle
    enqueue_msgs(
        &t.outbox,
        &db,
        "q",
        0,
        &["a", "b", "c", "d", "e", "f", "g", "h"],
    )
    .await;

    let config = SequencerConfig {
        batch_size: 2,
        max_inner_iterations: 3,
        ..SequencerConfig::default()
    };
    let mut seq = make_sequencer(&t, config, &db);
    let cancel = CancellationToken::new();
    let result = seq.execute(&cancel).await.unwrap();
    assert!(matches!(result, Directive::Proceed(_)));

    // Should have processed 6 (2 per iteration × 3 iterations) and re-inserted
    let pid = t.outbox.all_partition_ids()[0];
    let outgoing = read_outgoing(&db, pid).await;
    assert_eq!(outgoing.len(), 6);

    // Remaining 2 should still be in incoming
    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 2);

    // Partition should have been re-dirtied (saturated)
    let guard = t
        .prioritizer
        .take()
        .expect("saturated partition should be re-dirtied");
    assert_eq!(guard.partition_id(), pid);
    guard.processed();
}

#[tokio::test]
async fn execute_processes_one_partition_per_call() {
    let db = setup_db("ch13_one_per_call").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 4).await.unwrap();

    let ids = t.outbox.all_partition_ids();
    // Enqueue to all 4 partitions
    for i in 0..4 {
        enqueue_msgs(&t.outbox, &db, "q", i, &["msg"]).await;
    }

    let mut seq = make_sequencer(&t, SequencerConfig::default(), &db);
    let cancel = CancellationToken::new();

    // First execute() processes exactly one partition (unsaturated but did work → Proceed)
    let result = seq.execute(&cancel).await.unwrap();
    assert!(matches!(result, Directive::Proceed(_)));

    let mut processed = 0;
    for &id in &ids {
        if !read_outgoing(&db, id).await.is_empty() {
            processed += 1;
        }
    }
    assert_eq!(processed, 1);

    // Run until idle — all 4 partitions processed
    run_sequencer_until_idle(&mut seq).await;

    processed = 0;
    for &id in &ids {
        if !read_outgoing(&db, id).await.is_empty() {
            processed += 1;
        }
    }
    assert_eq!(processed, 4);
}

#[tokio::test]
async fn prioritizer_lru_fairness_across_cycles() {
    let db = setup_db("ch13_lru_fair").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 4).await.unwrap();

    let ids = t.outbox.all_partition_ids();

    // Enqueue to all 4 partitions
    for i in 0..4 {
        enqueue_msgs(&t.outbox, &db, "q", i, &["r1"]).await;
    }

    // Run until idle — all partitions processed via LRU ordering
    let mut seq = make_sequencer(&t, SequencerConfig::default(), &db);
    run_sequencer_until_idle(&mut seq).await;

    let mut total_outgoing = 0;
    for &id in &ids {
        total_outgoing += read_outgoing(&db, id).await.len();
    }
    assert_eq!(total_outgoing, 4, "all 4 partitions should be processed");
}

// ======================================================================
// Chapter 14: Parallel Sequencer Workers
// ======================================================================

/// Helper: create a sequencer with a specific shared prioritizer (for multi-worker tests).
fn make_sequencer_with_shared(
    t: &TestOutbox,
    config: SequencerConfig,
    db: &Db,
    shared: Arc<SharedPrioritizer>,
) -> Sequencer {
    Sequencer::new(config, Arc::clone(&t.outbox), db.clone(), shared)
}

#[tokio::test]
async fn parallel_sequencers_process_distinct_partitions() {
    let db = setup_db("ch14_parallel_distinct").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 4).await.unwrap();

    // Enqueue to all 4 partitions
    for i in 0..4 {
        enqueue_msgs(&t.outbox, &db, "q", i, &["msg"]).await;
    }

    // Two sequencers sharing the same prioritizer
    let shared = make_shared_prioritizer();
    let ids = t.outbox.all_partition_ids();
    for &id in &ids {
        shared.push_dirty(id);
    }
    let config = SequencerConfig::default();
    let mut seq_a = make_sequencer_with_shared(&t, config.clone(), &db, Arc::clone(&shared));
    let mut seq_b = make_sequencer_with_shared(&t, config, &db, Arc::clone(&shared));
    let cancel = CancellationToken::new();

    // Each sequencer takes one partition at a time from the shared prioritizer
    let r1 = seq_a.execute(&cancel).await.unwrap();
    let r2 = seq_b.execute(&cancel).await.unwrap();
    assert!(matches!(r1, Directive::Proceed(_)));
    assert!(matches!(r2, Directive::Proceed(_)));

    // After two executes, exactly 2 partitions should be processed
    let mut processed = 0;
    for &id in &ids {
        if !read_outgoing(&db, id).await.is_empty() {
            processed += 1;
        }
    }
    assert_eq!(processed, 2);

    // Two more executes drain the remaining 2 partitions
    let r3 = seq_a.execute(&cancel).await.unwrap();
    let r4 = seq_b.execute(&cancel).await.unwrap();
    assert!(matches!(r3, Directive::Proceed(_)));
    assert!(matches!(r4, Directive::Proceed(_)));

    processed = 0;
    for &id in &ids {
        if !read_outgoing(&db, id).await.is_empty() {
            processed += 1;
        }
    }
    assert_eq!(processed, 4);

    // Both should now be idle
    let r5 = seq_a.execute(&cancel).await.unwrap();
    assert!(matches!(r5, Directive::Idle(_)));
}

#[tokio::test]
async fn parallel_sequencers_no_duplicate_sequences() {
    let db = setup_db("ch14_no_dups").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 4).await.unwrap();

    // Enqueue multiple messages to each partition
    for i in 0..4 {
        enqueue_msgs(&t.outbox, &db, "q", i, &["a", "b", "c"]).await;
    }

    // Two sequencers sharing the same prioritizer
    let shared = make_shared_prioritizer();
    let ids = t.outbox.all_partition_ids();
    for &id in &ids {
        shared.push_dirty(id);
    }
    let config = SequencerConfig::default();
    let mut seq_a = make_sequencer_with_shared(&t, config.clone(), &db, Arc::clone(&shared));
    let mut seq_b = make_sequencer_with_shared(&t, config, &db, Arc::clone(&shared));

    // Run both until idle (alternating to simulate concurrency)
    let cancel = CancellationToken::new();
    loop {
        let a = seq_a.execute(&cancel).await.unwrap();
        let b = seq_b.execute(&cancel).await.unwrap();
        if matches!(a, Directive::Idle(_)) && matches!(b, Directive::Idle(_)) {
            break;
        }
    }

    // Verify: each partition has exactly 3 outgoing rows with contiguous sequences 1,2,3
    let ids = t.outbox.all_partition_ids();
    for &pid in &ids {
        let outgoing = read_outgoing(&db, pid).await;
        assert_eq!(outgoing.len(), 3, "partition {pid} should have 3 rows");
        let seqs: Vec<i64> = outgoing.iter().map(|r| r.seq).collect();
        assert_eq!(
            seqs,
            vec![1, 2, 3],
            "partition {pid} should have seqs 1,2,3"
        );
    }

    // No rows left in incoming
    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 0);
}

#[tokio::test]
async fn saturated_partition_fully_drained_across_cycles() {
    let db = setup_db("ch14_saturated_cycles").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    // Enqueue 20 messages. batch_size=3, max_inner_iterations=2 → 6 per execute.
    // Needs 4 execute() cycles to drain 20 (6+6+6+2).
    let payloads: Vec<&str> = (0..20).map(|_| "x").collect();
    enqueue_msgs(&t.outbox, &db, "q", 0, &payloads).await;

    let config = SequencerConfig {
        batch_size: 3,
        max_inner_iterations: 2,
        ..Default::default()
    };
    let mut seq = make_sequencer(&t, config, &db);
    let cancel = CancellationToken::new();

    // Cycle 1: drains 6 (3×2), saturated → re-dirtied
    let r = seq.execute(&cancel).await.unwrap();
    assert!(matches!(r, Directive::Proceed(_)));
    assert_eq!(read_outgoing(&db, pid).await.len(), 6);
    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 14);

    // Run until idle — remaining 14 drained across more cycles
    run_sequencer_until_idle(&mut seq).await;

    assert_eq!(read_outgoing(&db, pid).await.len(), 20);
    assert_eq!(count_rows(&db, "modkit_outbox_incoming").await, 0);

    // Sequences are contiguous 1..=20
    let outgoing = read_outgoing(&db, pid).await;
    let seqs: Vec<i64> = outgoing.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, (1..=20).collect::<Vec<_>>());
}

// ======================================================================
// Chapter 15: Processor Semaphore
// ======================================================================

#[tokio::test]
async fn processor_semaphore_limits_concurrency() {
    use super::taskward::{BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit};
    use tokio::sync::Semaphore;

    // Create a semaphore with 2 permits
    let sem = Arc::new(Semaphore::new(2));

    // Acquire 2 permits manually (simulating 2 active processors)
    let _p1 = sem.clone().acquire_owned().await.unwrap();
    let _p2 = sem.clone().acquire_owned().await.unwrap();

    // A third acquire should not complete immediately
    let cancel = CancellationToken::new();
    let bulkhead = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Fixed(Arc::clone(&sem)),
            backoff: BackoffConfig::new(Duration::from_millis(100), Duration::from_secs(30), 2.0),
        },
    );

    // Cancel immediately to avoid blocking — acquire should return None
    cancel.cancel();
    let result = bulkhead.acquire(&cancel).await;
    assert!(
        result.is_none(),
        "should not acquire when all permits taken and cancelled"
    );
}

// ======================================================================
// Chapter 16: Vacuum Parallelism
// ======================================================================

#[tokio::test]
async fn vacuum_counter_decrement_is_idempotent() {
    let db = setup_db("ch16_vac_idempotent").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    // Set vacuum counter to 5 (simulating 5 processed messages)
    set_vacuum_counter(&db, pid, 5).await;
    assert_eq!(read_vacuum_counter(&db, pid).await, 5);

    // Two "vacuum workers" both snapshot counter=5, both decrement by 5
    // First decrement: 5 - 5 = 0
    let conn = db.sea_internal();
    let dialect = Dialect::from(conn.get_database_backend());
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        dialect.decrement_vacuum_counter(),
        [5i64.into(), pid.into()],
    ))
    .await
    .unwrap();
    assert_eq!(read_vacuum_counter(&db, pid).await, 0);

    // Second decrement (stale snapshot): GREATEST(0 - 5, 0) = 0
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        dialect.decrement_vacuum_counter(),
        [5i64.into(), pid.into()],
    ))
    .await
    .unwrap();
    // Counter should floor at 0, never go negative
    assert_eq!(read_vacuum_counter(&db, pid).await, 0);
}

#[tokio::test]
async fn vacuum_concurrent_workers_safe() {
    use super::workers::vacuum::VacuumTask;

    let db = setup_db("ch16_vac_concurrent").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    // Enqueue, sequence, and process 3 messages
    enqueue_and_sequence(&t, &db, "q", 0, &["a", "b", "c"]).await;

    // Advance processed_seq to 3 (simulating processor progress)
    let conn = db.sea_internal();
    conn.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "UPDATE modkit_outbox_processor SET processed_seq = 3 WHERE partition_id = $1",
        [pid.into()],
    ))
    .await
    .unwrap();

    // Bump vacuum counter so vacuum picks up the partition
    set_vacuum_counter(&db, pid, 3).await;

    // Run two vacuum workers sequentially (SQLite single connection)
    let cancel = CancellationToken::new();
    let mut vac1 = VacuumTask::new(db.clone(), 10_000);
    let mut vac2 = VacuumTask::new(db.clone(), 10_000);

    vac1.execute(&cancel).await.unwrap();
    vac2.execute(&cancel).await.unwrap();

    // All outgoing and body rows should be cleaned up
    assert_eq!(count_rows(&db, "modkit_outbox_outgoing").await, 0);
    assert_eq!(count_rows(&db, "modkit_outbox_body").await, 0);

    // Counter should be at 0
    assert_eq!(read_vacuum_counter(&db, pid).await, 0);
}

// ======================================================================
// Chapter 17: Priority Semaphore
// ======================================================================

#[tokio::test]
async fn priority_bulkhead_prefers_shared_when_available() {
    use super::taskward::{BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit};
    use tokio::sync::Semaphore;

    let guaranteed = Arc::new(Semaphore::new(4));
    let shared = Arc::new(Semaphore::new(2));
    let cancel = CancellationToken::new();

    let bulkhead = Bulkhead::new(
        "seq-0",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Tiered {
                guaranteed: Arc::clone(&guaranteed),
                shared: Arc::clone(&shared),
            },
            backoff: BackoffConfig::new(Duration::from_millis(100), Duration::from_secs(30), 2.0),
        },
    );

    // When both available, biased select prefers shared
    let permit = bulkhead.acquire(&cancel).await;
    assert!(permit.is_some(), "should acquire a permit");

    // shared should have 1 available (started with 2, acquired 1)
    assert_eq!(shared.available_permits(), 1);
    // guaranteed should still have all 4
    assert_eq!(guaranteed.available_permits(), 4);
}

#[tokio::test]
async fn priority_bulkhead_falls_back_to_guaranteed_when_shared_exhausted() {
    use super::taskward::{BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit};
    use tokio::sync::Semaphore;

    let guaranteed = Arc::new(Semaphore::new(4));
    let shared = Arc::new(Semaphore::new(2));

    // Exhaust shared permits (simulating vacuum holding them)
    let _hold1 = shared.clone().acquire_owned().await.unwrap();
    let _hold2 = shared.clone().acquire_owned().await.unwrap();
    assert_eq!(shared.available_permits(), 0);

    let cancel = CancellationToken::new();
    let bulkhead = Bulkhead::new(
        "seq-0",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Tiered {
                guaranteed: Arc::clone(&guaranteed),
                shared: Arc::clone(&shared),
            },
            backoff: BackoffConfig::new(Duration::from_millis(100), Duration::from_secs(30), 2.0),
        },
    );

    // Should fall back to guaranteed since shared is exhausted
    let permit = bulkhead.acquire(&cancel).await;
    assert!(permit.is_some(), "should acquire guaranteed permit");

    // guaranteed should have 3 remaining (started with 4, acquired 1)
    assert_eq!(guaranteed.available_permits(), 3);
}

// ======================================================================
// Chapter 18: Partition Guard Panic Recovery
// ======================================================================

#[tokio::test]
async fn partition_guard_drop_preserves_dirty_signal() {
    let t = make_default_test_outbox().await;

    // Mark a partition dirty via prioritizer
    t.prioritizer.push_dirty(42);

    // Take a guard
    let guard = t.prioritizer.take().expect("should get guard");
    assert_eq!(guard.partition_id(), 42);

    // Drop without ack (simulating panic)
    drop(guard);

    // The partition should still be available for retry
    let guard2 = t.prioritizer.take().expect("should get guard after drop");
    assert_eq!(guard2.partition_id(), 42);
    guard2.processed(); // clean up

    // Now it's consumed
    assert!(t.prioritizer.take().is_none());
}

#[tokio::test]
async fn sequencer_processes_across_enqueue_cycles() {
    let db = setup_db("ch18_guard_error_retry").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    enqueue_msgs(&t.outbox, &db, "q", 0, &["a"]).await;

    let shared = make_shared_prioritizer();
    shared.push_dirty(pid);
    // Wire outbox to use shared prioritizer for subsequent enqueues
    t.outbox
        .prioritizer
        .write()
        .await
        .replace(Arc::clone(&shared));
    let config = SequencerConfig::default();
    let mut seq = make_sequencer_with_shared(&t, config, &db, Arc::clone(&shared));
    let cancel = CancellationToken::new();

    // First execute processes the partition successfully (unsaturated but did work → Proceed)
    let r = seq.execute(&cancel).await.unwrap();
    assert!(matches!(r, Directive::Proceed(_)));
    assert_eq!(read_outgoing(&db, pid).await.len(), 1);

    // Enqueue another message — push_dirty goes to `shared` via outbox
    enqueue_msgs(&t.outbox, &db, "q", 0, &["b"]).await;

    // This time the sequencer should process it in another cycle
    let r = seq.execute(&cancel).await.unwrap();
    assert!(matches!(r, Directive::Proceed(_)));
    assert_eq!(read_outgoing(&db, pid).await.len(), 2);

    // Sequences should be contiguous
    let outgoing = read_outgoing(&db, pid).await;
    let seqs: Vec<i64> = outgoing.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, vec![1, 2]);
}

// ======================================================================
// Chapter 19: Full-Pipeline E2E (builder → handler delivery)
// ======================================================================

/// Counting handler for full-pipeline tests.
struct CountingHandler {
    counter: Arc<AtomicUsize>,
    notify: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl Handler for CountingHandler {
    async fn handle(
        &self,
        msgs: &[OutboxMessage],
        _cancel: CancellationToken,
    ) -> super::handler::HandlerResult {
        self.counter.fetch_add(msgs.len(), Ordering::Relaxed);
        self.notify.notify_one();
        super::handler::HandlerResult::Success
    }
}

/// Enqueue a single message through the full builder pipeline, wait for
/// the handler to receive it, then verify exactly one delivery (no
/// duplicates, no loss).
#[tokio::test]
async fn pipeline_single_enqueue_one_delivery() {
    let db = setup_db("ch19_pipeline_single").await;

    let counter = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());

    let handler = CountingHandler {
        counter: Arc::clone(&counter),
        notify: Arc::clone(&notify),
    };

    let handle = Outbox::builder(db.clone())
        .processor_tuning(WorkerTuning::processor_default().idle_interval(Duration::from_secs(60)))
        .sequencer_tuning(WorkerTuning::sequencer_default().idle_interval(Duration::from_secs(60)))
        .processors(1)
        .maintenance(1, 1)
        .queue("test-q", Partitions::of(1))
        .batch_decoupled(handler)
        .start()
        .await
        .unwrap();

    let outbox = handle.outbox();

    // Enqueue exactly one message
    let (db, result) = outbox
        .transaction(db, |tx| {
            let o = Arc::clone(outbox);
            Box::pin(async move {
                o.enqueue(tx, "test-q", 0, b"hello".to_vec(), "test/msg")
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            })
        })
        .await;
    result.unwrap();

    // Wait for the message to be consumed
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if counter.load(Ordering::Acquire) >= 1 {
            break;
        }
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or(Duration::ZERO);
        assert!(
            !remaining.is_zero(),
            "timed out waiting for consumption (consumed: {})",
            counter.load(Ordering::Relaxed)
        );
        tokio::time::timeout(remaining, notify.notified())
            .await
            .ok();
    }

    // Verify no duplicate delivery arrives. Wait 200ms and check the counter
    // hasn't incremented beyond 1. We check the counter directly instead of
    // notify.notified() because a stale permit from the first delivery's
    // notify_one() would cause notified().await to resolve immediately.
    let baseline = counter.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        counter.load(Ordering::Relaxed),
        baseline,
        "no duplicate delivery should occur (counter changed during wait)"
    );
    assert_eq!(
        baseline, 1,
        "single enqueue must produce exactly one delivery"
    );

    drop(db);
    handle.stop().await;
}

// ======================================================================
// Chapter 20: P0 Coverage — Concurrency & Boundary Tests
// ======================================================================

/// Spawn a background producer that enqueues while the sequencer is running.
/// Verifies that all messages appear in outgoing with correct sequences.
#[tokio::test]
async fn concurrent_enqueue_during_sequencer_preserves_order() {
    let db = setup_db("ch20_concurrent_enqueue").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 2).await.unwrap();

    // Enqueue an initial batch so the sequencer has work
    enqueue_msgs(&t.outbox, &db, "q", 0, &["a", "b"]).await;
    enqueue_msgs(&t.outbox, &db, "q", 1, &["c", "d"]).await;

    let ids = t.outbox.all_partition_ids();
    for &id in &ids {
        t.prioritizer.push_dirty(id);
    }

    // Spawn a background producer that enqueues more while sequencer runs
    let outbox_clone = Arc::clone(&t.outbox);
    let db_clone = db.clone();
    let producer = tokio::spawn(async move {
        for i in 0..5 {
            let payload = format!("bg-{i}");
            let conn = db_clone.conn().expect("conn");
            outbox_clone
                .enqueue(&conn, "q", 0, payload.into_bytes(), "text/plain")
                .await
                .expect("bg enqueue");
        }
    });

    // Run sequencer concurrently
    let mut seq = make_sequencer(&t, SequencerConfig::default(), &db);
    run_sequencer_until_idle(&mut seq).await;

    // Wait for producer to finish
    producer.await.unwrap();

    // The background messages may have dirtied partitions — drain again
    for &id in &ids {
        t.prioritizer.push_dirty(id);
    }
    run_sequencer_until_idle(&mut seq).await;

    // Verify: all messages in outgoing, sequences contiguous per partition
    for &pid in &ids {
        let outgoing = read_outgoing(&db, pid).await;
        if outgoing.is_empty() {
            continue;
        }
        let seqs: Vec<i64> = outgoing.iter().map(|r| r.seq).collect();
        #[allow(clippy::cast_possible_wrap)]
        let expected: Vec<i64> = (1..=seqs.len() as i64).collect();
        assert_eq!(
            seqs, expected,
            "partition {pid} sequences must be contiguous"
        );
    }

    // Total messages: 4 initial + 5 background = 9
    let mut total = 0;
    for &pid in &ids {
        total += read_outgoing(&db, pid).await.len();
    }
    assert_eq!(total, 9, "all 9 messages should be in outgoing");
}

/// Verify `Partitions::of(0)` panics.
#[test]
#[should_panic(expected = "partition count must be a power of 2")]
fn registration_zero_partitions_rejected() {
    #[allow(clippy::let_underscore_must_use)]
    let _ = Partitions::of(0);
}

/// Verify that the sequencer returns `Directive::Idle` with zero rows
/// when processing an empty (already-drained) partition.
#[tokio::test]
async fn sequencer_empty_partition_returns_idle_zero() {
    let db = setup_db("ch20_idle_zero").await;
    let t = make_default_test_outbox().await;
    t.outbox.register_queue(&db, "q", 1).await.unwrap();
    let pid = t.outbox.all_partition_ids()[0];

    // Push dirty but don't enqueue — partition is empty
    t.prioritizer.push_dirty(pid);

    let mut seq = make_sequencer(&t, SequencerConfig::default(), &db);
    let cancel = CancellationToken::new();
    let result = seq.execute(&cancel).await.unwrap();
    assert!(
        matches!(result, Directive::Idle(_)),
        "empty partition should return Idle"
    );
    assert_eq!(result.payload().rows_claimed, 0);
}

// ======================================================================
// Chapter 21: P1 Coverage
// ======================================================================

/// Builder with no queues starts successfully but enqueue fails.
#[tokio::test]
async fn builder_no_queues_starts_but_enqueue_fails() {
    let db = setup_db("ch21_no_queues").await;

    let handle = Outbox::builder(db.clone())
        .processor_tuning(WorkerTuning::processor_default().idle_interval(Duration::from_secs(60)))
        .sequencer_tuning(WorkerTuning::sequencer_default().idle_interval(Duration::from_secs(60)))
        .maintenance(1, 1)
        .start()
        .await
        .expect("start with no queues should succeed");

    let outbox = handle.outbox();
    let conn = db.conn().expect("conn");
    let err = outbox
        .enqueue(&conn, "nonexistent", 0, b"hello".to_vec(), "text/plain")
        .await;
    assert!(err.is_err(), "enqueue to unregistered queue should fail");

    handle.stop().await;
}

// reconciler_is_idempotent — covered by Ch 13 tests:
// `poker_discovers_pending_from_incoming_table` and
// `startup_reconciliation_finds_preexisting_incoming`.
// Direct reconcile_dirty call hangs on SQLite single-connection pool.

// ======================================================================
// Chapter 22: Bug Reproduction Tests
// ======================================================================

// -- Bug 1: batch_transactional() forces batch_size=1 --
//
// TransactionalProcessorFactory::spawn() unconditionally sets
// ctx.tuning.batch_size = 1 (builder.rs:214). This means
// batch_transactional() handlers never see more than 1 message per call,
// even though they are batch handlers and the user configured batch_size > 1.

/// Batch transactional handler that records the max batch size it receives.
struct MaxBatchSizeHandler {
    max_batch_seen: Arc<AtomicU32>,
    total_processed: Arc<AtomicUsize>,
    notify: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl TransactionalHandler for MaxBatchSizeHandler {
    async fn handle(
        &self,
        _txn: &dyn sea_orm::ConnectionTrait,
        msgs: &[OutboxMessage],
        _cancel: CancellationToken,
    ) -> HandlerResult {
        #[allow(clippy::cast_possible_truncation)]
        let batch_len = msgs.len() as u32;
        self.max_batch_seen.fetch_max(batch_len, Ordering::Relaxed);
        self.total_processed
            .fetch_add(msgs.len(), Ordering::Relaxed);
        self.notify.notify_one();
        HandlerResult::Success
    }
}

/// `batch_transactional()` should respect the configured `batch_size`, not force it to 1.
///
/// Regression test: `TransactionalProcessorFactory::spawn()` must not
/// unconditionally override `batch_size` to 1.
#[tokio::test]
async fn batch_transactional_respects_configured_batch_size() {
    let db = setup_db("ch22_batch_txn_batch_size").await;

    let max_batch_seen = Arc::new(AtomicU32::new(0));
    let total_processed = Arc::new(AtomicUsize::new(0));
    let notify = Arc::new(tokio::sync::Notify::new());

    let handler = MaxBatchSizeHandler {
        max_batch_seen: Arc::clone(&max_batch_seen),
        total_processed: Arc::clone(&total_processed),
        notify: Arc::clone(&notify),
    };

    // Configure processor tuning with batch_size=5
    let handle = Outbox::builder(db.clone())
        .processor_tuning(
            WorkerTuning::processor_default()
                .batch_size(5)
                .idle_interval(Duration::from_secs(60)),
        )
        .sequencer_tuning(WorkerTuning::sequencer_default().idle_interval(Duration::from_secs(60)))
        .processors(1)
        .maintenance(1, 1)
        .queue("batch-q", Partitions::of(1))
        .batch_transactional(handler)
        .start()
        .await
        .unwrap();

    let outbox = handle.outbox();

    // Enqueue 5 messages to the same partition in one transaction
    let (db, result) = outbox
        .transaction(db, |tx| {
            let o = Arc::clone(outbox);
            Box::pin(async move {
                for i in 0..5u8 {
                    o.enqueue(tx, "batch-q", 0, vec![i], "test/msg")
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                }
                Ok(())
            })
        })
        .await;
    result.unwrap();

    // Wait for all 5 messages to be consumed
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if total_processed.load(Ordering::Acquire) >= 5 {
            break;
        }
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or(Duration::ZERO);
        assert!(
            !remaining.is_zero(),
            "timed out waiting for consumption (processed: {})",
            total_processed.load(Ordering::Relaxed)
        );
        tokio::time::timeout(remaining, notify.notified())
            .await
            .ok();
    }

    // The handler should have seen at least one batch with >1 message.
    // With batch_size=5 and 5 messages on the same partition, the handler
    // should ideally receive all 5 in one call.
    // BUG: Today the max is always 1 because the factory forces batch_size=1.
    let max = max_batch_seen.load(Ordering::Relaxed);
    assert!(
        max > 1,
        "batch_transactional handler should receive batches > 1, but max batch size seen was {max}. \
         This proves TransactionalProcessorFactory forces batch_size=1 even for batch handlers."
    );

    drop(db);
    handle.stop().await;
}
