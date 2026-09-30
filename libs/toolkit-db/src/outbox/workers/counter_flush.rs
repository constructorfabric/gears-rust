//! Publishes what this instance has done to each bounded queue, then re-reads.
//!
//! Nothing on a hot path writes a counter. The enqueue, the sequencer and the
//! ack report into an in-memory journal; this worker is the only thing that
//! turns those reports into rows, and the only thing that can see what *other*
//! instances have done. Its interval is therefore the window in which a
//! queue's reading is behind the truth, and so the bound on how far the queue
//! can overshoot.
//!
//! Publish first, re-read second. Reading before the write commits would take
//! a picture that excludes this instance's own delta, and adding the journal on
//! top of that picture would count the same work twice.
//!
//! A failed publish advances nothing, so the next pass recomputes the identical
//! delta from the monotonic journal. Nothing is lost and nothing is doubled.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::super::core::Outbox;
use super::super::taskward::{Directive, WorkerAction};
use super::super::types::OutboxError;
use crate::Db;

/// Publishes and refreshes the channel counters of every queue that has a
/// bound.
pub struct CounterFlush {
    pub outbox: Arc<Outbox>,
    pub db: Db,
}

impl CounterFlush {
    /// Returns the last failure, if any. A failure on one queue does not
    /// abandon the others - but it must still reach the worker loop, or the
    /// loop treats the pass as a success and clears the backoff it should be
    /// escalating.
    async fn run_once(&self, cancel: &CancellationToken) -> Option<OutboxError> {
        let conn = self.db.sea_internal();
        let runner = crate::secure::SeaOrmRunner::Conn(&conn);

        let mut failure = None;
        for (queue, admission) in self.outbox.bounded_queues() {
            if cancel.is_cancelled() {
                return failure;
            }

            let Some(ids) = self.outbox.queue_partition_ids(&queue) else {
                continue;
            };

            // Publish this instance's unpublished work, one row per partition
            // that has any. A partition nobody has touched costs nothing.
            let mut published_any = false;
            for (partition, delta) in admission.unpublished() {
                let Some(&partition_id) = ids.get(partition) else {
                    continue;
                };
                match self
                    .outbox
                    .apply_channel_deltas(&self.db, partition_id, delta)
                    .await
                {
                    Ok(()) => {
                        admission.published(partition, delta);
                        published_any = true;
                    }
                    Err(e) => {
                        warn!(
                            queue = %queue,
                            partition_id,
                            error = %e,
                            "counter flush: failed to publish a partition's counters, retrying next pass",
                        );
                        failure = Some(e);
                    }
                }
            }

            // A queue whose reading is still fresh and which had nothing to
            // publish needs no read: a longer configured interval is not
            // shortened by another queue's.
            if !published_any && !admission.is_stale() {
                continue;
            }

            match self.outbox.read_queue_channels(&runner, &queue).await {
                Ok(rows) => {
                    for (partition_id, channels) in rows {
                        // A partition's index is its position in the queue's
                        // id list.
                        if let Some(partition) = ids.iter().position(|&id| id == partition_id) {
                            admission.observe(partition, channels);
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        queue = %queue,
                        error = %e,
                        "counter flush: failed to read queue counters, admission continues on the previous snapshot",
                    );
                    failure = Some(e);
                }
            }
        }
        failure
    }
}

impl WorkerAction for CounterFlush {
    type Payload = ();
    type Error = OutboxError;

    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, Self::Error> {
        match self.run_once(cancel).await {
            Some(e) => Err(e),
            None => Ok(Directive::idle()),
        }
    }
}
