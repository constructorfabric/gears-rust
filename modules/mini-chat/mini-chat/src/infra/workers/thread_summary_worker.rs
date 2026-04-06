//! Thread summary outbox handler - processes `thread_summary` queue events.
//!
//! Runs as part of the outbox pipeline (leased strategy). All replicas
//! process events in parallel, partitioned by `chat_id`. No leader election needed.
//!
//! **P1 stub**: logs each message but performs no actual LLM invocation or
//! summary generation. Returns `Retry` so events accumulate safely in the
//! outbox until the handler is fully implemented.

use async_trait::async_trait;
use modkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};
use tracing::warn;

/// Stub handler for thread summary task events.
///
/// Returns `Retry` for every message - events accumulate safely in the outbox
/// until the summary worker ships. This ensures the queue is registered and
/// partitioned from day one.
pub struct ThreadSummaryHandler;

#[async_trait]
impl LeasedMessageHandler for ThreadSummaryHandler {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        warn!(
            partition_id = msg.partition_id,
            seq = msg.seq,
            "thread summary handler not yet implemented - retrying"
        );
        MessageResult::Retry
    }
}
#[cfg(test)]
#[path = "thread_summary_worker_tests.rs"]
mod tests;
