// Created: 2026-06-24 by Constructor Tech
//! Leader-election conformance scenarios (`SC-LEAD-*`).
//!
//! See the [scenario catalog](../docs/scenarios/leader.md). Every plugin
//! feeds its `LeaderElectionBackend` — whether a native implementation or the
//! `cluster` gear's cache-derived default — into this suite:
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use cluster_sdk::LeaderElectionBackend;
//! # use cluster_conformance::run_leader_conformance;
//! # async fn run(make_backend: impl Fn() -> Arc<dyn LeaderElectionBackend>) {
//! run_leader_conformance(make_backend).await;
//! # }
//! ```
//!
//! `SC-LEAD-008` (the ADR-009 weak-consistency constructor guard) is not a
//! scenario here — it exercises the concrete `CasBasedLeaderElectionBackend`
//! constructors directly, which this crate deliberately does not depend on
//! (every plugin depends on `cluster-conformance`, so this crate's real
//! dependencies stay limited to `cluster-sdk`). That guard is covered by the
//! `cluster` gear's own test suite
//! (`cluster/src/defaults/leader_tests.rs::new_rejects_eventually_consistent_cache`).

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::{
    ElectionConfig, LeaderElectionBackend, LeaderStatus, LeaderWatch, LeaderWatchEvent,
};

/// Runs every implemented L2 leader-election scenario against a fresh backend
/// from `make`. `SC-LEAD-002` is asserted only when the backend declares
/// `linearizable`; weaker backends skip the single-leader guarantee.
pub async fn run_leader_conformance<F>(make: F)
where
    F: Fn() -> Arc<dyn LeaderElectionBackend>,
{
    scenario_lead_001(make()).await;
    scenario_lead_002(make()).await;
    scenario_lead_003(make()).await;
    scenario_lead_004(make()).await;
    scenario_lead_005(make()).await;
    scenario_lead_006(make()).await;
    scenario_lead_007(make()).await;
}

/// SC-LEAD-001: a single candidate becomes `Leader`.
pub async fn scenario_lead_001(backend: Arc<dyn LeaderElectionBackend>) {
    let mut watch = backend.elect("svc").await.expect("elect must succeed");
    let status = first_status(&mut watch).await;
    assert_eq!(
        status,
        LeaderStatus::Leader,
        "SC-LEAD-001: the sole candidate must become Leader"
    );
}

/// SC-LEAD-002: with N candidates, at most one observes `Leader` at any time.
/// Capability-gated on `linearizable` — advisory backends may transiently elect
/// two leaders under partition, so the strict assertion does not apply.
pub async fn scenario_lead_002(backend: Arc<dyn LeaderElectionBackend>) {
    if !backend.features().linearizable {
        // Documented fallback: the suite does not assert single-leadership for a
        // backend that does not claim linearizable election.
        return;
    }
    let mut a = backend.elect("svc").await.expect("elect a");
    let mut b = backend.elect("svc").await.expect("elect b");
    let leaders = [first_status(&mut a).await, first_status(&mut b).await]
        .into_iter()
        .filter(|s| *s == LeaderStatus::Leader)
        .count();
    assert_eq!(
        leaders, 1,
        "SC-LEAD-002: a linearizable backend must elect exactly one leader among contenders"
    );
}

/// SC-LEAD-007: `ElectionConfig::new` rejects a zero `ttl`/`max_missed_renewals`
/// (a pure config-validation scenario; the backend is unused).
#[allow(
    clippy::unused_async,
    reason = "kept `async` so every `scenario_lead_*` shares one signature the runner can `.await` uniformly"
)]
pub async fn scenario_lead_007(_backend: Arc<dyn LeaderElectionBackend>) {
    assert!(
        matches!(
            ElectionConfig::new(Duration::ZERO, 3),
            Err(ClusterError::InvalidConfig { .. })
        ),
        "SC-LEAD-007: zero ttl must be rejected"
    );
    assert!(
        matches!(
            ElectionConfig::new(Duration::from_secs(15), 0),
            Err(ClusterError::InvalidConfig { .. })
        ),
        "SC-LEAD-007: zero max_missed_renewals must be rejected"
    );
    assert!(
        matches!(
            ElectionConfig::new(Duration::from_nanos(1), 250),
            Err(ClusterError::InvalidConfig { .. })
        ),
        "SC-LEAD-007: a ttl too small for the renewal budget must be rejected"
    );
}

/// SC-LEAD-003: the elected leader's claim auto-renews without any consumer
/// action; the status stays `Leader` across multiple renewal intervals.
pub async fn scenario_lead_003(backend: Arc<dyn LeaderElectionBackend>) {
    tokio::time::pause();
    // Short TTL so renewals fire quickly under controlled time.
    // max_missed_renewals=2 → renewal_interval = ttl / 3 ≈ 100 ms.
    let config = ElectionConfig::new(Duration::from_millis(300), 2).expect("valid config");
    let mut watch = backend.elect_with_config("e", config).await.expect("elect");
    // Wait until we hold leadership.
    loop {
        match watch.changed().await {
            LeaderWatchEvent::Status(LeaderStatus::Leader) => break,
            LeaderWatchEvent::Closed(err) => panic!("SC-LEAD-003: watch closed: {err}"),
            _ => {}
        }
    }
    // Advance across 5 renewal intervals; after each yield the renewal task runs.
    for _ in 0..5 {
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        assert!(
            watch.is_leader(),
            "SC-LEAD-003: auto-renewal must keep status Leader across renewal intervals"
        );
    }
    tokio::time::resume();
}

/// SC-LEAD-004: graceful `resign()` releases the claim; a waiting follower is
/// elected within a bounded number of events on the same backend.
pub async fn scenario_lead_004(backend: Arc<dyn LeaderElectionBackend>) {
    let mut a = backend.elect("e").await.expect("elect a");
    let mut b = backend.elect("e").await.expect("elect b");
    // Drive A to Leader.
    loop {
        match a.changed().await {
            LeaderWatchEvent::Status(LeaderStatus::Leader) => break,
            LeaderWatchEvent::Closed(err) => panic!("SC-LEAD-004: a closed: {err}"),
            _ => {}
        }
    }
    a.resign().await.expect("SC-LEAD-004: resign must succeed");
    assert!(
        wait_for_leader(&mut b).await,
        "SC-LEAD-004: successor must be elected promptly after resign"
    );
}

/// SC-LEAD-005: `status()`/`is_leader()` synchronously reflect the most recently
/// observed transition without additional I/O.
pub async fn scenario_lead_005(backend: Arc<dyn LeaderElectionBackend>) {
    let mut watch = backend.elect("e").await.expect("elect");
    // After any Status event the cached snapshot must agree.
    for _ in 0..64 {
        match watch.changed().await {
            LeaderWatchEvent::Status(s) => {
                assert_eq!(
                    watch.status(),
                    s,
                    "SC-LEAD-005: status() must equal the last Status event"
                );
                assert_eq!(
                    watch.is_leader(),
                    matches!(s, LeaderStatus::Leader),
                    "SC-LEAD-005: is_leader() must agree with the last Status event"
                );
                return;
            }
            LeaderWatchEvent::Closed(err) => panic!("SC-LEAD-005: watch closed: {err}"),
            _ => {}
        }
    }
    panic!("SC-LEAD-005: no Status event observed within the bound");
}

/// SC-LEAD-006: `Status(Lost)` is transient — the watch auto-reenrols and
/// eventually delivers `Leader` or `Follower` without the consumer calling
/// `elect()` again.
pub async fn scenario_lead_006(backend: Arc<dyn LeaderElectionBackend>) {
    tokio::time::pause();
    // Very short TTL with one allowed missed renewal so loss fires quickly.
    let config = ElectionConfig::new(Duration::from_millis(100), 1).expect("valid config");
    let mut watch = backend.elect_with_config("e", config).await.expect("elect");
    loop {
        match watch.changed().await {
            LeaderWatchEvent::Status(LeaderStatus::Leader) => break,
            LeaderWatchEvent::Closed(err) => panic!("SC-LEAD-006: watch closed: {err}"),
            _ => {}
        }
    }
    // Advance past the full TTL so the renewal misses its window. After
    // `advance`, timer futures wake up but tasks still need to be polled;
    // 64 yields lets the sweeper, the renewal task, and the watch forwarder
    // all process their events before we scan the watch stream.
    tokio::time::advance(Duration::from_millis(500)).await;
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }

    let mut saw_lost = false;
    for _ in 0..256 {
        match watch.changed().await {
            LeaderWatchEvent::Status(LeaderStatus::Lost) => {
                saw_lost = true;
            }
            LeaderWatchEvent::Status(_) if saw_lost => {
                // Re-enrolled on the same watch — scenario passes.
                tokio::time::resume();
                return;
            }
            LeaderWatchEvent::Closed(err) => panic!("SC-LEAD-006: watch closed: {err}"),
            _ => {}
        }
    }
    tokio::time::resume();
    panic!(
        "SC-LEAD-006: watch must re-enrol after Lost without the consumer calling elect() again"
    );
}

/// Polls `watch.changed()` up to 64 times, returning `true` if a `Leader`
/// status is observed.
async fn wait_for_leader(watch: &mut LeaderWatch) -> bool {
    for _ in 0..64 {
        match watch.changed().await {
            LeaderWatchEvent::Status(LeaderStatus::Leader) => return true,
            LeaderWatchEvent::Closed(_) => return false,
            _ => {}
        }
    }
    false
}

/// Awaits the watch's first leadership status, skipping non-status signals.
async fn first_status(watch: &mut LeaderWatch) -> LeaderStatus {
    for _ in 0..64 {
        match watch.changed().await {
            LeaderWatchEvent::Status(status) => return status,
            LeaderWatchEvent::Closed(err) => {
                panic!("watch closed before reporting status: {err}")
            }
            _ => {}
        }
    }
    panic!("watch produced no Status event within the bound");
}
