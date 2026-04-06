use std::sync::atomic::{AtomicUsize, Ordering};

use sea_orm::ConnectionTrait;

use super::batch::Batch;

/// A message read from the outbox for handler processing.
///
/// All messages in a single handler invocation belong to exactly one partition.
/// This is a documented invariant - the processor owns one partition and never
/// mixes messages across partitions in a single call.
pub struct OutboxMessage {
    pub partition_id: i64,
    pub seq: i64,
    pub payload: Vec<u8>,
    pub payload_type: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// How many times this message has been retried (0 on first attempt).
    /// The handler uses this to decide when to give up and return Reject.
    pub attempts: i16,
}

/// The result of a handler invocation.
#[derive(Debug, Clone)]
pub enum HandlerResult {
    /// All messages processed successfully. The processor advances the cursor
    /// past the last message. Processed outgoing and body rows are cleaned up
    /// asynchronously by the vacuum.
    Success,
    /// Transient failure. The cursor is not advanced; the same batch will be
    /// retried with exponential backoff. The `attempts` counter is incremented.
    Retry { reason: String },
    /// Permanent failure. All messages in the batch are moved to the dead-letter
    /// table with inline payload copies. The cursor is advanced past the batch.
    Reject { reason: String },
}

/// Result of a per-message leased handler invocation.
#[derive(Debug, Clone)]
pub enum MessageResult {
    /// Message processed successfully.
    Ok,
    /// Transient failure - retry this message and all remaining.
    Retry,
    /// Permanent failure - dead-letter this message, continue with the rest.
    Reject(String),
}

/// Batch handler that runs under a time-limited **lease**.
///
/// **Delivery guarantee:** at-least-once. The processor acquires a DB lease,
/// reads messages, then calls the handler outside any transaction. If the
/// lease expires before ack, another processor may re-deliver the same
/// messages. Handlers must be idempotent.
///
/// The framework enforces lease-aware cancellation by dropping the handler
/// future when the cancel point (`lease_duration - ack_headroom`) is reached.
///
/// Implement this trait directly for batch/chunked processing. For the common
/// per-message case, implement [`LeasedMessageHandler`] instead - a blanket
/// impl will provide `LeasedHandler` automatically.
#[async_trait::async_trait]
pub trait LeasedHandler: Send + Sync {
    async fn handle(&self, batch: &mut Batch<'_>) -> HandlerResult;
}

/// Per-message handler that runs under a time-limited **lease**.
///
/// Convenience trait for the common case of processing one message at a time.
/// A blanket impl bridges this to [`LeasedHandler`] - the framework loops,
/// tracks progress via [`Batch::ack`]/[`Batch::reject`], and handles partial
/// failure automatically. `PerMessageAdapter` is not needed.
///
/// Same delivery guarantees and cancellation semantics as [`LeasedHandler`].
#[async_trait::async_trait]
pub trait LeasedMessageHandler: Send + Sync {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult;
}

/// Blanket impl: every [`LeasedMessageHandler`] is automatically a
/// [`LeasedHandler`]. This replaces `PerMessageAdapter` for the leased path.
///
/// Checks [`Batch::remaining`] before each message. When the remaining budget
/// is zero the loop stops gracefully - the current in-flight call (if any)
/// has already completed, and no new messages are started. The processor's
/// `timeout_at` provides the hard backstop if the handler ignores `remaining()`.
#[async_trait::async_trait]
impl<H: LeasedMessageHandler> LeasedHandler for H {
    async fn handle(&self, batch: &mut Batch<'_>) -> HandlerResult {
        while let Some(msg) = batch.next_msg() {
            match LeasedMessageHandler::handle(self, msg).await {
                MessageResult::Ok => batch.ack(),
                MessageResult::Retry => {
                    return HandlerResult::Retry {
                        reason: "message handler returned Retry".into(),
                    };
                }
                MessageResult::Reject(reason) => batch.reject(reason),
            }
            // Stop starting new messages when the lease budget is exhausted.
            // The message just processed completed naturally (its HTTP call
            // finished within its own timeout). Don't start the next one.
            if batch.remaining().is_zero() {
                break;
            }
        }
        HandlerResult::Success
    }
}

/// Batch handler that runs **inside** the DB transaction holding the partition lock.
///
/// **Delivery guarantee:** exactly-once within the database transaction. The
/// handler can perform DB writes atomically with the ack - both commit or both
/// roll back together.
///
/// **Per-partition invariant:** all messages in a single `handle()` call belong
/// to exactly one partition.
///
/// **Cancellation:** transactional mode uses row-level locks (not time-based
/// leases). The framework drops the future on shutdown; no explicit cancel
/// token is passed.
#[async_trait::async_trait]
pub trait TransactionalHandler: Send + Sync {
    async fn handle(&self, txn: &dyn ConnectionTrait, msgs: &[OutboxMessage]) -> HandlerResult;

    /// Number of messages successfully processed before the batch completed.
    /// Returns `None` for batch handlers (default), `Some(n)` for `PerMessageAdapter`.
    /// The processor uses this for partial-failure semantics.
    fn processed_count(&self) -> Option<usize> {
        None
    }
}

/// Single-message handler (transactional mode).
///
/// Convenience trait for the common case of processing one message at a time.
/// Use via `QueueBuilder::transactional()`. Internally wrapped with [`PerMessageAdapter`].
///
/// Same delivery guarantees and cancellation semantics as [`TransactionalHandler`].
#[async_trait::async_trait]
pub trait TransactionalMessageHandler: Send + Sync {
    async fn handle(&self, txn: &dyn ConnectionTrait, msg: &OutboxMessage) -> HandlerResult;
}

/// Adapter: single-message transactional handler → batch transactional handler.
/// Processes messages one at a time, stops on first non-Success.
///
/// Tracks a `processed_count` - the number of messages successfully handled
/// before the batch completed (or failed). The processor reads this via
/// `TransactionalHandler::processed_count()` to support partial-failure
/// semantics.
pub struct PerMessageAdapter<H> {
    pub handler: H,
    processed: AtomicUsize,
}

impl<H> PerMessageAdapter<H> {
    pub fn new(handler: H) -> Self {
        Self {
            handler,
            processed: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl<H: TransactionalMessageHandler> TransactionalHandler for PerMessageAdapter<H> {
    async fn handle(&self, txn: &dyn ConnectionTrait, msgs: &[OutboxMessage]) -> HandlerResult {
        self.processed.store(0, Ordering::Release);
        for msg in msgs {
            let result = self.handler.handle(txn, msg).await;
            if !matches!(result, HandlerResult::Success) {
                return result;
            }
            self.processed.fetch_add(1, Ordering::Release);
        }
        HandlerResult::Success
    }

    fn processed_count(&self) -> Option<usize> {
        Some(self.processed.load(Ordering::Acquire))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "handler_tests.rs"]
mod tests;
