use std::collections::HashMap;

use super::batch::Batch;
use super::handler::{HandlerResult, LeasedHandler, OutboxMessage, TransactionalHandler};
use super::store::OutboxStore;
use super::types::{LeaseConfig, OutboxError};
use crate::Db;
use sea_orm::{ConnectionTrait, FromQueryResult, Statement, TransactionTrait};

/// Context for processing a single partition's batch.
pub struct ProcessContext<'a> {
    pub db: &'a Db,
    pub store: OutboxStore<'a>,
    pub partition_id: i64,
}

/// Sealed trait for compile-time processing mode dispatch.
///
/// Each implementation manages its own transaction scope. The processor
/// delegates the entire read→handle→ack cycle to the strategy.
pub trait ProcessingStrategy: Send + Sync {
    /// Process one batch for the given partition.
    ///
    /// `msg_batch_size` controls how many messages to fetch per cycle
    /// (from `WorkerTuning::batch_size`, possibly degraded by `PartitionMode`).
    ///
    /// Returns `Ok(Some(result))` if work was done, `Ok(None)` if the
    /// partition was empty or locked by another processor.
    fn process(
        &self,
        ctx: &ProcessContext<'_>,
        msg_batch_size: u32,
    ) -> impl std::future::Future<Output = Result<Option<ProcessResult>, OutboxError>> + Send;
}

/// Result of processing a batch.
pub struct ProcessResult {
    pub count: u32,
    pub handler_result: HandlerResult,
    /// Number of messages the handler successfully processed before the batch
    /// completed (or failed). `Some` for `PerMessageAdapter`-wrapped handlers,
    /// `None` for raw batch handlers. Used for partial-failure semantics.
    pub processed_count: Option<u32>,
}

// ---- SQL row types ----

#[derive(Debug, FromQueryResult)]
struct ProcessorRow {
    processed_seq: i64,
    attempts: i16,
}

#[derive(Debug, FromQueryResult)]
struct OutgoingRow {
    id: i64,
    body_id: i64,
    seq: i64,
}

#[derive(Debug, FromQueryResult)]
struct BodyRow {
    id: i64,
    payload: Vec<u8>,
    payload_type: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

// ---- Shared helpers ----

async fn read_messages(
    txn: &impl ConnectionTrait,
    store: &OutboxStore<'_>,
    partition_id: i64,
    proc_row: &ProcessorRow,
    msg_batch_size: u32,
) -> Result<Vec<OutboxMessage>, OutboxError> {
    // Use seq > processed_seq (not seq >= processed_seq + 1) - the cursor
    // stores the last processed seq, so `>` is the natural predicate.
    let outgoing_rows = OutgoingRow::find_by_statement(Statement::from_sql_and_values(
        store.backend(),
        store.read_outgoing_batch(msg_batch_size),
        [partition_id.into(), proc_row.processed_seq.into()],
    ))
    .all(txn)
    .await?;

    if outgoing_rows.is_empty() {
        return Ok(Vec::new());
    }

    // Batch body read: single SELECT ... WHERE id IN (...) instead of N+1 queries
    let body_ids: Vec<i64> = outgoing_rows.iter().map(|r| r.body_id).collect();
    let body_sql = store.build_read_body_batch(body_ids.len());
    let body_values: Vec<sea_orm::Value> = body_ids.iter().map(|&id| id.into()).collect();
    let body_rows = BodyRow::find_by_statement(Statement::from_sql_and_values(
        store.backend(),
        &body_sql,
        body_values,
    ))
    .all(txn)
    .await?;

    let body_map: HashMap<i64, BodyRow> = body_rows.into_iter().map(|b| (b.id, b)).collect();

    let mut msgs = Vec::with_capacity(outgoing_rows.len());
    for row in &outgoing_rows {
        let body = body_map.get(&row.body_id).ok_or_else(|| {
            OutboxError::Database(sea_orm::DbErr::Custom(format!(
                "body row {} not found for outgoing {}",
                row.body_id, row.id
            )))
        })?;

        msgs.push(OutboxMessage {
            partition_id,
            seq: row.seq,
            payload: body.payload.clone(),
            payload_type: body.payload_type.clone(),
            created_at: body.created_at,
            attempts: proc_row.attempts,
        });
    }

    Ok(msgs)
}

/// Append-only ack: only UPDATE `processed_seq`, no DELETEs.
/// Vacuum handles cleanup of processed outgoing + body rows.
async fn ack(
    txn: &impl ConnectionTrait,
    store: &OutboxStore<'_>,
    partition_id: i64,
    msgs: &[OutboxMessage],
    result: &HandlerResult,
) -> Result<(), OutboxError> {
    let last_seq = msgs.last().map_or(0, |m| m.seq);

    match result {
        HandlerResult::Success => {
            txn.execute(Statement::from_sql_and_values(
                store.backend(),
                store.advance_processed_seq(),
                [last_seq.into(), partition_id.into()],
            ))
            .await?;
            txn.execute(Statement::from_sql_and_values(
                store.backend(),
                store.bump_vacuum_counter(),
                [partition_id.into()],
            ))
            .await?;
        }
        HandlerResult::Retry { reason } => {
            txn.execute(Statement::from_sql_and_values(
                store.backend(),
                store.record_retry(),
                [reason.as_str().into(), partition_id.into()],
            ))
            .await?;
        }
        HandlerResult::Reject { reason } => {
            for msg in msgs {
                txn.execute(Statement::from_sql_and_values(
                    store.backend(),
                    store.insert_dead_letter(),
                    [
                        partition_id.into(),
                        msg.seq.into(),
                        msg.payload.clone().into(),
                        msg.payload_type.clone().into(),
                        msg.created_at.into(),
                        reason.as_str().into(),
                        msg.attempts.into(),
                    ],
                ))
                .await?;
            }

            txn.execute(Statement::from_sql_and_values(
                store.backend(),
                store.advance_processed_seq(),
                [last_seq.into(), partition_id.into()],
            ))
            .await?;
            txn.execute(Statement::from_sql_and_values(
                store.backend(),
                store.bump_vacuum_counter(),
                [partition_id.into()],
            ))
            .await?;
        }
    }

    Ok(())
}

async fn try_lock_and_read_state(
    txn: &impl ConnectionTrait,
    store: &OutboxStore<'_>,
    partition_id: i64,
) -> Result<Option<ProcessorRow>, OutboxError> {
    if let Some(lock_sql) = store.lock_processor() {
        let row = txn
            .query_one(Statement::from_sql_and_values(
                store.backend(),
                lock_sql,
                [partition_id.into()],
            ))
            .await?;
        if row.is_none() {
            return Ok(None);
        }
    }

    let proc_row = ProcessorRow::find_by_statement(Statement::from_sql_and_values(
        store.backend(),
        store.read_processor(),
        [partition_id.into()],
    ))
    .one(txn)
    .await?;

    Ok(proc_row)
}

// ---- Transactional strategy ----

/// Processes messages inside the DB transaction holding the partition lock.
/// Handler can perform atomic DB writes alongside the ack.
pub struct TransactionalStrategy {
    handler: Box<dyn TransactionalHandler>,
}

impl TransactionalStrategy {
    pub fn new(handler: Box<dyn TransactionalHandler>) -> Self {
        Self { handler }
    }
}

impl ProcessingStrategy for TransactionalStrategy {
    async fn process(
        &self,
        ctx: &ProcessContext<'_>,
        msg_batch_size: u32,
    ) -> Result<Option<ProcessResult>, OutboxError> {
        let conn = ctx.db.sea_internal();
        let txn = conn.begin().await?;

        let Some(proc_row) = try_lock_and_read_state(&txn, &ctx.store, ctx.partition_id).await?
        else {
            txn.commit().await?;
            return Ok(None);
        };

        let msgs = read_messages(
            &txn,
            &ctx.store,
            ctx.partition_id,
            &proc_row,
            msg_batch_size,
        )
        .await?;
        if msgs.is_empty() {
            txn.commit().await?;
            return Ok(None);
        }

        #[allow(clippy::cast_possible_truncation)]
        let count = msgs.len() as u32;

        let result = self.handler.handle(&txn, &msgs).await;
        #[allow(clippy::cast_possible_truncation)]
        let pc = self.handler.processed_count().map(|n| n as u32);

        // Transactional partial-failure semantics: on Reject/Retry the entire
        // transaction (including any handler side-effects) is committed with
        // the ack. Dead letters are created for all messages in the batch on
        // Reject - even those the handler processed successfully - because
        // the handler's successful work is atomic with the cursor advance.
        // The `processed_count` is still recorded in ProcessResult so the
        // PartitionMode state machine can degrade batch size intelligently.
        ack(&txn, &ctx.store, ctx.partition_id, &msgs, &result).await?;

        txn.commit().await?;

        Ok(Some(ProcessResult {
            count,
            handler_result: result,

            processed_count: pc,
        }))
    }
}

// ---- Shared lease helpers ----

/// Phase 1: Acquire a time-based lease and read messages.
/// Returns `None` if another processor holds the lease or no messages are available.
async fn acquire_lease_and_read(
    ctx: &ProcessContext<'_>,
    lease_id: &str,
    lease_secs: i64,
    msg_batch_size: u32,
) -> Result<Option<Vec<OutboxMessage>>, OutboxError> {
    let sea_conn = ctx.db.sea_internal();
    let txn = sea_conn.begin().await?;

    let proc_row = ctx
        .store
        .exec_lease_acquire(&txn, lease_id, lease_secs, ctx.partition_id)
        .await?
        .map(|(processed_seq, attempts)| ProcessorRow {
            processed_seq,
            // lease_acquire increments attempts in the DB so a crash leaves
            // a trace. Subtract 1 so the handler sees the pre-increment
            // value (0 = first attempt, 1 = one previous attempt, etc.).
            attempts: attempts.saturating_sub(1),
        });

    let Some(proc_row) = proc_row else {
        txn.commit().await?;
        return Ok(None);
    };

    let msgs = read_messages(
        &txn,
        &ctx.store,
        ctx.partition_id,
        &proc_row,
        msg_batch_size,
    )
    .await?;

    txn.commit().await?;

    if msgs.is_empty() {
        // Release the lease AND reset `attempts` back to 0.
        // The increment from `lease_acquire` is a crash-detection trace only:
        // if the process crashes between acquire and ack, the next processor
        // sees a non-zero attempt count. On idle polls (no messages),
        // `lease_release` resets attempts so they do not accumulate across
        // empty cycles.
        let conn = ctx.db.sea_internal();
        conn.execute(Statement::from_sql_and_values(
            ctx.store.backend(),
            ctx.store.lease_release(),
            [ctx.partition_id.into(), lease_id.into()],
        ))
        .await?;
        return Ok(None);
    }

    Ok(Some(msgs))
}

// ---- Lease-guarded ack helpers ----

/// Persist per-message rejections from the blanket impl as dead-letter rows.
async fn persist_rejections(
    txn: &impl ConnectionTrait,
    ctx: &ProcessContext<'_>,
    msgs: &[OutboxMessage],
    rejections: &[super::batch::Rejection],
) -> Result<(), OutboxError> {
    for rej in rejections {
        let msg = &msgs[rej.index];
        insert_dead_letter(txn, ctx, msg, &rej.reason).await?;
    }
    Ok(())
}

/// Advance the cursor and bump the vacuum counter. Returns `false` if the
/// lease expired (caller must rollback). Releases the lease if `seq == 0`.
async fn advance_cursor(
    txn: &impl ConnectionTrait,
    ctx: &ProcessContext<'_>,
    seq: i64,
    lease_id: &str,
) -> Result<bool, OutboxError> {
    if seq == 0 {
        txn.execute(Statement::from_sql_and_values(
            ctx.store.backend(),
            ctx.store.lease_release(),
            [ctx.partition_id.into(), lease_id.into()],
        ))
        .await?;
        return Ok(true);
    }

    let res = txn
        .execute(Statement::from_sql_and_values(
            ctx.store.backend(),
            ctx.store.lease_ack_advance(),
            [seq.into(), ctx.partition_id.into(), lease_id.into()],
        ))
        .await?;
    if res.rows_affected() == 0 {
        return Ok(false);
    }

    txn.execute(Statement::from_sql_and_values(
        ctx.store.backend(),
        ctx.store.bump_vacuum_counter(),
        [ctx.partition_id.into()],
    ))
    .await?;
    Ok(true)
}

/// Record a retry without advancing the cursor. Returns `false` if the
/// lease expired.
async fn record_retry(
    txn: &impl ConnectionTrait,
    ctx: &ProcessContext<'_>,
    reason: &str,
    lease_id: &str,
) -> Result<bool, OutboxError> {
    let res = txn
        .execute(Statement::from_sql_and_values(
            ctx.store.backend(),
            ctx.store.lease_record_retry(),
            [reason.into(), ctx.partition_id.into(), lease_id.into()],
        ))
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Sequence number of the last processed message, or 0 if nothing processed.
fn processed_advance_seq(msgs: &[OutboxMessage], processed: u32) -> i64 {
    if processed > 0 && (processed as usize) <= msgs.len() {
        msgs[processed as usize - 1].seq
    } else {
        0
    }
}

/// Phase 3: Lease-guarded ack. Used by `LeasedStrategy`.
async fn lease_guarded_ack(
    ctx: &ProcessContext<'_>,
    msgs: &[OutboxMessage],
    lease_id: &str,
    result: HandlerResult,
    processed: u32,
    rejections: &[super::batch::Rejection],
) -> Result<Option<ProcessResult>, OutboxError> {
    let ack_conn = ctx.db.sea_internal();
    let ack_txn = ack_conn.begin().await?;
    let count = u32::try_from(msgs.len()).unwrap_or(u32::MAX);

    // All three branches persist rejections first, then differ in cursor behavior.
    persist_rejections(&ack_txn, ctx, msgs, rejections).await?;

    let lease_ok = match &result {
        HandlerResult::Success => {
            advance_cursor(
                &ack_txn,
                ctx,
                processed_advance_seq(msgs, processed),
                lease_id,
            )
            .await?
        }
        HandlerResult::Retry { reason } => {
            let advance_seq = processed_advance_seq(msgs, processed);
            if advance_seq > 0 {
                // Partial progress: advance past processed prefix, retry the tail.
                advance_cursor(&ack_txn, ctx, advance_seq, lease_id).await?
            } else {
                // Nothing processed: record retry, no cursor advance.
                record_retry(&ack_txn, ctx, reason, lease_id).await?
            }
        }
        HandlerResult::Reject { reason } => {
            // Dead-letter the unprocessed tail (rejections already persisted above).
            let skip = (processed as usize).min(msgs.len());
            for msg in &msgs[skip..] {
                insert_dead_letter(&ack_txn, ctx, msg, reason).await?;
            }
            // Advance past the entire batch (all messages handled or dead-lettered).
            let last_seq = msgs.last().map_or(0, |m| m.seq);
            advance_cursor(&ack_txn, ctx, last_seq, lease_id).await?
        }
    };

    if !lease_ok {
        tracing::error!(
            partition_id = ctx.partition_id,
            "lease expired before ack, another processor may have taken over"
        );
        ack_txn.rollback().await?;
        return Ok(None);
    }

    ack_txn.commit().await?;

    Ok(Some(ProcessResult {
        count,
        handler_result: result,
        processed_count: Some(processed),
    }))
}

/// Insert a single dead-letter row.
async fn insert_dead_letter(
    txn: &impl ConnectionTrait,
    ctx: &ProcessContext<'_>,
    msg: &OutboxMessage,
    reason: &str,
) -> Result<(), OutboxError> {
    txn.execute(Statement::from_sql_and_values(
        ctx.store.backend(),
        ctx.store.insert_dead_letter(),
        [
            ctx.partition_id.into(),
            msg.seq.into(),
            msg.payload.clone().into(),
            msg.payload_type.clone().into(),
            msg.created_at.into(),
            reason.into(),
            msg.attempts.into(),
        ],
    ))
    .await?;
    Ok(())
}

// ---- Leased strategy ----

use std::sync::Arc;

/// Processes messages under a time-limited lease using `LeasedHandler`.
///
/// Three-phase pipeline: acquire lease + read, call handler with `timeout_at`,
/// lease-guarded ack. Cancellation is graceful: `batch.remaining()` signals
/// the handler to stop between messages; `timeout_at` is the hard backstop.
pub struct LeasedStrategy {
    handler: Arc<dyn LeasedHandler>,
    worker_id: String,
    lease_config: LeaseConfig,
}

impl LeasedStrategy {
    pub fn new(
        handler: Arc<dyn LeasedHandler>,
        worker_id: String,
        lease_config: LeaseConfig,
    ) -> Self {
        Self {
            handler,
            worker_id,
            lease_config,
        }
    }
}

impl ProcessingStrategy for LeasedStrategy {
    async fn process(
        &self,
        ctx: &ProcessContext<'_>,
        msg_batch_size: u32,
    ) -> Result<Option<ProcessResult>, OutboxError> {
        let lease_secs = i64::try_from(self.lease_config.duration.as_secs()).unwrap_or(i64::MAX);

        // Capture the clock before Phase 1 so that acquire+read time is
        // deducted from the handler budget. The DB lease starts at SQL
        // NOW(), so our Rust deadline must track the same origin.
        let lease_start = tokio::time::Instant::now();

        let Some(msgs) =
            acquire_lease_and_read(ctx, &self.worker_id, lease_secs, msg_batch_size).await?
        else {
            return Ok(None);
        };

        // Phase 2: call handler with graceful two-phase cancellation.
        //
        // Soft signal: batch.remaining() returns Duration::ZERO after the
        // deadline. The blanket impl checks this between messages and stops
        // starting new work.
        //
        // Hard drop: timeout_at drops the handler future if it didn't return
        // before the deadline (catches handlers that ignore remaining()).
        let deadline = lease_start + self.lease_config.handler_budget();
        let mut batch = Batch::new(&msgs, deadline);

        let result = tokio::time::timeout_at(deadline, self.handler.handle(&mut batch))
            .await
            .unwrap_or_else(|_| HandlerResult::Retry {
                reason: "lease expired".into(),
            });

        // Phase 3: lease-guarded ack
        lease_guarded_ack(
            ctx,
            &msgs,
            &self.worker_id,
            result,
            batch.processed(),
            batch.rejections(),
        )
        .await
    }
}

/// Generate a worker ID in the format `"{name}-{XXXXXX}"` where XXXXXX
/// is 6 random alphanumeric characters (A-Z, 0-9).
pub fn generate_worker_id(queue_name: &str) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    // Simple PRNG seeded from nanosecond clock - sufficient for worker ID uniqueness
    let mut seed = u64::from(nanos) ^ u64::from(std::process::id());
    let mut suffix = String::with_capacity(6);
    for _ in 0..6 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let idx = ((seed >> 33) as usize) % CHARSET.len();
        suffix.push(CHARSET[idx] as char);
    }
    format!("{queue_name}-{suffix}")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn worker_id_format() {
        let id = generate_worker_id("orders");
        assert!(id.starts_with("orders-"), "expected orders- prefix: {id}");
        let suffix = &id["orders-".len()..];
        assert_eq!(suffix.len(), 6, "suffix should be 6 chars: {suffix}");
        assert!(
            suffix
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
            "suffix should be A-Z0-9: {suffix}"
        );
    }

    #[test]
    fn worker_ids_differ() {
        let id1 = generate_worker_id("q");
        std::thread::sleep(std::time::Duration::from_millis(1));
        let id2 = generate_worker_id("q");
        assert_ne!(id1, id2, "worker IDs should differ: {id1} vs {id2}");
    }
}
