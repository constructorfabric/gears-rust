use std::sync::Arc;

use super::prioritizer::SharedPrioritizer;
use super::types::OutboxMessageId;

/// The result of enqueuing one or more messages: the ids that were written and
/// the partitions they landed in, carried together with the means to wake the
/// sequencers once the enclosing transaction has committed.
///
/// Enqueue does **not** mark the partitions dirty. That happens in
/// [`flush`](Self::flush), which the caller invokes *after* the transaction
/// that wrote the rows has committed. Marking a partition dirty before its rows
/// are durable is a race: a sequencer can claim the partition, find nothing
/// committed, and clear the dirty flag before the commit lands, leaving the rows
/// for the cold reconciler. Deferring the signal to a post-commit `flush` closes
/// that window.
///
/// Several enqueues performed in one transaction combine with `+` (or `+=`) into
/// a single handle, so the whole unit of work is flushed once:
///
/// ```ignore
/// let mut pending = FlushHandle::default();
/// pending += outbox.enqueue(tx, first).await?;
/// pending += outbox.enqueue(tx, second).await?;
/// // ... commit tx ...
/// pending.flush();
/// ```
#[must_use = "a FlushHandle marks no partition dirty until flushed; call .flush() after the transaction commits"]
#[derive(Default)]
pub struct FlushHandle {
    ids: Vec<OutboxMessageId>,
    partitions: Vec<i64>,
    /// `None` before the pipeline has started (no prioritizer installed) or for
    /// an empty accumulator seed; `flush` is then a no-op.
    prioritizer: Option<Arc<SharedPrioritizer>>,
}

impl std::fmt::Debug for FlushHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlushHandle")
            .field("ids", &self.ids)
            .field("partitions", &self.partitions)
            .field("started", &self.prioritizer.is_some())
            .finish()
    }
}

impl FlushHandle {
    /// Build a handle for a freshly enqueued set of messages.
    pub(crate) fn new(
        ids: Vec<OutboxMessageId>,
        partitions: Vec<i64>,
        prioritizer: Option<Arc<SharedPrioritizer>>,
    ) -> Self {
        Self {
            ids,
            partitions,
            prioritizer,
        }
    }

    /// The ids of the enqueued messages, in enqueue order.
    #[must_use]
    pub fn ids(&self) -> &[OutboxMessageId] {
        &self.ids
    }

    /// The single enqueued id. Convenience for the common one-message enqueue.
    #[must_use]
    pub fn id(&self) -> OutboxMessageId {
        debug_assert_eq!(
            self.ids.len(),
            1,
            "FlushHandle::id() called on a handle carrying {} ids",
            self.ids.len()
        );
        self.ids[0]
    }

    /// The partitions the enqueued messages landed in.
    #[must_use]
    pub fn partitions(&self) -> &[i64] {
        &self.partitions
    }

    /// Mark every carried partition dirty, then wake the sequencers.
    ///
    /// Call this only after the transaction that wrote the rows has committed.
    /// A no-op when the pipeline has not started or nothing was enqueued.
    pub fn flush(self) {
        let Some(prioritizer) = &self.prioritizer else {
            return;
        };
        for &partition_id in &self.partitions {
            prioritizer.push_dirty(partition_id);
        }
        prioritizer.wake_sequencers();
    }
}

impl std::ops::AddAssign for FlushHandle {
    fn add_assign(&mut self, rhs: Self) {
        self.ids.extend(rhs.ids);
        self.partitions.extend(rhs.partitions);
        match (&self.prioritizer, &rhs.prioritizer) {
            (Some(lhs), Some(rhs)) => {
                debug_assert!(
                    Arc::ptr_eq(lhs, rhs),
                    "combining FlushHandles from different outboxes"
                );
            }
            (None, Some(_)) => self.prioritizer = rhs.prioritizer,
            _ => {}
        }
    }
}

impl std::ops::Add for FlushHandle {
    type Output = Self;

    fn add(mut self, rhs: Self) -> Self {
        self += rhs;
        self
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn handle(ids: &[i64], partitions: &[i64], p: Option<Arc<SharedPrioritizer>>) -> FlushHandle {
        FlushHandle::new(
            ids.iter().copied().map(OutboxMessageId).collect(),
            partitions.to_vec(),
            p,
        )
    }

    #[tokio::test]
    async fn flush_marks_partitions_dirty_and_wakes() {
        let prioritizer = Arc::new(SharedPrioritizer::new());
        let notifier = prioritizer.notifier();

        handle(&[1], &[7], Some(Arc::clone(&prioritizer))).flush();

        // The wake left a permit, so notified() resolves immediately.
        tokio::time::timeout(std::time::Duration::from_millis(50), notifier.notified())
            .await
            .expect("flush should wake the sequencers");
        // The partition is now claimable.
        assert_eq!(prioritizer.take().expect("dirty partition").partition_id(), 7);
    }

    #[test]
    fn flush_without_prioritizer_is_a_noop() {
        // Before start() (no prioritizer) and the empty seed must not panic.
        handle(&[1], &[7], None).flush();
        FlushHandle::default().flush();
    }

    #[test]
    fn id_returns_the_single_id() {
        assert_eq!(handle(&[42], &[0], None).id(), OutboxMessageId(42));
    }

    #[test]
    fn add_merges_ids_and_partitions_and_adopts_prioritizer() {
        let prioritizer = Arc::new(SharedPrioritizer::new());
        let combined =
            FlushHandle::default() + handle(&[1], &[10], Some(Arc::clone(&prioritizer)));
        let combined = combined + handle(&[2, 3], &[20], Some(prioritizer));

        assert_eq!(
            combined.ids(),
            &[OutboxMessageId(1), OutboxMessageId(2), OutboxMessageId(3)]
        );
        assert_eq!(combined.partitions(), &[10, 20]);
        assert!(combined.prioritizer.is_some(), "empty seed adopts the prioritizer");
    }
}
