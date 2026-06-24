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

use cluster_sdk::error::ClusterError;
use cluster_sdk::lock::DistributedLockBackend;

/// Runs every implemented L2 lock scenario against a fresh backend from `make`.
pub async fn run_lock_conformance<F>(make: F)
where
    F: Fn() -> Arc<dyn DistributedLockBackend>,
{
    scenario_lock_001(make()).await;
    scenario_lock_002(make()).await;
    scenario_lock_003(make()).await;
    scenario_lock_004(make()).await;
    scenario_lock_005(make()).await;
    scenario_lock_007(make()).await;
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
pub async fn scenario_lock_002(backend: Arc<dyn DistributedLockBackend>) {
    // Paused time auto-advances to the next pending timer once the runtime
    // has nothing else ready to run, so awaiting `lock()` directly resolves
    // its internal timeout deterministically without a real 50ms sleep.
    tokio::time::pause();
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
    tokio::time::resume();
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
pub async fn scenario_lock_003(backend: Arc<dyn DistributedLockBackend>) {
    tokio::time::pause();
    let ttl = Duration::from_millis(100);
    let guard = backend
        .try_lock("m", ttl)
        .await
        .expect("SC-LOCK-003: initial acquire must succeed");
    drop(guard); // simulate crash — no I/O, no explicit release
    tokio::time::advance(Duration::from_millis(200)).await;
    tokio::task::yield_now().await;
    let result = backend.try_lock("m", ttl).await;
    assert!(
        result.is_ok(),
        "SC-LOCK-003: lock must be acquirable after crashed-holder TTL lapses, got {result:?}"
    );
    tokio::time::resume();
}

/// SC-LOCK-005: `renew` extends an active lease; renewing an expired lock
/// returns `LockExpired`.
pub async fn scenario_lock_005(backend: Arc<dyn DistributedLockBackend>) {
    tokio::time::pause();
    let ttl = Duration::from_millis(200);
    let guard = backend
        .try_lock("m", ttl)
        .await
        .expect("SC-LOCK-005: acquire must succeed");
    // Advance to just before expiry and renew — must succeed.
    tokio::time::advance(Duration::from_millis(150)).await;
    tokio::task::yield_now().await;
    guard
        .renew(Duration::from_millis(200))
        .await
        .expect("SC-LOCK-005: renew before expiry must succeed");
    // Let the lock fully expire.
    tokio::time::advance(Duration::from_millis(400)).await;
    tokio::task::yield_now().await;
    let err = guard
        .renew(Duration::from_millis(100))
        .await
        .expect_err("SC-LOCK-005: renewing an expired lock must fail");
    assert!(
        matches!(err, ClusterError::LockExpired { .. }),
        "SC-LOCK-005: renewing an expired lock must return LockExpired, got {err:?}"
    );
    tokio::time::resume();
}

/// SC-LOCK-007: dropping a `LockGuard` performs no remote I/O — the lock
/// persists until its TTL lapses (ADR-002: TTL is the only safety net).
pub async fn scenario_lock_007(backend: Arc<dyn DistributedLockBackend>) {
    tokio::time::pause();
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
    tokio::time::advance(Duration::from_millis(400)).await;
    tokio::task::yield_now().await;
    assert!(
        backend.try_lock("m", ttl).await.is_ok(),
        "SC-LOCK-007: lock must be acquirable after TTL lapses post-drop"
    );
    tokio::time::resume();
}

// SC-LOCK-006: a foreign holder cannot release another's lock — impractical
//   through the `DistributedLockBackend` trait alone (the owner-token is an
//   implementation detail not exposed by the trait). The invariant is covered at
//   the cache layer by SC-CACHE-008/009. Cherry-pickers can call those scenarios
//   directly; there is no `scenario_lock_006` stub to avoid implying otherwise.
// TODO(SC-LOCK-008) [L4]: a blocked `lock()` waiter is woken promptly on release
//   — fault-injection harness.
