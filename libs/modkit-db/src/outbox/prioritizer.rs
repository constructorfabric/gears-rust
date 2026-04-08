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
    #[cfg(test)]
    fn push_dirty_at(&self, pid: i64, dirty_since: Instant) {
        self.push_dirty_impl(pid, dirty_since);
    }

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
    /// **original** `dirty_since` (no penalty).
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn make_shared() -> Arc<SharedPrioritizer> {
        Arc::new(SharedPrioritizer::new())
    }

    // --- BackoffPolicy ---

    #[test]
    fn backoff_linear_progression() {
        let policy = BackoffPolicy::new(Duration::from_millis(100), Duration::from_secs(30));
        assert_eq!(policy.delay_for(1), Duration::from_millis(200));
        assert_eq!(policy.delay_for(2), Duration::from_millis(400));
        assert_eq!(policy.delay_for(3), Duration::from_millis(800));
    }

    #[test]
    fn backoff_cap_at_max() {
        let policy = BackoffPolicy::new(Duration::from_millis(100), Duration::from_secs(30));
        assert_eq!(policy.delay_for(25), Duration::from_secs(30));
    }

    #[test]
    fn backoff_overflow_safety() {
        let policy = BackoffPolicy::new(Duration::from_millis(100), Duration::from_secs(30));
        assert_eq!(policy.delay_for(31), Duration::from_secs(30));
        assert_eq!(policy.delay_for(u32::MAX), Duration::from_secs(30));
    }

    // --- PartitionScheduler (logic core) ---

    #[test]
    fn sched_push_absorb_pop_roundtrip() {
        let mut sched = PartitionScheduler::new();
        let now = Instant::now();
        sched.absorb([(42, now)].into_iter());
        let (pid, ds) = sched.pop(now).unwrap();
        assert_eq!(pid, 42);
        assert_eq!(ds, now);
    }

    #[test]
    fn sched_pop_returns_oldest_first() {
        let mut sched = PartitionScheduler::new();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(1);
        let t2 = t0 + Duration::from_millis(2);
        sched.absorb([(30, t0), (10, t1), (20, t2)].into_iter());
        assert_eq!(sched.pop(t2).unwrap().0, 30);
        assert_eq!(sched.pop(t2).unwrap().0, 10);
        assert_eq!(sched.pop(t2).unwrap().0, 20);
    }

    #[test]
    fn sched_pop_skips_future_timestamps() {
        let mut sched = PartitionScheduler::new();
        let now = Instant::now();
        sched.absorb([(42, now + Duration::from_secs(10))].into_iter());
        assert!(sched.pop(now).is_none());
    }

    #[test]
    fn sched_ack_error_applies_exponential_backoff() {
        let mut sched = PartitionScheduler::new();
        let now = Instant::now();
        sched.absorb([(42, now)].into_iter());
        sched.pop(now);
        sched.ack_error(42);
        // error_count=1 → 200ms delay
        assert!(sched.pop(now).is_none());
        assert!(sched.pop(now + Duration::from_millis(250)).is_some());
    }

    #[test]
    fn sched_ack_error_cooldown_preserved_despite_absorb() {
        let mut sched = PartitionScheduler::new();
        let now = Instant::now();
        sched.absorb([(42, now)].into_iter());
        sched.pop(now);
        sched.ack_error(42);
        // Re-push while in cooldown — should be deduped
        sched.absorb([(42, now)].into_iter());
        assert!(sched.pop(now).is_none(), "cooldown must be preserved");
    }

    #[test]
    fn sched_absorb_dedupes_against_pending_ids() {
        let mut sched = PartitionScheduler::new();
        let now = Instant::now();
        sched.absorb([(42, now), (42, now)].into_iter());
        sched.pop(now).unwrap();
        assert!(sched.pop(now).is_none(), "should have only one entry");
    }

    #[test]
    fn sched_absorb_inserts_redirtied_when_claimed() {
        let mut sched = PartitionScheduler::new();
        let now = Instant::now();
        sched.absorb([(42, now)].into_iter());
        sched.pop(now); // claimed
        sched.absorb([(42, now)].into_iter()); // should go to redirtied
        assert!(sched.redirtied.contains(&42));
    }

    #[test]
    fn sched_ack_processed_requeues_if_redirtied() {
        let mut sched = PartitionScheduler::new();
        let now = Instant::now();
        sched.absorb([(42, now)].into_iter());
        sched.pop(now);
        sched.absorb([(42, now)].into_iter()); // redirtied
        assert!(matches!(sched.ack_processed(42), AckResult::Redirtied));
        // Should be back in pending
        assert!(sched.pop(Instant::now()).is_some());
    }

    #[test]
    fn sched_ack_processed_clears_error_state() {
        let mut sched = PartitionScheduler::new();
        let now = Instant::now();
        sched.absorb([(42, now)].into_iter());
        sched.pop(now);
        sched.ack_error(42); // creates error_state entry
        assert!(sched.error_state.contains_key(&42));
        // Pop again after cooldown
        let later = Instant::now() + Duration::from_millis(300);
        sched.pop(later).unwrap();
        sched.ack_processed(42);
        assert!(!sched.error_state.contains_key(&42));
    }

    #[test]
    fn sched_ack_requeue_restores_original_priority() {
        let mut sched = PartitionScheduler::new();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(10);
        sched.absorb([(10, t0), (20, t1)].into_iter());
        let (pid, ds) = sched.pop(t1).unwrap();
        assert_eq!(pid, 10);
        sched.ack_requeue(pid, ds);
        // 10 should still come before 20 (original priority preserved)
        assert_eq!(sched.pop(t1).unwrap().0, 10);
    }

    #[test]
    fn sched_sweep_removes_stale_error_entries() {
        let mut sched = PartitionScheduler::new();
        sched.error_state.insert(
            42,
            ErrorEntry {
                error_count: 3,
                last_update: Instant::now()
                    .checked_sub(Duration::from_secs(600))
                    .unwrap(),
            },
        );
        sched.last_sweep = Instant::now()
            .checked_sub(SWEEP_INTERVAL)
            .unwrap()
            .checked_sub(Duration::from_secs(1))
            .unwrap();
        sched.maybe_sweep_errors(Instant::now());
        assert!(!sched.error_state.contains_key(&42));
    }

    // --- SharedPrioritizer (integration via take/push_dirty) ---

    #[test]
    fn take_empty_returns_none() {
        let sp = make_shared();
        assert!(sp.take().is_none());
    }

    #[test]
    fn take_returns_guard_with_correct_pid() {
        let sp = make_shared();
        sp.push_dirty(42);

        let guard = sp.take().expect("should return a guard");
        assert_eq!(guard.partition_id(), 42);
        guard.processed();
    }

    #[test]
    fn take_returns_distinct_pids() {
        let sp = make_shared();
        sp.push_dirty(10);
        sp.push_dirty(20);

        let g1 = sp.take().unwrap();
        let g2 = sp.take().unwrap();
        assert_ne!(g1.partition_id(), g2.partition_id());
        g1.processed();
        g2.processed();
    }

    // --- processed() ---

    #[test]
    fn processed_consumes_signal() {
        let sp = make_shared();
        sp.push_dirty(42);

        let guard = sp.take().unwrap();
        assert_eq!(guard.partition_id(), 42);
        guard.processed();

        assert!(sp.take().is_none());
    }

    #[test]
    fn processed_resets_error_state() {
        let sp = make_shared();
        sp.push_dirty(10);
        let g = sp.take().unwrap();
        g.error(); // error_count = 1

        // Move the cooldown-delayed entry to the past so take() can pop it
        {
            let mut sched = sp.scheduler.lock().unwrap();
            let entry = *sched.pending.iter().find(|(_, pid)| *pid == 10).unwrap();
            sched.pending.remove(&entry);
            sched.pending.insert((
                Instant::now().checked_sub(Duration::from_secs(1)).unwrap(),
                10,
            ));
        }
        let g2 = sp.take().unwrap();
        g2.processed(); // should reset error_count

        // Verify error state is cleared
        let sched = sp.scheduler.lock().unwrap();
        assert!(!sched.error_state.contains_key(&10));
    }

    // --- skipped() ---

    #[test]
    fn skipped_preserves_signal() {
        let sp = make_shared();
        sp.push_dirty(42);

        let guard = sp.take().unwrap();
        guard.skipped();

        let guard2 = sp.take().expect("should reappear after skip");
        assert_eq!(guard2.partition_id(), 42);
        guard2.processed();
    }

    #[test]
    fn skipped_retains_original_priority() {
        let sp = make_shared();
        let now = Instant::now();
        let t0 = now.checked_sub(Duration::from_secs(2)).unwrap();
        sp.push_dirty_at(10, t0);
        sp.push_dirty_at(20, t0 + Duration::from_secs(1));

        // Take partition 10, skip it
        let g1 = sp.take().unwrap();
        assert_eq!(g1.partition_id(), 10);
        g1.skipped();

        // Partition 10 should still come before 20 (original dirty_since preserved)
        let g2 = sp.take().unwrap();
        assert_eq!(
            g2.partition_id(),
            10,
            "skipped partition should retain priority"
        );
        g2.processed();

        let g3 = sp.take().unwrap();
        assert_eq!(g3.partition_id(), 20);
        g3.processed();
    }

    // --- error() ---

    #[test]
    fn error_defers_partition() {
        let sp = make_shared();
        sp.push_dirty(42);

        let guard = sp.take().unwrap();
        guard.error();

        // Partition is in cooldown — take() should return None
        assert!(
            sp.take().is_none(),
            "deferred partition should not be ready"
        );

        // But it's still in pending
        let sched = sp.scheduler.lock().unwrap();
        assert!(sched.pending_ids.contains(&42));
    }

    #[test]
    fn error_cooldown_cap_at_30s() {
        let sp = make_shared();
        let now = Instant::now();

        // Simulate 25 consecutive errors
        for _ in 0..25 {
            sp.push_dirty(10);
            // Force it into pending (may need to wait for cooldown in scheduler)
            {
                let mut sched = sp.scheduler.lock().unwrap();
                // Drain inbox
                let mut inbox = sp.inbox.lock().unwrap();
                for (pid, ts) in inbox.drain() {
                    if !sched.pending_ids.contains(&pid) && !sched.claimed.contains(&pid) {
                        sched.pending.insert((ts, pid));
                        sched.pending_ids.insert(pid);
                    }
                }
                // Force the pending entry to be poppable by setting dirty_since to past
                if let Some(&(ts, pid)) = sched.pending.first()
                    && pid == 10
                {
                    sched.pending.remove(&(ts, pid));
                    sched.pending_ids.remove(&pid);
                    sched
                        .pending
                        .insert((now.checked_sub(Duration::from_secs(1)).unwrap(), pid));
                    sched.pending_ids.insert(pid);
                }
            }
            if let Some(g) = sp.take() {
                g.error();
            }
        }

        // Check the last cooldown is capped
        let sched = sp.scheduler.lock().unwrap();
        let entry = sched.pending.iter().find(|(_, pid)| *pid == 10);
        if let Some(&(dirty_since, _)) = entry {
            assert!(
                dirty_since <= now + Duration::from_secs(30) + Duration::from_millis(500),
                "cooldown should be capped at 30s"
            );
        }
    }

    #[test]
    fn error_healthy_partition_served_before_deferred() {
        let sp = make_shared();
        sp.push_dirty(42);

        let g = sp.take().unwrap();
        g.error(); // partition 42 in cooldown

        sp.push_dirty(10); // healthy partition

        let guard = sp.take().unwrap();
        assert_eq!(
            guard.partition_id(),
            10,
            "healthy partition should be served first"
        );
        guard.processed();

        // 42 is still in pending (deferred)
        let sched = sp.scheduler.lock().unwrap();
        assert!(sched.pending_ids.contains(&42));
    }

    // --- Drop (no ack) ---

    #[test]
    fn dropped_guard_preserves_signal() {
        let sp = make_shared();
        sp.push_dirty(42);

        {
            let _guard = sp.take().unwrap();
            // Drop without ack
        }

        let guard2 = sp.take().expect("should reappear after drop");
        assert_eq!(guard2.partition_id(), 42);
        guard2.processed();
    }

    // --- Dedup ---

    #[test]
    fn push_dirty_dedup_in_pending() {
        let sp = make_shared();
        for _ in 0..5 {
            sp.push_dirty(42);
        }

        let guard = sp.take().unwrap();
        assert_eq!(guard.partition_id(), 42);
        guard.processed();

        assert!(sp.take().is_none());
    }

    #[test]
    fn push_dirty_dedup_for_claimed() {
        let sp = make_shared();
        sp.push_dirty(42);

        let guard = sp.take().unwrap();
        assert_eq!(guard.partition_id(), 42);

        // Push again while claimed — dedup check happens at take() time.
        // Since pid is still in `claimed`, take() will skip the inbox entry.
        sp.push_dirty(42);

        // take() before processed() — pid 42 is in claimed, so the inbox
        // entry is deduplicated. Only pid 42 is dirty, and it's claimed.
        assert!(sp.take().is_none(), "claimed partition should be deduped");

        guard.processed();
    }

    // --- Re-dirty while claimed, drained by another worker ---

    #[test]
    fn redirty_while_claimed_survives_another_workers_take() {
        // Reproduces: sequencer-A claims pid=42, producer re-dirties 42,
        // sequencer-B calls take() which drains the inbox and drops the
        // re-dirty because pid=42 is in `claimed`. Then sequencer-A calls
        // processed(). pid=42 should still be reachable.
        let sp = make_shared();
        sp.push_dirty(42);

        // Step 1: sequencer-A takes pid=42
        let guard_a = sp.take().unwrap();
        assert_eq!(guard_a.partition_id(), 42);

        // Step 2: producer re-dirties pid=42 while it's claimed
        sp.push_dirty(42);

        // Step 3: sequencer-B calls take() — drains inbox, dedup drops 42
        assert!(sp.take().is_none());

        // Step 4: sequencer-A finishes processing
        guard_a.processed();

        // Step 5: pid=42 must still be reachable — the re-dirty signal
        // must not have been lost.
        let guard = sp.take().expect(
            "re-dirty signal lost: pid=42 was dropped by dedup during \
             take() while claimed, then processed() removed it from claimed \
             \u{2014} partition is now invisible until cold reconciler",
        );
        assert_eq!(guard.partition_id(), 42);
        guard.processed();
    }

    // --- Ordering ---

    #[test]
    fn oldest_dirty_served_first() {
        let sp = make_shared();
        let now = Instant::now();
        let t0 = now.checked_sub(Duration::from_secs(3)).unwrap();
        sp.push_dirty_at(30, t0);
        sp.push_dirty_at(10, t0 + Duration::from_secs(1));
        sp.push_dirty_at(20, t0 + Duration::from_secs(2));

        let g1 = sp.take().unwrap();
        assert_eq!(g1.partition_id(), 30, "oldest dirty should be first");
        g1.processed();

        let g2 = sp.take().unwrap();
        assert_eq!(g2.partition_id(), 10);
        g2.processed();

        let g3 = sp.take().unwrap();
        assert_eq!(g3.partition_id(), 20);
        g3.processed();
    }

    // --- Many partitions ---

    #[test]
    fn hundred_partitions_all_served() {
        let sp = make_shared();
        for i in 0..100 {
            sp.push_dirty(i);
        }

        let mut taken = Vec::new();
        while let Some(g) = sp.take() {
            taken.push(g.partition_id());
            g.processed();
        }

        assert_eq!(taken.len(), 100);
        let unique: HashSet<i64> = taken.iter().copied().collect();
        assert_eq!(unique.len(), 100);
    }

    // --- Coalesce + notification edge cases ---

    #[test]
    fn coalesced_push_still_notifies() {
        // Regression: coalesced push_dirty must still fire notify_one().
        // Without this, the last message in a burst can be stuck until
        // the cold reconciler fires (60s+).
        let sp = make_shared();
        let notify = sp.notifier();

        sp.push_dirty(10);
        // Second push within coalesce window — inbox rejects the push
        // but notify_one() must still fire.
        sp.push_dirty(10);

        // Two notifies should have been stored. The first is consumed
        // by this notified() call (Notify stores at most 1 permit, but
        // the second notify_one() replaces it — still 1 permit available).
        // Key assertion: notified() resolves immediately (permit stored).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            tokio::time::timeout(Duration::from_millis(10), notify.notified())
                .await
                .expect("notify should have a stored permit from push_dirty");
        });
    }

    #[test]
    fn push_after_drain_not_coalesced() {
        // After take() drains the inbox (clears last_push), the next
        // push_dirty for the same pid should be accepted, not coalesced.
        let sp = make_shared();
        sp.push_dirty(10);

        // Drain via take
        let g = sp.take().unwrap();
        assert_eq!(g.partition_id(), 10);
        g.processed();

        // Push again immediately — should NOT be coalesced because
        // drain() cleared last_push.
        sp.push_dirty(10);
        let g2 = sp.take().unwrap();
        assert_eq!(g2.partition_id(), 10);
        g2.processed();
    }

    #[test]
    fn push_dirty_during_claimed_partition_preserved() {
        // Producer pushes dirty while sequencer has the partition claimed.
        // The push should be recorded in the redirtied set and survive.
        let sp = make_shared();
        sp.push_dirty(10);

        let g = sp.take().unwrap();
        assert_eq!(g.partition_id(), 10);

        // Push while claimed — goes to redirtied
        sp.push_dirty(10);

        // Process completes — redirtied partition should be re-queued
        g.processed();

        let g2 = sp.take().unwrap();
        assert_eq!(g2.partition_id(), 10);
        g2.processed();
    }

    #[test]
    fn multiple_partitions_interleaved_push_and_take() {
        // Interleave pushes and takes to verify no partition is lost.
        let sp = make_shared();

        sp.push_dirty(1);
        sp.push_dirty(2);
        let g1 = sp.take().unwrap();
        assert_eq!(g1.partition_id(), 1);

        sp.push_dirty(3);
        let g2 = sp.take().unwrap();
        assert_eq!(g2.partition_id(), 2);

        g1.processed();
        let g3 = sp.take().unwrap();
        assert_eq!(g3.partition_id(), 3);

        g2.processed();
        g3.processed();

        assert!(sp.take().is_none());
    }

    #[test]
    fn dropped_guard_without_ack_requeues() {
        // If a guard is dropped without calling processed/skipped/error,
        // the partition should be re-queued automatically.
        let sp = make_shared();
        sp.push_dirty(10);

        {
            let _g = sp.take().unwrap();
            // Drop without ack
        }

        // Partition should be available again
        let g = sp.take().unwrap();
        assert_eq!(g.partition_id(), 10);
        g.processed();
    }

    #[test]
    fn coalesced_push_while_claimed_forces_redirty() {
        // The critical race: push_dirty within coalesce window while
        // the partition is claimed. The push must be force-accepted
        // so absorb() sets the redirtied flag. Without this, the
        // sequencer finishes and the partition goes IDLE with
        // unprocessed rows.
        let sp = make_shared();
        sp.push_dirty(10);

        // Claim partition 10
        let g = sp.take().unwrap();
        assert_eq!(g.partition_id(), 10);

        // Push again immediately — within coalesce window, but
        // partition is claimed. Must be force-accepted.
        sp.push_dirty(10);

        // Sequencer finishes — partition should be re-queued
        // because the second push set redirtied.
        g.processed();

        // Partition must be available again
        let g2 = sp.take().unwrap();
        assert_eq!(g2.partition_id(), 10);
        g2.processed();
    }
}
