use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Notify;

/// Exponential backoff with cap: `base * 2^error_count`, capped at `max`.
///
/// Pure computation — no state, no sync. Used by [`PartitionScheduler`] to
/// determine cooldown delays for errored partitions.
#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    base: Duration,
    max: Duration,
}

impl BackoffPolicy {
    /// Create a new backoff policy.
    pub const fn new(base: Duration, max: Duration) -> Self {
        Self { base, max }
    }

    /// Compute the delay for the given consecutive error count.
    pub fn delay_for(&self, error_count: u32) -> Duration {
        #[allow(clippy::cast_possible_truncation)] // base is always small (millis)
        let ms = self.base.as_millis() as u64;
        Duration::from_millis(ms.saturating_mul(1u64 << error_count.min(31))).min(self.max)
    }
}

/// Default backoff: 100ms base, 30s cap.
const DEFAULT_BACKOFF: BackoffPolicy =
    BackoffPolicy::new(Duration::from_millis(100), Duration::from_secs(30));

/// Interval between stale error-state sweeps.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Error state entries older than this are swept.
const ERROR_STATE_TTL: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// PartitionScheduler — the logic core (&mut self, no sync)
// ---------------------------------------------------------------------------

/// Result of [`PartitionScheduler::ack_processed`].
pub enum AckResult {
    /// Partition consumed. No further action needed.
    Consumed,
    /// Partition was re-dirtied while claimed. Re-inserted at `Instant::now()`.
    /// Caller should `notify_one()`.
    Redirtied,
}

/// Per-partition error tracking.
struct ErrorEntry {
    error_count: u32,
    last_update: Instant,
}

/// Pure state machine for partition scheduling. All methods take `&mut self` —
/// no `Arc`, no `Mutex`, no `Notify`. The sync wrapper
/// ([`SharedPrioritizer`]) holds this behind a `Mutex`.
///
/// Invariant: every partition ID is in exactly one of
/// {idle (not in any set), `pending`, `claimed`}.
/// The `redirtied` set is a secondary flag on `claimed`.
pub struct PartitionScheduler {
    /// Priority queue: `(dirty_since, partition_id)`. Oldest first.
    pending: BTreeSet<(Instant, i64)>,
    /// Mirror of partition IDs in `pending` for O(1) membership checks.
    pending_ids: HashSet<i64>,
    /// Partitions held by sequencer workers.
    claimed: HashSet<i64>,
    /// Partitions that were re-dirtied while claimed.
    redirtied: HashSet<i64>,
    /// Sparse error state: only partitions that have errored.
    error_state: HashMap<i64, ErrorEntry>,
    /// Monotonic timestamp of the last stale-entry sweep.
    last_sweep: Instant,
    /// Backoff policy for error cooldowns.
    backoff: BackoffPolicy,
}

impl PartitionScheduler {
    fn new() -> Self {
        Self {
            pending: BTreeSet::new(),
            pending_ids: HashSet::new(),
            claimed: HashSet::new(),
            redirtied: HashSet::new(),
            error_state: HashMap::new(),
            last_sweep: Instant::now(),
            backoff: DEFAULT_BACKOFF,
        }
    }

    /// Absorb drained inbox entries. Deduplicates against `pending_ids`
    /// (skip) and `claimed` (insert into `redirtied`).
    fn absorb(&mut self, entries: impl Iterator<Item = (i64, Instant)>) {
        for (pid, dirty_since) in entries {
            if self.pending_ids.contains(&pid) {
                continue;
            }
            if self.claimed.contains(&pid) {
                self.redirtied.insert(pid);
                continue;
            }
            self.pending.insert((dirty_since, pid));
            self.pending_ids.insert(pid);
        }
    }

    /// Sweep stale error entries if `SWEEP_INTERVAL` has elapsed.
    fn maybe_sweep_errors(&mut self, now: Instant) {
        if now.duration_since(self.last_sweep) >= SWEEP_INTERVAL {
            self.last_sweep = now;
            self.error_state
                .retain(|_, entry| now.duration_since(entry.last_update) < ERROR_STATE_TTL);
        }
    }

    /// Pop the oldest eligible partition where `dirty_since <= now`.
    /// Moves the partition from `pending` to `claimed`.
    /// Returns `(pid, dirty_since)` or `None` if no work is ready.
    fn pop(&mut self, now: Instant) -> Option<(i64, Instant)> {
        let &(dirty_since, pid) = self.pending.first()?;
        if dirty_since > now {
            return None;
        }
        self.pending.remove(&(dirty_since, pid));
        self.pending_ids.remove(&pid);
        self.claimed.insert(pid);
        Some((pid, dirty_since))
    }

    /// Whether the partition is currently claimed by a sequencer.
    fn is_claimed(&self, pid: i64) -> bool {
        self.claimed.contains(&pid)
    }

    /// Whether the pending queue has entries eligible now.
    fn has_ready_work(&self, now: Instant) -> bool {
        self.pending
            .first()
            .is_some_and(|&(dirty_since, _)| dirty_since <= now)
    }

    /// Ack: partition processed successfully. Removes from `claimed`, clears
    /// error state. Returns [`AckResult::Redirtied`] if the partition was
    /// re-dirtied while claimed (re-inserted at `Instant::now()`).
    fn ack_processed(&mut self, pid: i64) -> AckResult {
        self.claimed.remove(&pid);
        self.error_state.remove(&pid);
        if self.redirtied.remove(&pid) {
            self.reinsert(pid, Instant::now());
            AckResult::Redirtied
        } else {
            AckResult::Consumed
        }
    }

    /// Ack: partition was skipped or guard dropped without ack.
    /// Restores the partition at its original `dirty_since` (no penalty).
    fn ack_requeue(&mut self, pid: i64, dirty_since: Instant) {
        self.claimed.remove(&pid);
        self.redirtied.remove(&pid);
        self.reinsert(pid, dirty_since);
    }

    /// Ack: partition errored. Applies exponential backoff cooldown.
    fn ack_error(&mut self, pid: i64) {
        self.claimed.remove(&pid);
        self.redirtied.remove(&pid);
        let now = Instant::now();
        let entry = self.error_state.entry(pid).or_insert(ErrorEntry {
            error_count: 0,
            last_update: now,
        });
        entry.error_count += 1;
        entry.last_update = now;
        let delay = self.backoff.delay_for(entry.error_count);
        self.reinsert(pid, now + delay);
    }

    /// Insert a partition into pending.
    fn reinsert(&mut self, pid: i64, dirty_since: Instant) {
        self.pending.insert((dirty_since, pid));
        self.pending_ids.insert(pid);
    }
}

// ---------------------------------------------------------------------------
// Inbox — producer-side coalescing buffer
// ---------------------------------------------------------------------------

/// Coalesce window for inbox dedup. Duplicate `push_dirty` calls for the
/// same pid within this window are suppressed (no push, no `notify_one()`).
/// This prevents N concurrent producers from generating N notifications for
/// the same partition — only the first push within the window fires.
const INBOX_COALESCE: Duration = Duration::from_millis(10);

/// Producer-side inbox with time-based dedup to prevent redundant notifications.
///
/// Each `push_dirty(pid)` checks if a recent push for the same pid exists
/// within [`INBOX_COALESCE`]. If so, the push and its `notify_one()` are
/// suppressed. After the window expires, the next push goes through normally.
struct Inbox {
    queue: VecDeque<(i64, Instant)>,
    /// Tracks the last push time per pid for coalescing.
    last_push: HashMap<i64, Instant>,
}

impl Inbox {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            last_push: HashMap::new(),
        }
    }

    /// Try to push a pid. Returns `true` if the push was accepted (new or
    /// coalesce window expired), `false` if suppressed (duplicate within window).
    fn try_push(&mut self, pid: i64, now: Instant) -> bool {
        if let Some(&last) = self.last_push.get(&pid)
            && now.duration_since(last) < INBOX_COALESCE
        {
            return false;
        }
        self.last_push.insert(pid, now);
        self.queue.push_back((pid, now));
        true
    }

    /// Force-push bypassing the coalesce window. Used when the partition
    /// is claimed by a sequencer — the push must reach `absorb()` so the
    /// `redirtied` flag is set, guaranteeing the partition is re-processed.
    fn force_push(&mut self, pid: i64, now: Instant) {
        self.last_push.insert(pid, now);
        self.queue.push_back((pid, now));
    }

    /// Drain queued entries and clear stale coalesce entries.
    fn drain(&mut self) -> VecDeque<(i64, Instant)> {
        // Clear coalesce map — absorbed entries will be deduped
        // by pending_ids anyway. Keeping stale entries would
        // suppress legitimate re-pushes after processing completes.
        self.last_push.clear();
        std::mem::take(&mut self.queue)
    }
}

// ---------------------------------------------------------------------------
// SharedPrioritizer — thin sync wrapper
// ---------------------------------------------------------------------------

/// Priority-queue-based partition scheduler for parallel sequencer workers.
///
/// Producers call [`push_dirty()`](Self::push_dirty) to signal new work.
/// Sequencer workers call [`take()`](Self::take) to claim the highest-priority
/// partition. The returned [`PartitionGuard`] must be acked via `processed()`,
/// `skipped()`, or `error()`.
///
/// # Partition state machine
///
/// ```text
///                  push_dirty(pid)
///   ┌────────┐ ─────────────────────> ┌─────────────────┐
///   │  IDLE  │                        │    PENDING      │
///   │        │ <── processed()        │ (BTreeSet by    │
///   │  not   │     && !redirtied      │  dirty_since)   │
///   │  in    │                        └────────┬────────┘
///   │  any   │                                 │ take()
///   │  set   │                                 │ pop oldest where
///   └────────┘                                 │ dirty_since <= now
///                                              v
///                                      ┌───────────────┐
///                                      │    CLAIMED    │
///                                      │ (in claimed   │◄── push_dirty(pid)
///                                      │  set)         │    while claimed →
///                                      └──┬──┬──┬──────┘    redirtied set
///                         ┌───────────────┘  │  └────────────────┐
///                         v                  v                   v
///                  processed()         skipped()/drop()      error()
///                  if redirtied:       → PENDING             → PENDING
///                   → PENDING           (original priority)   (now + backoff)
///                    (at Instant::now)
///                  if !redirtied:
///                   → IDLE
///
///   Error cooldown: dirty_since = now + 100ms * 2^error_count (max 30s).
///   take() skips entries where dirty_since > now.
///   Error state expires after 5 min, swept every 60s.
/// ```
///
/// # Design
///
/// Uses a split-mutex design:
/// - **`inbox`**: producers push here (brief lock, no contention with sequencers)
/// - **`scheduler`**: only sequencer workers touch this (drain inbox + pop + ack)
///
/// The two mutexes are never held simultaneously, eliminating deadlock risk.
/// All state machine logic lives in [`PartitionScheduler`] (`&mut self`
/// methods) — this struct only orchestrates locking and notification.
pub struct SharedPrioritizer {
    /// Deduplicating inbox — producers push here, duplicates suppressed.
    inbox: std::sync::Mutex<Inbox>,
    /// Partition state machine — only sequencer workers touch this.
    scheduler: std::sync::Mutex<PartitionScheduler>,
    /// Sequencer wakeup signal. Owned by the prioritizer, exposed via
    /// [`notifier()`](Self::notifier) for worker subscription.
    notify: Arc<Notify>,
}

impl SharedPrioritizer {
    /// Create a new shared prioritizer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inbox: std::sync::Mutex::new(Inbox::new()),
            scheduler: std::sync::Mutex::new(PartitionScheduler::new()),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Expose the wakeup signal for worker subscription via
    /// `WorkerBuilder::notifier(prioritizer.notifier())`.
    pub fn notifier(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    /// Fire-and-forget sequencer wakeup. Used by `Outbox::flush()`.
    pub(crate) fn wake_sequencers(&self) {
        self.notify.notify_one();
    }

    /// Signal that a partition has pending work. Called by producers
    /// (enqueue, poker, sequencer re-dirty on saturation/error).
    ///
    /// Lock-contention is minimal: only the inbox mutex is held, for a
    /// single `push_back`.
    pub fn push_dirty(&self, pid: i64) {
        self.push_dirty_impl(pid, Instant::now());
    }

    /// Test-only variant that accepts an explicit timestamp instead of
    /// `Instant::now()`, allowing tests to bypass coalesce/cooldown
    /// windows without real sleeps.
    fn push_dirty_impl(&self, pid: i64, dirty_since: Instant) {
        let mut inbox = self
            .inbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut accepted = inbox.try_push(pid, dirty_since);
        if !accepted {
            // Coalesced — check if the partition is currently claimed
            // by a sequencer. If so, force the push so that absorb()
            // will set the redirtied flag. Without this, the sequencer
            // could finish processing and return the partition to IDLE
            // while new rows (committed after the drain) go unnoticed.
            let sched = self
                .scheduler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if sched.is_claimed(pid) {
                inbox.force_push(pid, dirty_since);
                accepted = true;
            }
        }
        drop(inbox);

        if accepted {
            self.notify.notify_one();
        }
    }

    /// Get the next partition to process, or `None` if no work is available.
    ///
    /// Drains the inbox, deduplicates, then pops the oldest-dirty partition
    /// whose `dirty_since <= now`.
    pub fn take(self: &Arc<Self>) -> Option<PartitionGuard> {
        // Phase 1: drain inbox into a local buffer (brief lock)
        let drained = {
            let mut inbox = self
                .inbox
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inbox.drain()
        };

        // Phase 2: absorb + pop (scheduler lock)
        let mut sched = self
            .scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        sched.absorb(drained.into_iter());

        let now = Instant::now();
        sched.maybe_sweep_errors(now);

        let (pid, dirty_since) = sched.pop(now)?;
        let has_more = sched.has_ready_work(now);
        drop(sched);

        if has_more {
            self.notify.notify_one();
        }

        Some(PartitionGuard {
            pid,
            dirty_since,
            prioritizer: Arc::clone(self),
            acked: false,
        })
    }
}

// ---------------------------------------------------------------------------
// PartitionGuard — RAII lease
// ---------------------------------------------------------------------------

/// RAII guard returned by [`SharedPrioritizer::take()`].
///
/// The caller must explicitly ack the outcome via `processed()`, `skipped()`,
/// or `error()`. Each method consumes the guard (no double-ack). If dropped
/// without ack (panic, early return), the partition is re-inserted into
/// `pending` at its original priority (same as `skipped()`).
pub struct PartitionGuard {
    pid: i64,
    dirty_since: Instant,
    prioritizer: Arc<SharedPrioritizer>,
    acked: bool,
}

impl PartitionGuard {
    /// The partition ID this guard represents.
    pub fn partition_id(&self) -> i64 {
        self.pid
    }

    /// Partition was locked and fully processed. Dirty signal consumed —
    /// unless the partition was re-dirtied while claimed, in which case
    /// it is re-inserted into `pending`.
    pub fn processed(mut self) {
        self.acked = true;
        let mut sched = self
            .prioritizer
            .scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = sched.ack_processed(self.pid);
        let should_notify =
            matches!(result, AckResult::Redirtied) || sched.has_ready_work(Instant::now());
        drop(sched);
        if should_notify {
            self.prioritizer.notify.notify_one();
        }
    }

    /// Partition lock was held by another worker (`SKIP LOCKED`).
    /// Dirty signal preserved — partition goes back to `pending` at its
    /// original `dirty_since` (no penalty).
    pub fn skipped(mut self) {
        self.acked = true;
        self.requeue();
    }

    /// Partition processing failed with a DB error.
    /// Dirty signal preserved — partition goes back to `pending` with
    /// exponential backoff cooldown.
    pub fn error(mut self) {
        self.acked = true;
        let mut sched = self
            .prioritizer
            .scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sched.ack_error(self.pid);
    }

    /// Re-insert at original priority and notify. Shared by `skipped()` and
    /// `Drop::drop()`.
    fn requeue(&self) {
        let mut sched = self
            .prioritizer
            .scheduler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sched.ack_requeue(self.pid, self.dirty_since);
        drop(sched);
        self.prioritizer.notify.notify_one();
    }
}

impl Drop for PartitionGuard {
    fn drop(&mut self) {
        if !self.acked {
            self.requeue();
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "prioritizer_tests.rs"]
mod tests;
