// @cpt-dod:cpt-cf-clst-dod-smoke-tests-resolution:p1
//! Contract smoke tests: per-primitive resolution and capability-mismatch
//! startup failure (`cpt-cf-clst-dod-smoke-tests-resolution`).
//!
//! Resolution succeeds for a bound backend across all four primitives, an
//! unbound profile reports [`ClusterError::ProfileNotBound`], and a declared
//! capability the bound backend cannot satisfy fails *at resolution* (startup)
//! with a [`ClusterError::CapabilityNotMet`] naming the primitive, the unmet
//! capability, and the provider.

mod common;

use std::sync::Arc;

use cluster::defaults::{
    CacheBasedServiceDiscoveryBackend, CasBasedDistributedLockBackend,
    CasBasedLeaderElectionBackend,
};
use cluster_sdk::cache::{CacheCapability, ClusterCacheBackend};
use cluster_sdk::discovery::ServiceDiscoveryCapability;
use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::LeaderElectionCapability;
use cluster_sdk::lock::LockCapability;
use cluster_sdk::profile::ClusterProfile;
use cluster_sdk::registration::{
    register_cache_backend, register_leader_election_backend, register_lock_backend,
    register_service_discovery_backend,
};
use cluster_sdk::{ClusterCacheV1, DistributedLockV1, LeaderElectionV1, ServiceDiscoveryV1};
use common::{MemCacheBackend, SmokeProfile};
use toolkit::client_hub::ClientHub;

/// Registers a cache and its three cache-derived default backends for the smoke
/// profile, returning the hub. Panics on any setup failure (a fixture wiring bug
/// is a test bug, not a contract result).
fn hub_with_all_primitives(cache: Arc<dyn ClusterCacheBackend>) -> ClientHub {
    let hub = ClientHub::new();

    let Ok(leader) = CasBasedLeaderElectionBackend::new(Arc::clone(&cache)) else {
        panic!("leader backend must construct over a linearizable cache");
    };
    let Ok(lock) = CasBasedDistributedLockBackend::new(Arc::clone(&cache)) else {
        panic!("lock backend must construct over a linearizable cache");
    };
    let discovery = CacheBasedServiceDiscoveryBackend::new(Arc::clone(&cache));

    let cache_ok = register_cache_backend(&hub, SmokeProfile::NAME, cache).is_ok();
    let leader_ok =
        register_leader_election_backend(&hub, SmokeProfile::NAME, Arc::new(leader)).is_ok();
    let lock_ok = register_lock_backend(&hub, SmokeProfile::NAME, Arc::new(lock)).is_ok();
    let discovery_ok =
        register_service_discovery_backend(&hub, SmokeProfile::NAME, Arc::new(discovery)).is_ok();
    assert!(
        cache_ok && leader_ok && lock_ok && discovery_ok,
        "registration under a valid profile must succeed"
    );
    hub
}

#[tokio::test]
async fn every_primitive_resolves_against_a_bound_backend() {
    let hub = hub_with_all_primitives(MemCacheBackend::linearizable());

    let Ok(cache) = ClusterCacheV1::resolver(&hub)
        .profile(SmokeProfile)
        .require(CacheCapability::Linearizable)
        .require(CacheCapability::PrefixWatch)
        .resolve()
    else {
        panic!("cache must resolve against the bound linearizable backend");
    };
    // The resolved facade reflects the bound backend's declared characteristics.
    assert!(cache.features().prefix_watch);

    assert!(
        LeaderElectionV1::resolver(&hub)
            .profile(SmokeProfile)
            .require(LeaderElectionCapability::Linearizable)
            .resolve()
            .is_ok(),
        "leader election must resolve against the bound backend"
    );
    assert!(
        DistributedLockV1::resolver(&hub)
            .profile(SmokeProfile)
            .require(LockCapability::Linearizable)
            .resolve()
            .is_ok(),
        "distributed lock must resolve against the bound backend"
    );
    assert!(
        ServiceDiscoveryV1::resolver(&hub)
            .profile(SmokeProfile)
            .resolve()
            .is_ok(),
        "service discovery must resolve against the bound backend"
    );
}

#[tokio::test]
async fn unbound_profile_reports_profile_not_bound() {
    let hub = ClientHub::new();
    let result = ClusterCacheV1::resolver(&hub)
        .profile(SmokeProfile)
        .resolve();
    assert!(matches!(
        result,
        Err(ClusterError::ProfileNotBound { profile: "smoke" })
    ));
}

#[tokio::test]
async fn cache_capability_mismatch_fails_startup_naming_primitive_requirement_provider() {
    let hub = ClientHub::new();
    let cache: Arc<dyn ClusterCacheBackend> = MemCacheBackend::eventually_consistent();
    assert!(register_cache_backend(&hub, SmokeProfile::NAME, cache).is_ok());

    // A linearizable requirement is unmet by the eventually-consistent backend:
    // resolution (startup) fails, naming the primitive, capability, and provider.
    let Err(ClusterError::CapabilityNotMet {
        primitive,
        capability,
        provider,
    }) = ClusterCacheV1::resolver(&hub)
        .profile(SmokeProfile)
        .require(CacheCapability::Linearizable)
        .resolve()
    else {
        panic!("an unmet linearizable requirement must fail resolution");
    };
    assert_eq!(primitive, "ClusterCacheV1");
    assert_eq!(capability, "Linearizable");
    assert!(
        provider.contains("MemCacheBackend"),
        "provider must name the concrete backend, got `{provider}`"
    );
}

#[tokio::test]
async fn cache_prefix_watch_capability_mismatch_fails_startup() {
    let hub = ClientHub::new();
    let cache: Arc<dyn ClusterCacheBackend> = MemCacheBackend::linearizable_without_prefix_watch();
    assert!(register_cache_backend(&hub, SmokeProfile::NAME, cache).is_ok());

    assert!(matches!(
        ClusterCacheV1::resolver(&hub)
            .profile(SmokeProfile)
            .require(CacheCapability::PrefixWatch)
            .resolve(),
        Err(ClusterError::CapabilityNotMet {
            primitive: "ClusterCacheV1",
            capability: "PrefixWatch",
            ..
        })
    ));
}

#[tokio::test]
async fn leader_capability_mismatch_fails_startup() {
    let hub = ClientHub::new();
    // A leader backend over an eventually-consistent cache declares
    // `linearizable == false`, so a `Linearizable` requirement is unmet.
    let cache: Arc<dyn ClusterCacheBackend> = MemCacheBackend::eventually_consistent();
    let leader = CasBasedLeaderElectionBackend::new_allow_weak_consistency(cache);
    assert!(register_leader_election_backend(&hub, SmokeProfile::NAME, Arc::new(leader)).is_ok());

    let Err(ClusterError::CapabilityNotMet {
        primitive,
        capability,
        provider,
    }) = LeaderElectionV1::resolver(&hub)
        .profile(SmokeProfile)
        .require(LeaderElectionCapability::Linearizable)
        .resolve()
    else {
        panic!("an unmet linearizable requirement must fail resolution");
    };
    assert_eq!(primitive, "LeaderElectionV1");
    assert_eq!(capability, "Linearizable");
    assert!(
        provider.contains("CasBasedLeaderElectionBackend"),
        "provider must name the concrete backend, got `{provider}`"
    );
}

#[tokio::test]
async fn lock_capability_mismatch_fails_startup() {
    let hub = ClientHub::new();
    let cache: Arc<dyn ClusterCacheBackend> = MemCacheBackend::eventually_consistent();
    let lock = CasBasedDistributedLockBackend::new_allow_weak_consistency(cache);
    assert!(register_lock_backend(&hub, SmokeProfile::NAME, Arc::new(lock)).is_ok());

    let Err(ClusterError::CapabilityNotMet {
        primitive,
        capability,
        provider,
    }) = DistributedLockV1::resolver(&hub)
        .profile(SmokeProfile)
        .require(LockCapability::Linearizable)
        .resolve()
    else {
        panic!("an unmet linearizable requirement must fail resolution");
    };
    assert_eq!(primitive, "DistributedLockV1");
    assert_eq!(capability, "Linearizable");
    assert!(
        provider.contains("CasBasedDistributedLockBackend"),
        "provider must name the concrete backend, got `{provider}`"
    );
}

#[tokio::test]
async fn service_discovery_capability_mismatch_fails_startup() {
    let hub = ClientHub::new();
    // The cache-based default evaluates metadata predicates client-side, so it
    // declares no `metadata_pushdown`; a `MetadataFiltering` requirement is unmet.
    let cache: Arc<dyn ClusterCacheBackend> = MemCacheBackend::linearizable();
    let discovery = CacheBasedServiceDiscoveryBackend::new(cache);
    assert!(
        register_service_discovery_backend(&hub, SmokeProfile::NAME, Arc::new(discovery)).is_ok()
    );

    let Err(ClusterError::CapabilityNotMet {
        primitive,
        capability,
        provider,
    }) = ServiceDiscoveryV1::resolver(&hub)
        .profile(SmokeProfile)
        .require(ServiceDiscoveryCapability::MetadataFiltering)
        .resolve()
    else {
        panic!("an unmet metadata-filtering requirement must fail resolution");
    };
    assert_eq!(primitive, "ServiceDiscoveryV1");
    assert_eq!(capability, "MetadataFiltering");
    assert!(
        provider.contains("CacheBasedServiceDiscoveryBackend"),
        "provider must name the concrete backend, got `{provider}`"
    );
}
