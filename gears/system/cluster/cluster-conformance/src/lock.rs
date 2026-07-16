// Created: 2026-06-24 by Constructor Tech
//! Distributed-lock conformance scenarios (`SC-LOCK-*`).
//!
//! See the [scenario catalog](../docs/scenarios/lock.md). Cache-only plugins
//! feed the `cluster` gear's `CasBasedDistributedLockBackend`
//! (`cluster::defaults::CasBasedDistributedLockBackend` — not a
//! `cluster-conformance` dependency, so not an intra-doc link here) into this
//! suite.

use std::sync::Arc;
use std::time::Duration;

use std::future::Future;

use cluster_sdk::error::ClusterError;
use cluster_sdk::lock::DistributedLockBackend;

use crate::factory::{ScenarioBackend, run_scenario};
use crate::time::TimeControl;

/// Runs every implemented L2 lock scenario, each against a fresh backend built
/// by the async factory `make` and torn down afterward (see
/// [`ScenarioBackend`]). `time` selects the clock model: pass
/// [`TimeControl::Virtual`] for in-memory / cache-default fixtures, or
/// [`TimeControl::Real`] for a real-I/O backend (e.g. the Postgres plugin over a
/// live `sqlx` pool, which cannot use a paused clock — see [`crate::time`]).
///
/// A backend using real advisory-lock keyspace scoped to a whole server (e.g.
/// Postgres `pg_advisory_lock`) should have its `make` factory hand out a fresh
/// *server* per call, since several scenarios leave a name locked on exit; the
/// per-scenario teardown returned by the factory stops each one before the next
/// is built.
pub async fn run_lock_conformance<F, Fut>(make: F, time: TimeControl)
where
    F: Fn() -> Fut,
    Fut: Future<Output = ScenarioBackend<dyn DistributedLockBackend>>,
{
    run_scenario(make(), scenario_lock_001).await;
    run_scenario(make(), |b| scenario_lock_002(b, time)).await;
    run_scenario(make(), |b| scenario_lock_003(b, time)).await;
    run_scenario(make(), scenario_lock_004).await;
    run_scenario(make(), |b| scenario_lock_005(b, time)).await;
    run_scenario(make(), |b| scenario_lock_007(b, time)).await;
}

/// SC-LOCK-001: `try_lock` succeeds when free, returns `LockContended` when held.
pub async fn scenario_lock_001(backend: Arc<dyn DistributedLockBackend>) {
    let ttl = Duration::from_secs(30);
    let _guard = backend
        .try_lock("res", ttl)
        .await
        .expect("SC-LOCK-001: try_lock on a free lock must succeed");
    let contended = backend.try_lock("res", ttl).await;
    assert!(
        matches!(contended, Err(ClusterError::LockContended { .. })),
        "SC-LOCK-001: try_lock on a held lock must return LockContended, got {contended:?}"
    );
}

/// SC-LOCK-002: `lock` blocks up to `timeout`, then returns
/// `LockTimeout { name, waited }`.
pub async fn scenario_lock_002(backend: Arc<dyn DistributedLockBackend>, time: TimeControl) {
    // Under `Virtual`, paused time auto-advances to the next pending timer once
    // the runtime has nothing else ready to run, so awaiting `lock()` directly
    // resolves its internal timeout deterministically without a real 50ms sleep.
    // Under `Real`, the 50ms timeout simply elapses in real wall-clock time.
    time.begin();
    let ttl = Duration::from_secs(30);
    let _guard = backend
        .try_lock("res", ttl)
        .await
        .expect("hold the lock so the next acquisition must wait");
    let timed_out = backend.lock("res", ttl, Duration::from_millis(50)).await;
    match timed_out {
        Err(ClusterError::LockTimeout { name, .. }) => {
            assert_eq!(name, "res", "SC-LOCK-002: timeout reports the lock name");
        }
        other => panic!("SC-LOCK-002: expected LockTimeout, got {other:?}"),
    }
    time.end();
}

/// SC-LOCK-004: explicit `release()` wakes a waiter blocked in `lock()` —
/// the waiter acquires promptly after release, well before its own timeout.
pub async fn scenario_lock_004(backend: Arc<dyn DistributedLockBackend>) {
    let ttl = Duration::from_secs(30);
    let guard = backend.try_lock("res", ttl).await.expect("acquire");

    let waiter_backend = Arc::clone(&backend);
    let waiter = tokio::spawn(async move {
        waiter_backend
            .lock("res", ttl, Duration::from_secs(5))
            .await
    });
    // Give B time to attempt the claim and start waiting before A releases.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !waiter.is_finished(),
        "SC-LOCK-004 setup: B must still be waiting on the held lock"
    );

    guard
        .release()
        .await
        .expect("explicit release must succeed");

    let acquired = waiter
        .await
        .expect("waiter task must not panic")
        .expect("SC-LOCK-004: a blocked waiter must acquire promptly after release");
    drop(acquired);
}

/// SC-LOCK-003: a lock held by a crashed holder (guard dropped without
/// `release()`) becomes acquirable once its TTL lapses.
pub async fn scenario_lock_003(backend: Arc<dyn DistributedLockBackend>, time: TimeControl) {
    time.begin();
    let ttl = Duration::from_millis(100);
    let guard = backend
        .try_lock("m", ttl)
        .await
        .expect("SC-LOCK-003: initial acquire must succeed");
    drop(guard); // simulate crash — no I/O, no explicit release
    // Past the TTL: a reaper-driven backend (Real) must have reclaimed the
    // crashed holder's lock by now, so poll rather than single-shot to tolerate
    // reaper-tick jitter within the real wait.
    time.elapse(Duration::from_millis(200)).await;
    let acquired = poll_try_lock(&backend, "m", ttl, time).await;
    assert!(
        acquired,
        "SC-LOCK-003: lock must be acquirable after crashed-holder TTL lapses"
    );
    time.end();
}

/// SC-LOCK-005: `renew` extends an active lease; renewing an expired lock
/// returns `LockExpired`.
pub async fn scenario_lock_005(backend: Arc<dyn DistributedLockBackend>, time: TimeControl) {
    time.begin();
    let ttl = Duration::from_millis(200);
    let guard = backend
        .try_lock("m", ttl)
        .await
        .expect("SC-LOCK-005: acquire must succeed");
    // Advance partway into the lease and renew — must succeed. Only 50 ms of the
    // 200 ms lease is elapsed (not 150) so ~150 ms of margin remains for
    // scheduling and the database round trip under `Real`; leaving just 50 ms
    // let a loaded CI runner expire the lease before `renew` landed, flaking the
    // test (PGR-D5). This still verifies pre-expiry renewal.
    time.elapse(Duration::from_millis(50)).await;
    guard
        .renew(Duration::from_millis(200))
        .await
        .expect("SC-LOCK-005: renew before expiry must succeed");
    // Let the lock fully expire (well past the TTL so a reaper-driven backend
    // has reclaimed it under `Real`).
    time.elapse(Duration::from_millis(400)).await;
    let err = guard
        .renew(Duration::from_millis(100))
        .await
        .expect_err("SC-LOCK-005: renewing an expired lock must fail");
    assert!(
        matches!(err, ClusterError::LockExpired { .. }),
        "SC-LOCK-005: renewing an expired lock must return LockExpired, got {err:?}"
    );
    time.end();
}

/// SC-LOCK-007: dropping a `LockGuard` performs no remote I/O — the lock
/// persists until its TTL lapses (ADR-002: TTL is the only safety net).
pub async fn scenario_lock_007(backend: Arc<dyn DistributedLockBackend>, time: TimeControl) {
    time.begin();
    let ttl = Duration::from_millis(200);
    let guard = backend
        .try_lock("m", ttl)
        .await
        .expect("SC-LOCK-007: acquire must succeed");
    drop(guard);
    // Immediately after drop — before the TTL — the lock must still be "held"
    // because no remote release occurred.
    let still_held = backend.try_lock("m", ttl).await;
    assert!(
        matches!(still_held, Err(ClusterError::LockContended { .. })),
        "SC-LOCK-007: dropping a guard must not eagerly release the lock (got {still_held:?})"
    );
    // After the TTL lapses the lock becomes acquirable again.
    time.elapse(Duration::from_millis(400)).await;
    assert!(
        poll_try_lock(&backend, "m", ttl, time).await,
        "SC-LOCK-007: lock must be acquirable after TTL lapses post-drop"
    );
    time.end();
}

/// Attempts `try_lock(name)` until it succeeds or a bounded budget elapses.
///
/// Under [`TimeControl::Virtual`] the caller has already advanced past the TTL
/// and any fixture sweeper has run, so a single attempt is deterministic. Under
/// [`TimeControl::Real`] a reaper-driven backend (e.g. Postgres) may lag the
/// wait by up to one reaper interval, so retry with short real sleeps rather
/// than assert on a single racy attempt.
async fn poll_try_lock(
    backend: &Arc<dyn DistributedLockBackend>,
    name: &str,
    ttl: Duration,
    time: TimeControl,
) -> bool {
    let attempts = match time {
        TimeControl::Virtual => 1,
        TimeControl::Real => 40,
    };
    for i in 0..attempts {
        if backend.try_lock(name, ttl).await.is_ok() {
            return true;
        }
        if time == TimeControl::Real && i + 1 < attempts {
            tokio::time::sleep(Duration::from_millis(25)).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
    false
}

// SC-LOCK-006: a foreign holder cannot release another's lock — impractical
//   through the `DistributedLockBackend` trait alone (the owner-token is an
//   implementation detail not exposed by the trait). The invariant is covered at
//   the cache layer by SC-CACHE-008/009. Cherry-pickers can call those scenarios
//   directly; there is no `scenario_lock_006` stub to avoid implying otherwise.
// TODO(SC-LOCK-008) [L4]: a blocked `lock()` waiter is woken promptly on release
//   — fault-injection harness.
