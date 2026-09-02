//! Runs the shared, backend-agnostic conformance suites from
//! `cf-gears-cluster-conformance` against this gear's real cache-derived default
//! backends (`CasBasedLeaderElectionBackend`, `CasBasedDistributedLockBackend`,
//! `CasBasedLeaderElectionBackend`), each built over the crate's known-good
//! linearizable `MemCache` fixture.
//!
//! This is the "first real exercise" the conformance crate's docs describe: the
//! suites live next to the SDK contract, and every plugin — starting with this
//! gear — feeds its concrete backend through the `run_*_conformance` entry
//! points. The runners build a fresh backend per scenario via the `make`
//! closure, so a fresh `MemCache` per call keeps state from leaking between
//! scenarios.
//!
//! Each suite runs under the default `current_thread` runtime because the
//! timeout/TTL scenarios drive time with `tokio::time::pause()`/`advance()`,
//! which panics on a `multi_thread` runtime.

use std::sync::Arc;

use cluster::defaults::{CasBasedDistributedLockBackend, CasBasedLeaderElectionBackend};
use cluster_conformance::MemCache;
use cluster_conformance::{
    ScenarioBackend, TimeControl, run_leader_conformance, run_lock_conformance,
};
use cluster_sdk::leader::LeaderElectionBackend;
use cluster_sdk::lock::DistributedLockBackend;

#[tokio::test]
async fn leader_election_conformance() {
    run_leader_conformance(
        || async {
            let cache = MemCache::linearizable();
            ScenarioBackend::bare(Arc::new(
                CasBasedLeaderElectionBackend::new(cache)
                    .expect("linearizable cache is accepted")
                    // Virtual clock: `TimeControl::Virtual` lapses leases with
                    // `tokio::time::advance`, which the production wall clock (H3)
                    // does not move under a paused runtime.
                    .with_virtual_clock(),
            ) as Arc<dyn LeaderElectionBackend>)
        },
        TimeControl::Virtual,
    )
    .await;
}

#[tokio::test]
async fn distributed_lock_conformance() {
    run_lock_conformance(
        || async {
            let cache = MemCache::linearizable();
            ScenarioBackend::bare(Arc::new(
                CasBasedDistributedLockBackend::new(cache)
                    .expect("linearizable cache is accepted")
                    .with_virtual_clock(),
            ) as Arc<dyn DistributedLockBackend>)
        },
        TimeControl::Virtual,
    )
    .await;
}

// The same suites, through the reserved lease keyspace (B2)
//
// The wiring no longer hands the defaults the cache handle the cache API serves:
// it hands them `reserved_lease_cache(...)`, a scoped view whose prefix no legal
// cache key can express. Every lease operation therefore traverses a
// `ScopedCacheBackend` in production — including `watch`, which the leader
// backend re-subscribes through — so the suites are re-run over that view. Two
// full passes rather than one substitution: the defaults are also constructible
// over a bare cache (the plugins' own conformance does exactly that), and both
// paths have to keep working.

/// The lease algebra is indifferent to the prefix beneath it — asserted, not
/// assumed, because every CAS predicate, fence comparison and steal in the suite
/// now runs against physically renamed keys.
#[tokio::test]
async fn leader_election_conformance_over_the_reserved_lease_keyspace() {
    run_leader_conformance(
        || async {
            let cache = cluster_sdk::reserved_lease_cache(MemCache::linearizable());
            ScenarioBackend::bare(Arc::new(
                CasBasedLeaderElectionBackend::new(cache)
                    .expect("linearizable cache is accepted")
                    .with_virtual_clock(),
            ) as Arc<dyn LeaderElectionBackend>)
        },
        TimeControl::Virtual,
    )
    .await;
}

/// As above for the lock, whose blocking waiter re-subscribes by hand through
/// `cache().watch(&key)` — the one call that has to re-apply the prefix on every
/// reconnect rather than only at subscription.
#[tokio::test]
async fn distributed_lock_conformance_over_the_reserved_lease_keyspace() {
    run_lock_conformance(
        || async {
            let cache = cluster_sdk::reserved_lease_cache(MemCache::linearizable());
            ScenarioBackend::bare(Arc::new(
                CasBasedDistributedLockBackend::new(cache)
                    .expect("linearizable cache is accepted")
                    .with_virtual_clock(),
            ) as Arc<dyn DistributedLockBackend>)
        },
        TimeControl::Virtual,
    )
    .await;
}
