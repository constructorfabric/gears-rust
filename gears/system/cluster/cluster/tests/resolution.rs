// @cpt-dod:cpt-cf-clst-dod-smoke-tests-resolution:p1
//! Contract smoke tests: per-primitive resolution and capability-mismatch
//! startup failure (`cpt-cf-clst-dod-smoke-tests-resolution`).
//!
//! Resolution succeeds for a bound backend across all three primitives, a profile
//! the cluster client does not bind reports [`ClusterError::ProfileNotBound`], and
//! a declared capability the bound backend cannot satisfy fails *at resolution*
//! (startup) with a [`ClusterError::CapabilityNotMet`] naming the primitive, the
//! unmet capability, and the provider.
//!
//! Since item `K4` these go through the **real wiring**: a facade resolves through
//! the process's single `dyn ClusterClient`, which the wiring registers over the
//! profile set it published (DESIGN.md). Binding a backend
//! into the hub is no longer enough on its own, and a fixture that did so would be
//! testing a path no consumer takes.

mod common;

use std::sync::Arc;

use cluster::defaults::{CasBasedDistributedLockBackend, CasBasedLeaderElectionBackend};
use cluster::{ClusterHandle, ClusterWiring, ProfileBackends};
use cluster_sdk::cache::{CacheCapability, ClusterCacheBackend};
use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::LeaderElectionCapability;
use cluster_sdk::lock::LockCapability;
use cluster_sdk::profile::ClusterProfile;
use cluster_sdk::{ClusterCacheV1, DistributedLockV1, LeaderElectionV1};
use common::{MemCacheBackend, SmokeProfile, wire_smoke_profile};
use toolkit::client_hub::ClientHub;

/// A profile no fixture ever binds, for the "cluster is reachable and does not
/// bind this one" row of DESIGN.md's table.
#[derive(Clone, Copy)]
struct AbsentProfile;
impl ClusterProfile for AbsentProfile {
    const NAME: &'static str = "absent";
}

/// Wires the smoke profile with all three primitives bound **explicitly**, which
/// is what a weakly-consistent fixture requires: the omit-default auto-wrap
/// refuses to layer a CAS default over an eventually-consistent cache, and these
/// mismatch tests need exactly that combination to exist.
fn wire_weakly_consistent(
    hub: &Arc<ClientHub>,
    cache: Arc<dyn ClusterCacheBackend>,
) -> ClusterHandle {
    let leader = CasBasedLeaderElectionBackend::new_allow_weak_consistency(Arc::clone(&cache));
    let lock = CasBasedDistributedLockBackend::new_allow_weak_consistency(Arc::clone(&cache));
    let backends = ProfileBackends::new(cache)
        .with_leader_election(Arc::new(leader))
        .with_lock(Arc::new(lock));
    let Ok(handle) = ClusterWiring::builder(Arc::clone(hub))
        .profile(SmokeProfile, backends)
        .build_and_start()
    else {
        panic!("explicitly-bound backends must wire whatever their consistency");
    };
    handle
}

#[tokio::test]
async fn every_primitive_resolves_against_a_bound_backend() {
    let hub = Arc::new(ClientHub::new());
    let handle = wire_smoke_profile(&hub, MemCacheBackend::linearizable());

    let Ok(cache) = ClusterCacheV1::resolver(&hub)
        .profile(SmokeProfile)
        .require(CacheCapability::Linearizable)
        .require(CacheCapability::PrefixWatch)
        .resolve()
        .await
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
            .await
            .is_ok(),
        "leader election must resolve against the bound backend"
    );
    assert!(
        DistributedLockV1::resolver(&hub)
            .profile(SmokeProfile)
            .require(LockCapability::Linearizable)
            .resolve()
            .await
            .is_ok(),
        "distributed lock must resolve against the bound backend"
    );

    handle.stop().await;
}

#[tokio::test]
async fn unbound_profile_reports_profile_not_bound() {
    let hub = Arc::new(ClientHub::new());
    let handle = wire_smoke_profile(&hub, MemCacheBackend::linearizable());

    // Cluster is reachable and binds `smoke`, not `absent`: a permanent config
    // error, returned by `resolve()` itself and naming the profile the caller
    // asked for.
    let result = ClusterCacheV1::resolver(&hub)
        .profile(AbsentProfile)
        .resolve()
        .await;
    assert!(matches!(
        result,
        Err(ClusterError::ProfileNotBound { profile: "absent" })
    ));

    handle.stop().await;
}

/// The other absence, which reads identically at the call site and not at all at
/// the resolve site: **nothing** is wired in this process (§4.9.1). `resolve()`
/// tolerates it — a Profile 3 cold start looks the same — and the first call
/// names the profile, so the misconfiguration lazy binding could otherwise hide
/// surfaces at the first operation rather than never.
#[tokio::test]
async fn nothing_wired_resolves_and_the_first_call_reports_it() {
    let hub = ClientHub::new();

    let Ok(cache) = ClusterCacheV1::resolver(&hub)
        .profile(SmokeProfile)
        .resolve()
        .await
    else {
        panic!("an empty hub must not fail resolution");
    };

    assert!(matches!(
        cache.get("k").await,
        Err(ClusterError::ProfileNotBound { profile: "smoke" })
    ));
}

#[tokio::test]
async fn cache_capability_mismatch_fails_startup_naming_primitive_requirement_provider() {
    let hub = Arc::new(ClientHub::new());
    let handle = wire_weakly_consistent(&hub, MemCacheBackend::eventually_consistent());

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
        .await
    else {
        panic!("an unmet linearizable requirement must fail resolution");
    };
    assert_eq!(primitive, "ClusterCacheV1");
    assert_eq!(capability, "Linearizable");
    // On the programmatic path the descriptor's provider identity *is* the
    // backend's own `provider_name()`, so this is the same string as before `K4`.
    // Under operator config it becomes the configured provider name instead, which
    // is what an operator needs to read (§5.5).
    assert!(
        provider.contains("MemCacheBackend"),
        "provider must name the concrete backend, got `{provider}`"
    );

    handle.stop().await;
}

#[tokio::test]
async fn cache_prefix_watch_capability_mismatch_fails_startup() {
    let hub = Arc::new(ClientHub::new());
    let handle = wire_smoke_profile(&hub, MemCacheBackend::linearizable_without_prefix_watch());

    assert!(matches!(
        ClusterCacheV1::resolver(&hub)
            .profile(SmokeProfile)
            .require(CacheCapability::PrefixWatch)
            .resolve()
            .await,
        Err(ClusterError::CapabilityNotMet {
            primitive: "ClusterCacheV1",
            capability: "PrefixWatch",
            ..
        })
    ));

    handle.stop().await;
}

#[tokio::test]
async fn leader_capability_mismatch_fails_startup() {
    // A leader backend over an eventually-consistent cache declares
    // `linearizable == false`, so a `Linearizable` requirement is unmet.
    let hub = Arc::new(ClientHub::new());
    let handle = wire_weakly_consistent(&hub, MemCacheBackend::eventually_consistent());

    let Err(ClusterError::CapabilityNotMet {
        primitive,
        capability,
        provider,
    }) = LeaderElectionV1::resolver(&hub)
        .profile(SmokeProfile)
        .require(LeaderElectionCapability::Linearizable)
        .resolve()
        .await
    else {
        panic!("an unmet linearizable requirement must fail resolution");
    };
    assert_eq!(primitive, "LeaderElectionV1");
    assert_eq!(capability, "Linearizable");
    assert!(
        provider.contains("CasBasedLeaderElectionBackend"),
        "provider must name the concrete backend, got `{provider}`"
    );

    handle.stop().await;
}

#[tokio::test]
async fn lock_capability_mismatch_fails_startup() {
    let hub = Arc::new(ClientHub::new());
    let handle = wire_weakly_consistent(&hub, MemCacheBackend::eventually_consistent());

    let Err(ClusterError::CapabilityNotMet {
        primitive,
        capability,
        provider,
    }) = DistributedLockV1::resolver(&hub)
        .profile(SmokeProfile)
        .require(LockCapability::Linearizable)
        .resolve()
        .await
    else {
        panic!("an unmet linearizable requirement must fail resolution");
    };
    assert_eq!(primitive, "DistributedLockV1");
    assert_eq!(capability, "Linearizable");
    assert!(
        provider.contains("CasBasedDistributedLockBackend"),
        "provider must name the concrete backend, got `{provider}`"
    );

    handle.stop().await;
}
