//! Keeps the derived counters honest, as its own task.
//!
//! It does one thing: for a partition it has been told looks unaudited, see
//! whether that partition is *empty* - nothing unsequenced and nothing past its
//! processed cursor - and if it is, correct the counters, because an empty
//! partition's backlog is provably zero.
//!
//! Emptiness is the audit's own finding, not something the wakeup asserts. The
//! vacuum can only say "this partition was dirty and I found nothing to
//! collect", which suggests the counters and the rows disagree but says nothing
//! about work still waiting past the cursor.
//!
//! It does not run *inside* the vacuum, because then a failed chunk delete
//! would silently stop the counters from ever being looked at, and the check
//! would inherit the vacuum's hour-long idle pace for no reason of its own.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::super::core::Outbox;
use super::super::taskward::{Directive, Signal, WorkerAction};
use super::super::types::OutboxError;
use crate::Db;

/// Corrects the counters of partitions the vacuum reported as unaudited.
pub struct CounterAudit {
    pub outbox: Arc<Outbox>,
    pub db: Db,
    /// Partitions the vacuum found dirty with nothing to collect.
    pub unaudited: Arc<Signal<i64>>,
}

impl CounterAudit {
    /// Every partition whose snapshot claims a backlog.
    ///
    /// The safety net's filter, and it costs no statement: drift is by
    /// definition a claim of work that is not there, so a partition claiming
    /// nothing needs no probe. A queue that is genuinely empty is free to
    /// sweep, and a busy one only pays for the partitions that hold something.
    fn claiming_a_backlog(&self) -> Vec<i64> {
        let mut claiming = Vec::new();
        for (queue, admission) in self.outbox.bounded_queues() {
            let Some(ids) = self.outbox.queue_partition_ids(&queue) else {
                continue;
            };
            for (partition, &partition_id) in ids.iter().enumerate() {
                if !admission.partition_pending(partition).is_zero() {
                    claiming.push(partition_id);
                }
            }
        }
        claiming
    }
}

impl WorkerAction for CounterAudit {
    type Payload = ();
    type Error = OutboxError;

    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, Self::Error> {
        // Signalled partitions first, then the timer's own sweep. The signal
        // is the fast path for the one shape the vacuum can recognise; the
        // sweep is what covers the rest, and the incoming channel has no
        // signal at all - the vacuum never looks at it, and rows deleted from
        // it are never acked, so nothing bumps the marker that would bring the
        // vacuum round.
        let mut partitions = self.unaudited.take();
        for partition_id in self.claiming_a_backlog() {
            if !partitions.contains(&partition_id) {
                partitions.push(partition_id);
            }
        }
        if partitions.is_empty() {
            return Ok(Directive::idle());
        }

        let mut failure = None;
        for partition_id in partitions {
            if cancel.is_cancelled() {
                break;
            }

            // The cursor is read fresh rather than passed along: the vacuum's
            // value could be stale by the time this runs, and a stale cursor
            // would make the probe answer about the wrong seq range.
            // A correction is logged where it happens, with the drift it
            // repaired, so nothing is counted here.
            if let Err(e) = self.outbox.audit_partition(&self.db, partition_id).await {
                warn!(
                    partition_id,
                    error = %e,
                    "counter check failed, leaving the reading as it was",
                );
                // One partition's failure does not abandon the others, but it
                // must still reach the worker loop or the backoff is cleared.
                failure = Some(e);
            }
        }

        if let Some(e) = failure {
            return Err(e);
        }

        // Idle even after correcting: there is no more work than what was
        // taken, and `Proceed` here would mean "check again in case" rather
        // than "more is waiting", which is what it means everywhere else.
        Ok(Directive::idle())
    }
}
