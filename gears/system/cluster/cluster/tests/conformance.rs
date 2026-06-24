//! Runs the shared, backend-agnostic conformance suites from
//! `cf-gears-cluster-conformance` against this gear's real cache-derived default
//! backends (`CasBasedLeaderElectionBackend`, `CasBasedDistributedLockBackend`,
//! `CacheBasedServiceDiscoveryBackend`), each built over the crate's known-good
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

use cluster::defaults::{
    CacheBasedServiceDiscoveryBackend, CasBasedDistributedLockBackend,
    CasBasedLeaderElectionBackend,
};
use cluster_conformance::MemCache;
use cluster_conformance::{
    run_discovery_conformance, run_leader_conformance, run_lock_conformance,
};
use cluster_sdk::discovery::ServiceDiscoveryBackend;
use cluster_sdk::leader::LeaderElectionBackend;
use cluster_sdk::lock::DistributedLockBackend;

#[tokio::test]
async fn leader_election_conformance() {
    run_leader_conformance(|| {
        let cache = MemCache::linearizable();
        Arc::new(CasBasedLeaderElectionBackend::new(cache).expect("linearizable cache is accepted"))
            as Arc<dyn LeaderElectionBackend>
    })
    .await;
}

#[tokio::test]
async fn distributed_lock_conformance() {
    run_lock_conformance(|| {
        let cache = MemCache::linearizable();
        Arc::new(
            CasBasedDistributedLockBackend::new(cache).expect("linearizable cache is accepted"),
        ) as Arc<dyn DistributedLockBackend>
    })
    .await;
}

#[tokio::test]
async fn service_discovery_conformance() {
    run_discovery_conformance(|| {
        let cache = MemCache::linearizable();
        Arc::new(CacheBasedServiceDiscoveryBackend::new(cache)) as Arc<dyn ServiceDiscoveryBackend>
    })
    .await;
}
