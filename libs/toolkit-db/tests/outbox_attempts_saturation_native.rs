#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(all(feature = "integration", any(feature = "pg", feature = "mysql")))]

//! Native (PG/`MySQL`) integration test for the processor's `attempts` counter.
//!
//! `attempts` is a `SMALLINT`, and it counts every pass of a partition whose
//! handler keeps returning `Retry` - through a days-long downstream outage, say -
//! until the cursor moves again. An increment past 32767 fails its statement
//! (`smallint out of range` on `Postgres`, error 1264 on strict-mode `MySQL`). In
//! leased mode that increment is the lease acquire itself, so the handler never
//! runs again and the partition stays stuck after the outage is over. In
//! transactional mode it is the retry record, so every retry at the limit is
//! rolled back as a worker error. The counter must saturate instead.
//!
//! The in-process test suite runs on `SQLite` only, so it never executes these
//! dialects' statements. This test drives the real pipeline against a real
//! server, with the counter put at the limit by hand.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::outbox::{
    HandlerResult, LeasedMessageHandler, MessageResult, Outbox, OutboxMessage, Partitions, Record,
    TransactionalMessageHandler, WorkerTuning, outbox_migrations,
};
use toolkit_db::{ConnectOpts, Db, connect_db};

const PROCESSOR_TABLE: &str = "toolkit_outbox_processor";

/// Retry reason of the transactional handler, read back from `last_error`.
const DOWNSTREAM_DOWN: &str = "downstream unavailable";

/// Records the `attempts` of every delivery, and retries until `let_through`
/// is set, then acks.
struct GatedHandler {
    let_through: Arc<AtomicBool>,
    seen_attempts: Arc<Mutex<Vec<i16>>>,
}

#[async_trait::async_trait]
impl LeasedMessageHandler for GatedHandler {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        self.seen_attempts.lock().unwrap().push(msg.attempts);
        if self.let_through.load(Ordering::Acquire) {
            MessageResult::Ok
        } else {
            MessageResult::Retry
        }
    }
}

/// Retries every message until `let_through` is set, then acks it.
struct GatedTxHandler {
    let_through: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl TransactionalMessageHandler for GatedTxHandler {
    async fn handle(
        &self,
        _txn: &sea_orm::DatabaseExecutor<'_>,
        _msg: &OutboxMessage,
    ) -> HandlerResult {
        if self.let_through.load(Ordering::Acquire) {
            HandlerResult::Success
        } else {
            HandlerResult::Retry {
                reason: DOWNSTREAM_DOWN.into(),
            }
        }
    }
}

#[derive(Debug)]
struct ProcessorState {
    processed_seq: i64,
    attempts: i16,
    last_error: Option<String>,
}

async fn read_processor(raw: &DatabaseConnection) -> ProcessorState {
    let row = raw
        .query_one_raw(Statement::from_string(
            raw.get_database_backend(),
            format!("SELECT processed_seq, attempts, last_error FROM {PROCESSOR_TABLE}"),
        ))
        .await
        .expect("read processor row")
        .expect("processor row exists");
    ProcessorState {
        processed_seq: row.try_get("", "processed_seq").expect("processed_seq"),
        attempts: row.try_get("", "attempts").expect("attempts"),
        last_error: row.try_get("", "last_error").expect("last_error"),
    }
}

/// Put the counter of the only partition at the limit, as a long run of
/// retries would.
async fn saturate_attempts(raw: &DatabaseConnection) {
    let res = raw
        .execute_unprepared(&format!(
            "UPDATE {PROCESSOR_TABLE} SET attempts = {}",
            i16::MAX
        ))
        .await
        .expect("saturate attempts");
    assert_eq!(res.rows_affected(), 1, "one partition to saturate");
}

async fn enqueue_one(outbox: &Outbox, db: &Db) {
    let conn = db.conn().expect("conn");
    outbox
        .enqueue(
            &conn,
            Record::to("q", 0)
                .payload(b"msg".to_vec(), "text/plain")
                .build()
                .unwrap(),
        )
        .await
        .expect("enqueue")
        .fire();
}

const DEADLINE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(50);

/// Poll until `done` holds, failing with `what` at the deadline.
async fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(POLL).await;
    }
}

/// Poll the processor row until `done` holds, failing with `what` at the deadline.
async fn wait_for_processor(
    raw: &DatabaseConnection,
    what: &str,
    done: impl Fn(&ProcessorState) -> bool,
) -> ProcessorState {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let state = read_processor(raw).await;
        if done(&state) {
            return state;
        }
        assert!(Instant::now() < deadline, "{what} (last state: {state:?})");
        tokio::time::sleep(POLL).await;
    }
}

/// Leased mode: a retry at the limit still takes the lease and reaches the
/// handler, and the partition delivers once the downstream recovers.
async fn a_saturated_leased_partition_still_delivers(url: &str) {
    let db = connect_db(url, ConnectOpts::default())
        .await
        .expect("connect");
    run_migrations_for_testing(&db, outbox_migrations())
        .await
        .expect("migrations");
    let raw = sea_orm::Database::connect(url).await.expect("raw connect");

    let let_through = Arc::new(AtomicBool::new(false));
    let seen_attempts = Arc::new(Mutex::new(Vec::new()));
    let handle = Outbox::builder(db.clone())
        .processors(1)
        .maintenance(1, 1)
        .processor_tuning(
            WorkerTuning::processor()
                .retry_base(Duration::from_millis(50))
                .retry_max(Duration::from_millis(200)),
        )
        .queue("q", Partitions::of(1))
        .leased(GatedHandler {
            let_through: Arc::clone(&let_through),
            seen_attempts: Arc::clone(&seen_attempts),
        })
        .done()
        .start()
        .await
        .expect("start outbox");

    enqueue_one(handle.outbox(), &db).await;

    // Saturate only once the handler is retrying the message. A pass that
    // finds no messages releases the lease with the counter reset (the
    // processor makes one right after start), but from here on every pass
    // finds this one, so only an ack can reset it.
    wait_until("the message never reached the handler", || {
        !seen_attempts.lock().unwrap().is_empty()
    })
    .await;
    saturate_attempts(&raw).await;

    let saturated = i16::MAX - 1;
    wait_until(
        "a partition at the attempts limit never reached its handler again",
        || seen_attempts.lock().unwrap().last() == Some(&saturated),
    )
    .await;
    assert_eq!(
        read_processor(&raw).await.attempts,
        i16::MAX,
        "attempts saturates at the limit"
    );

    // The downstream recovers: the message is delivered and the counter resets.
    let_through.store(true, Ordering::Release);
    let state = wait_for_processor(&raw, "the message was never delivered", |s| {
        s.processed_seq == 1
    })
    .await;
    assert_eq!(state.attempts, 0, "delivery resets the counter");
    assert_eq!(
        seen_attempts.lock().unwrap().last(),
        Some(&saturated),
        "the handler sees the saturated count, not a reset one"
    );

    handle.stop().await;
}

/// Transactional mode: a retry at the limit is recorded rather than failing,
/// and the partition delivers once the downstream recovers.
async fn a_retry_at_the_limit_is_recorded_transactional(url: &str) {
    let db = connect_db(url, ConnectOpts::default())
        .await
        .expect("connect");
    run_migrations_for_testing(&db, outbox_migrations())
        .await
        .expect("migrations");
    let raw = sea_orm::Database::connect(url).await.expect("raw connect");

    let let_through = Arc::new(AtomicBool::new(false));
    let handle = Outbox::builder(db.clone())
        .processors(1)
        .maintenance(1, 1)
        .processor_tuning(
            WorkerTuning::processor()
                .retry_base(Duration::from_millis(50))
                .retry_max(Duration::from_millis(200)),
        )
        .queue("q", Partitions::of(1))
        .transactional(GatedTxHandler {
            let_through: Arc::clone(&let_through),
        })
        .start()
        .await
        .expect("start outbox");

    // In transactional mode only a retry or an ack writes the counter, so it
    // can be saturated before anything is enqueued.
    saturate_attempts(&raw).await;
    enqueue_one(handle.outbox(), &db).await;

    // The retry commits its reason. An overflowing increment rolls the whole
    // ack back, so `last_error` would never be written.
    let state = wait_for_processor(
        &raw,
        "a retry at the attempts limit was never recorded",
        |s| s.last_error.as_deref() == Some(DOWNSTREAM_DOWN),
    )
    .await;
    assert_eq!(state.attempts, i16::MAX, "attempts saturates at the limit");
    assert_eq!(state.processed_seq, 0);

    // The downstream recovers: the message is delivered and the counter resets.
    let_through.store(true, Ordering::Release);
    let state = wait_for_processor(&raw, "the message was never delivered", |s| {
        s.processed_seq == 1
    })
    .await;
    assert_eq!(state.attempts, 0, "delivery resets the counter");

    handle.stop().await;
}

#[cfg(feature = "pg")]
#[tokio::test]
async fn postgres_saturated_leased_partition_still_delivers() -> anyhow::Result<()> {
    let db = common::bring_up_postgres().await?;
    a_saturated_leased_partition_still_delivers(&db.url).await;
    Ok(())
}

#[cfg(feature = "mysql")]
#[tokio::test]
async fn mysql_saturated_leased_partition_still_delivers() -> anyhow::Result<()> {
    let db = common::bring_up_mysql().await?;
    a_saturated_leased_partition_still_delivers(&db.url).await;
    Ok(())
}

#[cfg(feature = "pg")]
#[tokio::test]
async fn postgres_transactional_retry_at_the_limit_is_recorded() -> anyhow::Result<()> {
    let db = common::bring_up_postgres().await?;
    a_retry_at_the_limit_is_recorded_transactional(&db.url).await;
    Ok(())
}

#[cfg(feature = "mysql")]
#[tokio::test]
async fn mysql_transactional_retry_at_the_limit_is_recorded() -> anyhow::Result<()> {
    let db = common::bring_up_mysql().await?;
    a_retry_at_the_limit_is_recorded_transactional(&db.url).await;
    Ok(())
}
