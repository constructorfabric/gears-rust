// Created: 2026-10-07 by Constructor Tech
//! Lease-token conformance scenarios (`SC-LTOK-*`) — the token half of the lock
//! and leader traits.
//!
//! [`run_lock_conformance`](crate::run_lock_conformance) and
//! [`run_leader_conformance`](crate::run_leader_conformance) exercise only the
//! guard / watch half. The token half (`acquire` / `renew` / `release`, `join` /
//! `renew` / `resign`) is what the serving gear calls for every Profile-3 consumer
//! (§5.8.1, ADR-012). Like the guard suites, these scenarios are runtime-agnostic
//! and take a [`TimeControl`].

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::{ElectionConfig, LeaderElectionBackend};
use cluster_sdk::lease::LeaseToken;
use cluster_sdk::lock::DistributedLockBackend;

use crate::factory::{ScenarioBackend, run_scenario};
use crate::time::TimeControl;

/// A TTL long enough that no scenario step outlives it by accident.
const LONG_TTL: Duration = Duration::from_secs(30);
/// A TTL short enough that a scenario can wait it out under [`TimeControl::Real`].
const SHORT_TTL: Duration = Duration::from_millis(300);
/// How long past [`SHORT_TTL`] a scenario waits before treating the lease as lapsed.
/// Under `Real` this is capped by `TimeControl::REAL_CAP` (500 ms), so two elapses
/// are used where a full lapse must be certain.
const LAPSE: Duration = Duration::from_millis(400);
/// Steal-poll step and attempt budget: several seconds, not one TTL. An
/// observer-clock backend (the k8s plugin, §2.8) steals only after watching an
/// unchanged record for a full TTL, and an election's TTL reads back as
/// `leaseDurationSeconds`, rounded up to whole seconds.
const STEAL_STEP: Duration = Duration::from_millis(100);
const STEAL_ATTEMPTS: usize = 60;

/// Runs every lock token-path scenario against a fresh backend per scenario.
pub async fn run_lock_token_conformance<F, Fut>(make: F, time: TimeControl)
where
    F: Fn() -> Fut,
    Fut: Future<Output = ScenarioBackend<dyn DistributedLockBackend>>,
{
    run_scenario(make(), scenario_ltok_lock_001).await;
    run_scenario(make(), scenario_ltok_lock_002).await;
    run_scenario(make(), |b| scenario_ltok_lock_003(b, time)).await;
    run_scenario(make(), |b| scenario_ltok_lock_004(b, time)).await;
    run_scenario(make(), scenario_ltok_lock_005).await;
}

/// Runs every leader token-path scenario against a fresh backend per scenario.
pub async fn run_leader_token_conformance<F, Fut>(make: F, time: TimeControl)
where
    F: Fn() -> Fut,
    Fut: Future<Output = ScenarioBackend<dyn LeaderElectionBackend>>,
{
    run_scenario(make(), scenario_ltok_lead_001).await;
    run_scenario(make(), |b| scenario_ltok_lead_002(b, time)).await;
    run_scenario(make(), |b| scenario_ltok_lead_003(b, time)).await;
}

/// SC-LTOK-LOCK-001: `acquire` / `renew` / `release` round trip. A held name is
/// `LockContended` to another owner; a renew of the live lease succeeds; a
/// release frees it for the next acquirer.
pub async fn scenario_ltok_lock_001(backend: Arc<dyn DistributedLockBackend>) {
    let a = backend
        .acquire("res", "a", LONG_TTL)
        .await
        .expect("SC-LTOK-LOCK-001: acquire on a free name must succeed");
    let contended = backend.acquire("res", "b", LONG_TTL).await;
    assert!(
        matches!(contended, Err(ClusterError::LockContended { .. })),
        "SC-LTOK-LOCK-001: acquire on a held name must be LockContended, got {contended:?}"
    );
    backend
        .renew(&a, LONG_TTL)
        .await
        .expect("SC-LTOK-LOCK-001: renew of a live lease must succeed");
    backend
        .release(&a)
        .await
        .expect("SC-LTOK-LOCK-001: release of a live lease must succeed");
    let b = backend
        .acquire("res", "b", LONG_TTL)
        .await
        .expect("SC-LTOK-LOCK-001: a released name must be acquirable");
    backend.release(&b).await.expect("cleanup release");
}

/// SC-LTOK-LOCK-002: release is idempotent by absence, and a released token is dead:
/// a second release is `Ok`, a renew is `LockExpired`.
pub async fn scenario_ltok_lock_002(backend: Arc<dyn DistributedLockBackend>) {
    let a = backend
        .acquire("res", "a", LONG_TTL)
        .await
        .expect("SC-LTOK-LOCK-002: acquire");
    backend
        .release(&a)
        .await
        .expect("SC-LTOK-LOCK-002: release");
    backend
        .release(&a)
        .await
        .expect("SC-LTOK-LOCK-002: a repeated release must be Ok (idempotent by absence)");
    let renewed = backend.renew(&a, LONG_TTL).await;
    assert!(
        matches!(renewed, Err(ClusterError::LockExpired { .. })),
        "SC-LTOK-LOCK-002: renew of a released lease must be LockExpired, got {renewed:?}"
    );
}

/// SC-LTOK-LOCK-003: a lease that lapsed — even one nobody has taken since — cannot
/// be renewed by token: `LockExpired`.
///
/// The token-path twin of `SC-LOCK-005`. A holder-only backend (no stored deadline,
/// e.g. the k8s plugin; DESIGN §3.19.1) fails it: the lapsed claim still names its
/// holder, so the renew is accepted.
pub async fn scenario_ltok_lock_003(backend: Arc<dyn DistributedLockBackend>, time: TimeControl) {
    time.begin();
    let a = backend
        .acquire("res", "a", SHORT_TTL)
        .await
        .expect("SC-LTOK-LOCK-003: acquire");
    time.elapse(LAPSE).await;
    time.elapse(LAPSE).await;
    let renewed = backend.renew(&a, SHORT_TTL).await;
    assert!(
        matches!(renewed, Err(ClusterError::LockExpired { .. })),
        "SC-LTOK-LOCK-003: renew of a lapsed (unstolen) lease must be LockExpired, got {renewed:?}"
    );
    time.end();
}

/// SC-LTOK-LOCK-004: after a lapse and a steal, the old token is fenced out —
/// `renew` is `LockExpired`, `release` is a no-op `Ok` that leaves the
/// successor's lease untouched — and the successor's token carries a different
/// fence.
pub async fn scenario_ltok_lock_004(backend: Arc<dyn DistributedLockBackend>, time: TimeControl) {
    time.begin();
    let a = backend
        .acquire("res", "a", SHORT_TTL)
        .await
        .expect("SC-LTOK-LOCK-004: acquire");
    time.elapse(LAPSE).await;
    let b = steal_lock(&backend, "res", "b", time)
        .await
        .expect("SC-LTOK-LOCK-004: a lapsed lease must become acquirable by another owner");
    assert_ne!(
        (a.owner.as_str(), a.fence),
        (b.owner.as_str(), b.fence),
        "SC-LTOK-LOCK-004: the successor's token must not match the lapsed holder's"
    );
    let stale_renew = backend.renew(&a, LONG_TTL).await;
    assert!(
        matches!(stale_renew, Err(ClusterError::LockExpired { .. })),
        "SC-LTOK-LOCK-004: renew by a fenced-out token must be LockExpired, got {stale_renew:?}"
    );
    backend
        .release(&a)
        .await
        .expect("SC-LTOK-LOCK-004: release by a fenced-out token must be Ok (no-op)");
    let still_held = backend.acquire("res", "c", LONG_TTL).await;
    assert!(
        matches!(still_held, Err(ClusterError::LockContended { .. })),
        "SC-LTOK-LOCK-004: a stale release must not free the successor's lease, got {still_held:?}"
    );
    backend
        .renew(&b, LONG_TTL)
        .await
        .expect("SC-LTOK-LOCK-004: the successor's lease must survive the stale release");
    backend.release(&b).await.expect("cleanup release");
    time.end();
}

/// SC-LTOK-LOCK-005: the guard and token halves share one lease — a name held by a
/// guard is `LockContended` to `acquire`, and vice versa.
pub async fn scenario_ltok_lock_005(backend: Arc<dyn DistributedLockBackend>) {
    let guard = backend
        .try_lock("res", LONG_TTL)
        .await
        .expect("SC-LTOK-LOCK-005: try_lock");
    let by_token = backend.acquire("res", "a", LONG_TTL).await;
    assert!(
        matches!(by_token, Err(ClusterError::LockContended { .. })),
        "SC-LTOK-LOCK-005: acquire on a guard-held name must be LockContended, got {by_token:?}"
    );
    guard
        .release()
        .await
        .expect("SC-LTOK-LOCK-005: guard release");
    let token = backend
        .acquire("res", "a", LONG_TTL)
        .await
        .expect("SC-LTOK-LOCK-005: acquire after the guard released");
    let by_guard = backend.try_lock("res", LONG_TTL).await;
    assert!(
        matches!(by_guard, Err(ClusterError::LockContended { .. })),
        "SC-LTOK-LOCK-005: try_lock on a token-held name must be LockContended, got {:?}",
        by_guard.map(|_| ())
    );
    backend.release(&token).await.expect("cleanup release");
}

/// SC-LTOK-LEAD-001: `join` / `renew` / `resign` round trip. A second owner's join
/// is `None` while the claim is live; resign frees it (idempotently) and kills
/// the token; the next join wins.
pub async fn scenario_ltok_lead_001(backend: Arc<dyn LeaderElectionBackend>) {
    let config = ElectionConfig::default();
    let a = backend
        .join("e", "a", config)
        .await
        .expect("SC-LTOK-LEAD-001: join")
        .expect("SC-LTOK-LEAD-001: the sole candidate's join must win");
    let b = backend
        .join("e", "b", config)
        .await
        .expect("SC-LTOK-LEAD-001: second join");
    assert!(
        b.is_none(),
        "SC-LTOK-LEAD-001: a join against a live claim must be None, got {b:?}"
    );
    backend
        .renew(&a, config.ttl())
        .await
        .expect("SC-LTOK-LEAD-001: renew of a live claim must succeed");
    backend.resign(&a).await.expect("SC-LTOK-LEAD-001: resign");
    backend
        .resign(&a)
        .await
        .expect("SC-LTOK-LEAD-001: a repeated resign must be Ok (idempotent by absence)");
    let renewed = backend.renew(&a, config.ttl()).await;
    assert!(
        matches!(renewed, Err(ClusterError::LockExpired { .. })),
        "SC-LTOK-LEAD-001: renew of a resigned claim must be LockExpired, got {renewed:?}"
    );
    let b = backend
        .join("e", "b", config)
        .await
        .expect("SC-LTOK-LEAD-001: join after resign")
        .expect("SC-LTOK-LEAD-001: a resigned claim must be immediately joinable");
    backend.resign(&b).await.expect("cleanup resign");
}

/// SC-LTOK-LEAD-002: a lapsed (unstolen) claim cannot be renewed by token — the
/// election twin of `SC-LTOK-LOCK-003`.
pub async fn scenario_ltok_lead_002(backend: Arc<dyn LeaderElectionBackend>, time: TimeControl) {
    time.begin();
    let config = short_election();
    let a = backend
        .join("e", "a", config)
        .await
        .expect("SC-LTOK-LEAD-002: join")
        .expect("SC-LTOK-LEAD-002: the sole candidate's join must win");
    time.elapse(LAPSE).await;
    time.elapse(LAPSE).await;
    let renewed = backend.renew(&a, config.ttl()).await;
    assert!(
        matches!(renewed, Err(ClusterError::LockExpired { .. })),
        "SC-LTOK-LEAD-002: renew of a lapsed (unstolen) claim must be LockExpired, got {renewed:?}"
    );
    time.end();
}

/// SC-LTOK-LEAD-003: after a lapse and a takeover, the old token is fenced out —
/// renew `LockExpired`, resign a no-op that leaves the new leader in place.
pub async fn scenario_ltok_lead_003(backend: Arc<dyn LeaderElectionBackend>, time: TimeControl) {
    time.begin();
    let config = short_election();
    let a = backend
        .join("e", "a", config)
        .await
        .expect("SC-LTOK-LEAD-003: join")
        .expect("SC-LTOK-LEAD-003: the sole candidate's join must win");
    time.elapse(LAPSE).await;
    // The successor joins on a long TTL, so its claim cannot itself lapse while
    // the stale token's operations are checked against it.
    let long = ElectionConfig::default();
    let b = steal_election(&backend, "e", "b", long, time)
        .await
        .expect("SC-LTOK-LEAD-003: a lapsed claim must become joinable by another owner");
    let stale_renew = backend.renew(&a, config.ttl()).await;
    assert!(
        matches!(stale_renew, Err(ClusterError::LockExpired { .. })),
        "SC-LTOK-LEAD-003: renew by a fenced-out token must be LockExpired, got {stale_renew:?}"
    );
    backend
        .resign(&a)
        .await
        .expect("SC-LTOK-LEAD-003: resign by a fenced-out token must be Ok (no-op)");
    backend
        .renew(&b, long.ttl())
        .await
        .expect("SC-LTOK-LEAD-003: the new leader's claim must survive the stale resign");
    let c = backend
        .join("e", "c", config)
        .await
        .expect("SC-LTOK-LEAD-003: third join");
    assert!(
        c.is_none(),
        "SC-LTOK-LEAD-003: a stale resign must not free the new leader's claim, got {c:?}"
    );
    backend.resign(&b).await.expect("cleanup resign");
    time.end();
}

fn short_election() -> ElectionConfig {
    ElectionConfig::new(SHORT_TTL, 2).expect("valid short election config")
}

/// Polls `acquire` until a lapsed lease is taken over, within the steal budget.
async fn steal_lock(
    backend: &Arc<dyn DistributedLockBackend>,
    name: &str,
    owner: &str,
    time: TimeControl,
) -> Option<LeaseToken> {
    for _ in 0..STEAL_ATTEMPTS {
        match backend.acquire(name, owner, LONG_TTL).await {
            Ok(token) => return Some(token),
            Err(ClusterError::LockContended { .. }) => time.elapse(STEAL_STEP).await,
            Err(other) => panic!("steal attempt failed: {other:?}"),
        }
    }
    None
}

/// Polls `join` until a lapsed claim is taken over, within the steal budget.
async fn steal_election(
    backend: &Arc<dyn LeaderElectionBackend>,
    name: &str,
    owner: &str,
    config: ElectionConfig,
    time: TimeControl,
) -> Option<LeaseToken> {
    for _ in 0..STEAL_ATTEMPTS {
        match backend.join(name, owner, config).await {
            Ok(Some(token)) => return Some(token),
            Ok(None) => time.elapse(STEAL_STEP).await,
            Err(other) => panic!("steal attempt failed: {other:?}"),
        }
    }
    None
}
