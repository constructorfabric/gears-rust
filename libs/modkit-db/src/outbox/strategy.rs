use std::collections::HashMap;
use std::time::Duration;

use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use tokio_util::sync::CancellationToken;

use super::dialect::Dialect;
use super::handler::{Handler, HandlerResult, OutboxMessage, TransactionalHandler};
use super::types::OutboxError;
use crate::Db;

/// Context for processing a single partition's batch.
pub struct ProcessContext<'a> {
    pub db: &'a Db,
    pub backend: DbBackend,
    pub dialect: Dialect,
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
        lease_duration: Duration,
        msg_batch_size: u32,
        cancel: CancellationToken,
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
    backend: DbBackend,
    dialect: &Dialect,
    partition_id: i64,
    proc_row: &ProcessorRow,
    msg_batch_size: u32,
) -> Result<Vec<OutboxMessage>, OutboxError> {
    // Use seq > processed_seq (not seq >= processed_seq + 1) — the cursor
    // stores the last processed seq, so `>` is the natural predicate.
    let outgoing_rows = OutgoingRow::find_by_statement(Statement::from_sql_and_values(
        backend,
        dialect.read_outgoing_batch(msg_batch_size),
        [partition_id.into(), proc_row.processed_seq.into()],
    ))
    .all(txn)
    .await?;

    if outgoing_rows.is_empty() {
        return Ok(Vec::new());
    }

    // Batch body read: single SELECT ... WHERE id IN (...) instead of N+1 queries
    let body_ids: Vec<i64> = outgoing_rows.iter().map(|r| r.body_id).collect();
    let body_sql = dialect.build_read_body_batch(body_ids.len());
    let body_values: Vec<sea_orm::Value> = body_ids.iter().map(|&id| id.into()).collect();
    let body_rows = BodyRow::find_by_statement(Statement::from_sql_and_values(
        backend,
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
    backend: DbBackend,
    dialect: &Dialect,
    partition_id: i64,
    msgs: &[OutboxMessage],
    result: &HandlerResult,
) -> Result<(), OutboxError> {
    let last_seq = msgs.last().map_or(0, |m| m.seq);

    match result {
        HandlerResult::Success => {
            txn.execute(Statement::from_sql_and_values(
                backend,
                dialect.advance_processed_seq(),
                [last_seq.into(), partition_id.into()],
            ))
            .await?;
            txn.execute(Statement::from_sql_and_values(
                backend,
                dialect.bump_vacuum_counter(),
                [partition_id.into()],
            ))
            .await?;
        }
        HandlerResult::Retry { reason } => {
            txn.execute(Statement::from_sql_and_values(
                backend,
                dialect.record_retry(),
                [reason.as_str().into(), partition_id.into()],
            ))
            .await?;
        }
        HandlerResult::Reject { reason } => {
            for msg in msgs {
                txn.execute(Statement::from_sql_and_values(
                    backend,
                    dialect.insert_dead_letter(),
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
                backend,
                dialect.advance_processed_seq(),
                [last_seq.into(), partition_id.into()],
            ))
            .await?;
            txn.execute(Statement::from_sql_and_values(
                backend,
                dialect.bump_vacuum_counter(),
                [partition_id.into()],
            ))
            .await?;
        }
    }

    Ok(())
}

async fn try_lock_and_read_state(
    txn: &impl ConnectionTrait,
    backend: DbBackend,
    dialect: &Dialect,
    partition_id: i64,
) -> Result<Option<ProcessorRow>, OutboxError> {
    if let Some(lock_sql) = dialect.lock_processor() {
        let row = txn
            .query_one(Statement::from_sql_and_values(
                backend,
                lock_sql,
                [partition_id.into()],
            ))
            .await?;
        if row.is_none() {
            return Ok(None);
        }
    }

    let proc_row = ProcessorRow::find_by_statement(Statement::from_sql_and_values(
        backend,
        dialect.read_processor(),
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
        _lease_duration: Duration,
        msg_batch_size: u32,
        cancel: CancellationToken,
    ) -> Result<Option<ProcessResult>, OutboxError> {
        let conn = ctx.db.sea_internal();
        let txn = conn.begin().await?;

        let Some(proc_row) =
            try_lock_and_read_state(&txn, ctx.backend, &ctx.dialect, ctx.partition_id).await?
        else {
            txn.commit().await?;
            return Ok(None);
        };

        let msgs = read_messages(
            &txn,
            ctx.backend,
            &ctx.dialect,
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

        let result = self.handler.handle(&txn, &msgs, cancel).await;
        #[allow(clippy::cast_possible_truncation)]
        let pc = self.handler.processed_count().map(|n| n as u32);

        // Transactional partial-failure semantics: on Reject/Retry the entire
        // transaction (including any handler side-effects) is committed with
        // the ack. Dead letters are created for all messages in the batch on
        // Reject — even those the handler processed successfully — because
        // the handler's successful work is atomic with the cursor advance.
        // The `processed_count` is still recorded in ProcessResult so the
        // PartitionMode state machine can degrade batch size intelligently.
        ack(
            &txn,
            ctx.backend,
            &ctx.dialect,
            ctx.partition_id,
            &msgs,
            &result,
        )
        .await?;

        txn.commit().await?;

        Ok(Some(ProcessResult {
            count,
            handler_result: result,

            processed_count: pc,
        }))
    }
}

// ---- Decoupled strategy ----

/// Processes messages outside any DB transaction.
/// Uses lease-based 3-phase: acquire lease+read → handle → lease-guarded ack.
pub struct DecoupledStrategy {
    handler: Box<dyn Handler>,
    worker_id: String,
}

impl DecoupledStrategy {
    pub fn new(handler: Box<dyn Handler>, worker_id: String) -> Self {
        Self { handler, worker_id }
    }
}

impl ProcessingStrategy for DecoupledStrategy {
    async fn process(
        &self,
        ctx: &ProcessContext<'_>,
        lease_duration: Duration,
        msg_batch_size: u32,
        cancel: CancellationToken,
    ) -> Result<Option<ProcessResult>, OutboxError> {
        let lease_id = &self.worker_id;
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let lease_secs = lease_duration.as_secs() as i64;

        // Phase 1: Acquire lease + read messages
        let (_proc_row, msgs) = {
            let sea_conn = ctx.db.sea_internal();
            let txn = sea_conn.begin().await?;

            // Acquire lease via dialect helper (encapsulates RETURNING vs SELECT fallback)
            let proc_row = ctx
                .dialect
                .exec_lease_acquire(&txn, ctx.backend, lease_id, lease_secs, ctx.partition_id)
                .await?
                .map(|(processed_seq, attempts)| ProcessorRow {
                    processed_seq,
                    attempts,
                });

            let Some(mut proc_row) = proc_row else {
                txn.commit().await?;
                return Ok(None);
            };

            // lease_acquire increments attempts in the DB so a crash leaves
            // a trace. Subtract 1 so the handler sees the pre-increment
            // value (0 = first attempt, 1 = one previous attempt, etc.).
            proc_row.attempts = proc_row.attempts.saturating_sub(1);

            let msgs = read_messages(
                &txn,
                ctx.backend,
                &ctx.dialect,
                ctx.partition_id,
                &proc_row,
                msg_batch_size,
            )
            .await?;

            txn.commit().await?;

            if msgs.is_empty() {
                // Release lease — nothing to process
                let conn = ctx.db.sea_internal();
                conn.execute(Statement::from_sql_and_values(
                    ctx.backend,
                    ctx.dialect.lease_release(),
                    [ctx.partition_id.into(), lease_id.as_str().into()],
                ))
                .await?;
                return Ok(None);
            }

            (proc_row, msgs)
        };

        #[allow(clippy::cast_possible_truncation)]
        let count = msgs.len() as u32;

        // Phase 2: call handler outside any transaction
        // Create a child token that fires at 80% of lease duration
        let lease_cancel = cancel.child_token();
        let lease_timer = {
            let token = lease_cancel.clone();
            let deadline = lease_duration.mul_f64(0.8);
            tokio::spawn(async move {
                tokio::time::sleep(deadline).await;
                token.cancel();
            })
        };

        let result = self.handler.handle(&msgs, lease_cancel).await;
        #[allow(clippy::cast_possible_truncation)]
        let pc = self.handler.processed_count().map(|n| n as u32);
        lease_timer.abort();

        // Phase 3: lease-guarded ack
        let ack_conn = ctx.db.sea_internal();
        let ack_txn = ack_conn.begin().await?;

        let last_seq = msgs.last().map_or(0, |m| m.seq);

        match &result {
            HandlerResult::Success => {
                let res = ack_txn
                    .execute(Statement::from_sql_and_values(
                        ctx.backend,
                        ctx.dialect.lease_ack_advance(),
                        [
                            last_seq.into(),
                            ctx.partition_id.into(),
                            lease_id.as_str().into(),
                        ],
                    ))
                    .await?;
                if res.rows_affected() == 0 {
                    tracing::error!(
                        partition_id = ctx.partition_id,
                        "lease expired before ack \u{2014} another processor may have taken over"
                    );
                    ack_txn.commit().await?;
                    return Ok(None);
                }
                ack_txn
                    .execute(Statement::from_sql_and_values(
                        ctx.backend,
                        ctx.dialect.bump_vacuum_counter(),
                        [ctx.partition_id.into()],
                    ))
                    .await?;
            }
            HandlerResult::Retry { reason } => {
                // Retry: cursor not advanced — the same batch will be re-read.
                // processed_count is carried in ProcessResult for PartitionMode
                // degradation (batch size reduction), but no partial ack occurs.
                let res = ack_txn
                    .execute(Statement::from_sql_and_values(
                        ctx.backend,
                        ctx.dialect.lease_record_retry(),
                        [
                            reason.as_str().into(),
                            ctx.partition_id.into(),
                            lease_id.as_str().into(),
                        ],
                    ))
                    .await?;
                if res.rows_affected() == 0 {
                    tracing::error!(
                        partition_id = ctx.partition_id,
                        "lease expired before retry ack"
                    );
                    ack_txn.commit().await?;
                    return Ok(None);
                }
            }
            HandlerResult::Reject { reason } => {
                // Partial-failure semantics: when PerMessageAdapter reports
                // processed_count, only dead-letter the unprocessed tail
                // (msgs[pc..]). The successfully-processed prefix had its
                // side-effects committed outside the DB transaction, so we
                // don't dead-letter those. For batch handlers (pc = None),
                // dead-letter the entire batch (existing behavior).
                let skip = pc.map_or(0, |n| n as usize).min(msgs.len());
                for msg in &msgs[skip..] {
                    ack_txn
                        .execute(Statement::from_sql_and_values(
                            ctx.backend,
                            ctx.dialect.insert_dead_letter(),
                            [
                                ctx.partition_id.into(),
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

                let res = ack_txn
                    .execute(Statement::from_sql_and_values(
                        ctx.backend,
                        ctx.dialect.lease_ack_advance(),
                        [
                            last_seq.into(),
                            ctx.partition_id.into(),
                            lease_id.as_str().into(),
                        ],
                    ))
                    .await?;
                if res.rows_affected() == 0 {
                    tracing::error!(
                        partition_id = ctx.partition_id,
                        "lease expired before reject ack"
                    );
                    ack_txn.commit().await?;
                    return Ok(None);
                }
                ack_txn
                    .execute(Statement::from_sql_and_values(
                        ctx.backend,
                        ctx.dialect.bump_vacuum_counter(),
                        [ctx.partition_id.into()],
                    ))
                    .await?;
            }
        }

        ack_txn.commit().await?;

        Ok(Some(ProcessResult {
            count,
            handler_result: result,

            processed_count: pc,
        }))
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
    // Simple PRNG seeded from nanosecond clock — sufficient for worker ID uniqueness
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
