//! The outbox work queues (DESIGN §3.7; feature 01 §5.1): `rating.child_work`, `rating.rollup` and
//! one `rating.delivery.e{feed_epoch}` queue per feed epoch, in the `toolkit_db::outbox` tables
//! created with the consumer-owned prefix `bss_rating__outbox` (database ADR-0001).
//!
//! A work item carries `tenant_id` and the object id only; its worker re-reads every decision from
//! the rows. Each queue's worker is supplied by the feature that owns it (the rater, roll-up and
//! the delivery publisher); until it is built the queue is declared with a handler that leaves
//! every message in place, so nothing enqueued early is lost or dead-lettered.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_db::Db;
use toolkit_db::outbox::{
    LeasedMessageHandler, MessageResult, Outbox, OutboxError, OutboxHandle, OutboxMessage,
    Partitions,
};

/// The outbox table prefix: tables `bss_rating__outbox_*` in the gear's namespace.
pub const OUTBOX_TABLE_PREFIX: &str = "bss_rating__outbox";

/// The rater's queue (partition `hash(subscription_id)`).
pub const CHILD_WORK_QUEUE: &str = "rating.child_work";

/// The parent roll-up queue (partition `hash(fact_id)`).
pub const ROLLUP_QUEUE: &str = "rating.rollup";

/// The delivery queue of feed epoch `epoch` (partition `hash(tenant_id) mod n`).
#[must_use]
pub fn delivery_queue(epoch: u32) -> String {
    format!("rating.delivery.e{epoch}")
}

/// The queues of one gear instance and their partition counts. Built only through
/// [`QueueLayout::new`], so no queue is declared twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueLayout {
    child_work: Partitions,
    rollup: Partitions,
    delivery: Partitions,
    feed_epochs: Vec<u32>,
}

/// Why a queue layout is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    /// The feed epochs are empty, zero or repeated, so a delivery queue would be missing or
    /// declared twice.
    #[error("the feed epochs must be distinct epochs of at least 1")]
    FeedEpochs,
}

impl QueueLayout {
    /// The partitions of `rating.child_work`, `rating.rollup` and each delivery queue, and the
    /// feed epochs whose delivery queues are served.
    ///
    /// # Errors
    ///
    /// [`LayoutError::FeedEpochs`] for empty, zero or repeated epochs.
    pub fn new(
        child_work: Partitions,
        rollup: Partitions,
        delivery: Partitions,
        feed_epochs: Vec<u32>,
    ) -> Result<Self, LayoutError> {
        let mut distinct = feed_epochs.clone();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.is_empty() || distinct.len() != feed_epochs.len() || distinct.contains(&0) {
            return Err(LayoutError::FeedEpochs);
        }
        Ok(Self {
            child_work,
            rollup,
            delivery,
            feed_epochs,
        })
    }

    /// The feed epochs whose delivery queues are served.
    #[must_use]
    pub fn feed_epochs(&self) -> &[u32] {
        &self.feed_epochs
    }

    /// Every queue name with its partitions.
    #[must_use]
    pub fn queues(&self) -> Vec<(String, Partitions)> {
        let mut queues = vec![
            (CHILD_WORK_QUEUE.to_owned(), self.child_work),
            (ROLLUP_QUEUE.to_owned(), self.rollup),
        ];
        queues.extend(
            self.feed_epochs
                .iter()
                .map(|epoch| (delivery_queue(*epoch), self.delivery)),
        );
        queues
    }
}

/// The worker of one queue.
pub type QueueWorker = Arc<dyn LeasedMessageHandler>;

/// Starts the outbox with every queue of `layout`, each served by `worker_for(queue)`.
///
/// # Errors
///
/// An invalid prefix or queue name, or a failure to start the pipeline.
pub async fn start(
    db: Db,
    layout: &QueueLayout,
    worker_for: impl Fn(&str) -> QueueWorker,
) -> Result<OutboxHandle, OutboxError> {
    let mut builder = Outbox::builder(db).table_prefix(OUTBOX_TABLE_PREFIX)?;
    for (queue, partitions) in layout.queues() {
        builder = builder
            .queue(&queue, partitions)
            .leased(Forward(worker_for(&queue)))
            .done();
    }
    builder.start().await
}

/// Adapts a shared worker to the builder, which takes handlers by value.
struct Forward(QueueWorker);

#[async_trait]
impl LeasedMessageHandler for Forward {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        self.0.handle(msg).await
    }
}

/// The worker of a queue whose owning feature is not built yet: it leaves the message in place
/// for the real worker.
#[derive(Debug, Clone)]
pub struct UnwiredQueue {
    queue: String,
}

impl UnwiredQueue {
    /// The placeholder of `queue`.
    #[must_use]
    pub fn new(queue: impl Into<String>) -> Self {
        Self {
            queue: queue.into(),
        }
    }
}

#[async_trait]
impl LeasedMessageHandler for UnwiredQueue {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        tracing::warn!(
            target: "bss_rating.queues",
            queue = %self.queue,
            partition = msg.partition_id,
            seq = msg.seq,
            "a work item reached a queue whose worker is not built; it stays queued"
        );
        MessageResult::Retry
    }
}

#[cfg(test)]
#[path = "queues_tests.rs"]
mod tests;
