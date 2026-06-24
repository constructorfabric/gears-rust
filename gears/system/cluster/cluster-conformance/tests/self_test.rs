// Created: 2026-06-24 by Constructor Tech
//! Self-test: the conformance suites must pass against the known-correct
//! [`MemCache`] fixture. This proves the cache suite's assertions are
//! satisfiable (it doesn't reject a correct backend).
//!
//! The `run_leader_conformance`/`run_lock_conformance`/`run_discovery_conformance`
//! suites are not self-tested here against a concrete backend: this crate is a
//! dependency of every plugin (so its real `[dependencies]` stay limited to
//! `cluster-sdk`, deliberately excluding the `cluster` gear's SDK-default
//! backends), and it has no in-crate implementation of `LeaderElectionBackend`
//! / `DistributedLockBackend` / `ServiceDiscoveryBackend` to dogfood them
//! against. They get their first real exercise once a plugin (including the
//! `cluster` gear's own `CasBasedLeaderElectionBackend`,
//! `CasBasedDistributedLockBackend`, and `CacheBasedServiceDiscoveryBackend`,
//! already covered by `cluster/src/defaults/{leader,lock,discovery}_tests.rs`)
//! adopts them.

use std::sync::Arc;

use cluster_conformance::fixture::MemCache;
use cluster_conformance::{
    run_cache_conformance, run_restart_conformance, run_watch_lifecycle_conformance,
};
use cluster_sdk::ClusterCacheBackend;

#[tokio::test]
async fn cache_suite_passes_against_memcache() {
    run_cache_conformance(|| MemCache::linearizable() as Arc<dyn ClusterCacheBackend>).await;
}

#[tokio::test]
async fn restart_suite_passes() {
    run_restart_conformance().await;
}

#[tokio::test]
async fn watch_lifecycle_suite_passes() {
    run_watch_lifecycle_conformance().await;
}
