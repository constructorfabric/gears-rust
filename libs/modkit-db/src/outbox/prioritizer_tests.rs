use super::*;
use std::collections::HashSet;

impl SharedPrioritizer {
    fn push_dirty_at(&self, pid: i64, dirty_since: Instant) {
        self.push_dirty_impl(pid, dirty_since);
    }
}

fn make_shared() -> Arc<SharedPrioritizer> {
    Arc::new(SharedPrioritizer::new())
}

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
    sched.pop(now);
    sched.absorb([(42, now)].into_iter());
    assert!(sched.redirtied.contains(&42));
}

#[test]
fn sched_ack_processed_requeues_if_redirtied() {
    let mut sched = PartitionScheduler::new();
    let now = Instant::now();
    sched.absorb([(42, now)].into_iter());
    sched.pop(now);
    sched.absorb([(42, now)].into_iter());
    assert!(matches!(sched.ack_processed(42), AckResult::Redirtied));
    assert!(sched.pop(Instant::now()).is_some());
}

#[test]
fn sched_ack_processed_clears_error_state() {
    let mut sched = PartitionScheduler::new();
    let now = Instant::now();
    sched.absorb([(42, now)].into_iter());
    sched.pop(now);
    sched.ack_error(42);
    assert!(sched.error_state.contains_key(&42));
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
    g.error();

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
    g2.processed();

    let sched = sp.scheduler.lock().unwrap();
    assert!(!sched.error_state.contains_key(&10));
}

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

    let g1 = sp.take().unwrap();
    assert_eq!(g1.partition_id(), 10);
    g1.skipped();

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

#[test]
fn error_defers_partition() {
    let sp = make_shared();
    sp.push_dirty(42);

    let guard = sp.take().unwrap();
    guard.error();

    assert!(
        sp.take().is_none(),
        "deferred partition should not be ready"
    );

    let sched = sp.scheduler.lock().unwrap();
    assert!(sched.pending_ids.contains(&42));
}

#[test]
fn error_cooldown_cap_at_30s() {
    let sp = make_shared();
    let now = Instant::now();

    for _ in 0..25 {
        sp.push_dirty(10);
        {
            let mut sched = sp.scheduler.lock().unwrap();
            let mut inbox = sp.inbox.lock().unwrap();
            for (pid, ts) in inbox.drain() {
                if !sched.pending_ids.contains(&pid) && !sched.claimed.contains(&pid) {
                    sched.pending.insert((ts, pid));
                    sched.pending_ids.insert(pid);
                }
            }
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
    g.error();

    sp.push_dirty(10);

    let guard = sp.take().unwrap();
    assert_eq!(
        guard.partition_id(),
        10,
        "healthy partition should be served first"
    );
    guard.processed();

    let sched = sp.scheduler.lock().unwrap();
    assert!(sched.pending_ids.contains(&42));
}

#[test]
fn dropped_guard_preserves_signal() {
    let sp = make_shared();
    sp.push_dirty(42);

    {
        let _guard = sp.take().unwrap();
    }

    let guard2 = sp.take().expect("should reappear after drop");
    assert_eq!(guard2.partition_id(), 42);
    guard2.processed();
}

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

    sp.push_dirty(42);

    assert!(sp.take().is_none(), "claimed partition should be deduped");

    guard.processed();
}

#[test]
fn redirty_while_claimed_survives_another_workers_take() {
    let sp = make_shared();
    sp.push_dirty(42);

    let guard_a = sp.take().unwrap();
    assert_eq!(guard_a.partition_id(), 42);

    sp.push_dirty(42);

    assert!(sp.take().is_none());

    guard_a.processed();

    let guard = sp.take().expect(
        "re-dirty signal lost: pid=42 was dropped by dedup during \
         take() while claimed, then processed() removed it from claimed \
         - partition is now invisible until cold reconciler",
    );
    assert_eq!(guard.partition_id(), 42);
    guard.processed();
}

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

#[test]
fn coalesced_push_still_notifies() {
    let sp = make_shared();
    let notify = sp.notifier();

    sp.push_dirty(10);
    sp.push_dirty(10);

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
    let sp = make_shared();
    sp.push_dirty(10);

    let g = sp.take().unwrap();
    assert_eq!(g.partition_id(), 10);
    g.processed();

    sp.push_dirty(10);
    let g2 = sp.take().unwrap();
    assert_eq!(g2.partition_id(), 10);
    g2.processed();
}

#[test]
fn push_dirty_during_claimed_partition_preserved() {
    let sp = make_shared();
    sp.push_dirty(10);

    let g = sp.take().unwrap();
    assert_eq!(g.partition_id(), 10);

    sp.push_dirty(10);

    g.processed();

    let g2 = sp.take().unwrap();
    assert_eq!(g2.partition_id(), 10);
    g2.processed();
}

#[test]
fn multiple_partitions_interleaved_push_and_take() {
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
    let sp = make_shared();
    sp.push_dirty(10);

    {
        let _g = sp.take().unwrap();
    }

    let g = sp.take().unwrap();
    assert_eq!(g.partition_id(), 10);
    g.processed();
}

#[test]
fn coalesced_push_while_claimed_forces_redirty() {
    let sp = make_shared();
    sp.push_dirty(10);

    let g = sp.take().unwrap();
    assert_eq!(g.partition_id(), 10);

    sp.push_dirty(10);

    g.processed();

    let g2 = sp.take().unwrap();
    assert_eq!(g2.partition_id(), 10);
    g2.processed();
}
