use std::sync::Arc;

use super::prioritizer::SharedPrioritizer;
use super::types::OutboxMessageId;

/// The result of enqueuing one or more messages: the ids that were written and
/// the partitions they landed in, carried together with the means to wake the
/// sequencers once the enclosing transaction has committed.
///
/// Enqueue does **not** mark the partitions dirty. That happens in
/// [`fire`](Self::fire), which the caller invokes *after* the transaction
/// that wrote the rows has committed. Marking a partition dirty before its rows
/// are durable is a race: a sequencer can claim the partition, find nothing
/// committed, and clear the dirty flag before the commit lands, leaving the rows
/// for the cold reconciler. Deferring the signal to a post-commit `fire` closes
/// that window.
///
/// Several enqueues performed in one transaction combine with `+` (or `+=`) into
/// a single wake, so the whole unit of work is fired once:
///
/// ```ignore
/// let mut wake = Wake::empty();
/// wake += outbox.enqueue(tx, first).await?;
/// wake += outbox.enqueue(tx, second).await?;
/// // ... commit tx ...
/// wake.fire();
/// ```
#[must_use = "a Wake wakes no sequencer until fired; call .fire() after the transaction commits"]
pub struct Wake {
    ids: Vec<OutboxMessageId>,
    partitions: Vec<i64>,
    /// `None` before the pipeline has started (no prioritizer installed) or for
    /// an empty wake; `fire` is then a no-op.
    prioritizer: Option<Arc<SharedPrioritizer>>,
}

impl std::fmt::Debug for Wake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wake")
            .field("ids", &self.ids)
            .field("partitions", &self.partitions)
            .field("started", &self.prioritizer.is_some())
            .finish()
    }
}

impl Wake {
    /// A wake that carries no work; [`fire`](Self::fire) is a no-op.
    ///
    /// This is the accumulation seed, the value a zero-enqueue path returns, and
    /// the only public constructor available to out-of-crate implementors of a
    /// trait that returns `Wake` (the real constructor is crate-private).
    /// It is deliberately not a `Default`: an empty wake is a meaningful
    /// construction, not a trivial zero value.
    pub fn empty() -> Self {
        Self {
            ids: Vec::new(),
            partitions: Vec::new(),
            prioritizer: None,
        }
    }

    /// Build a wake for a freshly enqueued set of messages.
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

    /// The partitions the enqueued messages landed in.
    #[must_use]
    pub fn partitions(&self) -> &[i64] {
        &self.partitions
    }

    /// Mark every carried partition dirty, then wake the sequencers.
    ///
    /// Call this only after the transaction that produced this wake has
    /// **committed** - check the transaction's outcome first and fire on the
    /// success path. On rollback, drop the wake instead: firing would wake
    /// the sequencer for rows that no longer exist (harmless, but pointless).
    /// A no-op when the pipeline has not started or nothing was enqueued.
    pub fn fire(mut self) {
        let Some(prioritizer) = self.prioritizer.take() else {
            return;
        };
        for partition_id in std::mem::take(&mut self.partitions) {
            prioritizer.push_dirty(partition_id);
        }
        prioritizer.wake_sequencers();
    }

    /// Deliberately drop without firing - the rollback path, where the rows
    /// were never committed and must not wake a sequencer. Marks the wake
    /// handled so [`Drop`] stays silent.
    pub fn discard(mut self) {
        self.prioritizer = None;
        self.partitions.clear();
    }
}

impl Drop for Wake {
    /// A wake that carried work but was neither fired nor discarded means
    /// rows were written and no sequencer woken, so delivery waits for the cold
    /// reconciler. `fire` and `discard` clear the wake, so this fires only on
    /// a genuine leak.
    fn drop(&mut self) {
        if self.prioritizer.is_some() && !self.partitions.is_empty() {
            tracing::warn!(
                messages = self.ids.len(),
                partitions = ?self.partitions,
                "Wake dropped unhandled: any committed rows wait for the cold \
                 reconciler. fire() after commit, discard() on rollback."
            );
        }
    }
}

impl std::ops::AddAssign for Wake {
    fn add_assign(&mut self, mut rhs: Self) {
        // Drain rhs in place rather than move its fields out: Wake now
        // implements Drop, so a partial move is disallowed. Emptied, rhs drops
        // as a no-op.
        self.ids.append(&mut rhs.ids);
        self.partitions.append(&mut rhs.partitions);
        // Keep partitions distinct, exactly as enqueue_batch does: several
        // enqueues into one partition within a unit of work must mark it dirty
        // once, not once per message. ids are left as-is - every message is a
        // distinct row.
        self.partitions.sort_unstable();
        self.partitions.dedup();
        if let Some(rhs_prioritizer) = rhs.prioritizer.take() {
            match &self.prioritizer {
                // A wake carries exactly one prioritizer, and fire wakes only
                // that one. Merging handles from two different outboxes would
                // silently drop rhs's prioritizer and leave its partitions for
                // the cold reconciler, so this is a hard error in release too,
                // not a debug-only guard.
                Some(lhs) => assert!(
                    Arc::ptr_eq(lhs, &rhs_prioritizer),
                    "combining Wakes from different outboxes"
                ),
                None => self.prioritizer = Some(rhs_prioritizer),
            }
        }
    }
}

impl std::ops::Add for Wake {
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

    fn handle(ids: &[i64], partitions: &[i64], p: Option<Arc<SharedPrioritizer>>) -> Wake {
        Wake::new(
            ids.iter().copied().map(OutboxMessageId).collect(),
            partitions.to_vec(),
            p,
        )
    }

    #[tokio::test]
    async fn fire_marks_partitions_dirty_and_wakes() {
        let prioritizer = Arc::new(SharedPrioritizer::new());
        let notifier = prioritizer.notifier();

        handle(&[1], &[7], Some(Arc::clone(&prioritizer))).fire();

        // The wake left a permit, so notified() resolves immediately.
        tokio::time::timeout(std::time::Duration::from_millis(50), notifier.notified())
            .await
            .expect("fire should wake the sequencers");
        // The partition is now claimable.
        assert_eq!(
            prioritizer.take().expect("dirty partition").partition_id(),
            7
        );
    }

    #[tokio::test]
    async fn fire_with_nothing_to_wake_is_a_noop() {
        // An empty wake that still carries a live prioritizer must wake
        // nothing - it has no partitions to dirty.
        let prioritizer = Arc::new(SharedPrioritizer::new());
        Wake::new(Vec::new(), Vec::new(), Some(Arc::clone(&prioritizer))).fire();
        assert!(
            prioritizer.take().is_none(),
            "firing an empty wake must not mark any partition dirty"
        );

        // And a wake with no prioritizer (before start / the empty seed) must
        // not panic.
        handle(&[1], &[7], None).fire();
        Wake::empty().fire();
    }

    #[tokio::test]
    async fn add_merges_and_one_fire_marks_every_partition_dirty() {
        let prioritizer = Arc::new(SharedPrioritizer::new());
        let combined = Wake::empty() + handle(&[1], &[10], Some(Arc::clone(&prioritizer)));
        let combined = combined + handle(&[2, 3], &[20], Some(Arc::clone(&prioritizer)));

        assert_eq!(
            combined.ids(),
            &[OutboxMessageId(1), OutboxMessageId(2), OutboxMessageId(3)]
        );
        assert_eq!(combined.partitions(), &[10, 20]);

        // The accumulate-then-fire path finalization relies on: one fire of the
        // merged wake must mark every carried partition dirty. (This also
        // proves the empty seed adopted the prioritizer - otherwise fire would
        // be a no-op and nothing would be dirtied.)
        combined.fire();
        let mut dirtied = Vec::new();
        while let Some(guard) = prioritizer.take() {
            dirtied.push(guard.partition_id());
            guard.processed();
        }
        dirtied.sort_unstable();
        assert_eq!(dirtied, [10, 20]);
    }

    #[test]
    fn add_keeps_partitions_distinct() {
        let prioritizer = Arc::new(SharedPrioritizer::new());
        // Two enqueues into the same partition within one unit of work.
        let combined = handle(&[1], &[10], Some(Arc::clone(&prioritizer)))
            + handle(&[2], &[10], Some(prioritizer));

        assert_eq!(
            combined.ids(),
            &[OutboxMessageId(1), OutboxMessageId(2)],
            "every message is a distinct row"
        );
        assert_eq!(
            combined.partitions(),
            &[10],
            "the partition is carried once"
        );
        combined.discard();
    }
}
