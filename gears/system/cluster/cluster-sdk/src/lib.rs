// Created: 2026-06-03 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-sdk-foundation-crate-scaffold:p1
//! # Cluster SDK foundation
//!
//! `cluster_sdk` is the shared, serde-free, dyn-safe contract foundation every
//! cluster coordination primitive (cache, leader election, distributed lock,
//! service discovery) builds on. It provides the cross-cutting types and
//! helpers that let the public contract evolve independently of any backend:
//!
//! - [`ClusterError`] — the unified error model, plus [`ProviderErrorKind`]
//!   for programmatic retryability classification.
//! - [`ClusterProfile`] — the typed profile marker, with [`profile_scope`] and
//!   [`validate_cluster_name`] helpers that remove magic-string profile names.
//! - [`assert_dyn_compatible!`] — a compile-time dyn-compatibility assertion
//!   harness applied per backend trait so any change that breaks
//!   dyn-compatibility fails the build.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

#[allow(
    clippy::module_name_repetitions,
    reason = "cache domain types intentionally share the `Cache*` prefix mandated by DESIGN §3.1"
)]
pub mod cache;
#[allow(
    clippy::module_name_repetitions,
    reason = "the `Cas*`/`Cache*`-prefixed default backend names are mandated by DESIGN §3.11 and disambiguate the three cache-backed defaults"
)]
pub mod defaults;
#[allow(
    clippy::module_name_repetitions,
    reason = "service-discovery domain types intentionally share the `Service*`/`ServiceDiscovery*` prefix mandated by DESIGN §3.1"
)]
pub mod discovery;
pub mod error;
pub mod gts;
#[allow(
    clippy::module_name_repetitions,
    reason = "leader-election domain types intentionally share the `Leader*`/`LeaderElection*` prefix mandated by DESIGN §3.1"
)]
pub mod leader;
#[allow(
    clippy::module_name_repetitions,
    reason = "lock domain types intentionally share the `Lock*`/`DistributedLock*` prefix mandated by DESIGN §3.1"
)]
pub mod lock;
pub mod observability;
pub mod profile;
pub mod provider;
pub mod registration;
pub mod restart;
mod scope;

pub use cache::{
    CacheCapability, CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, CacheResolverBuilder,
    CacheWatch, CacheWatchEvent, CacheWatchSender, CacheWatchTrySendError, ClusterCacheBackend,
    ClusterCacheV1, PollingPrefixWatch, validate_cache_capabilities,
};
pub use defaults::{
    CacheBasedServiceDiscoveryBackend, CasBasedDistributedLockBackend,
    CasBasedLeaderElectionBackend, ShutdownRevoke,
};
pub use discovery::{
    DiscoveryFilter, InstanceState, MetaMatch, ServiceCommandReceiver, ServiceDiscoveryBackend,
    ServiceDiscoveryCapability, ServiceDiscoveryFeatures, ServiceDiscoveryResolverBuilder,
    ServiceDiscoveryV1, ServiceHandle, ServiceInstance, ServiceRegistration, ServiceRequest,
    ServiceResponder, ServiceWatch, ServiceWatchEvent, ServiceWatchSender, StateFilter,
    TopologyChange, validate_service_discovery_capabilities,
};
pub use error::{ClusterError, ProviderErrorKind};
pub use gts::ClusterPluginSpecV1;
pub use leader::{
    ElectionConfig, LeaderElectionBackend, LeaderElectionCapability, LeaderElectionFeatures,
    LeaderElectionResolverBuilder, LeaderElectionV1, LeaderStatus, LeaderWatch, LeaderWatchEvent,
    LeaderWatchSender, ResignReceiver, ResignResponder, validate_leader_election_capabilities,
};
pub use lock::{
    DistributedLockBackend, DistributedLockV1, LockCapability, LockCommandReceiver, LockFeatures,
    LockGuard, LockRequest, LockResolverBuilder, LockResponder, validate_lock_capabilities,
};
pub use observability::{ClusterMetrics, InstrumentedCache, NoopMetrics};
pub use profile::{
    CLUSTER_NAME_RULE, ClusterProfile, is_valid_cluster_name, profile_scope, validate_cluster_name,
};
pub use provider::{ClusterCacheProvider, StopHook};
pub use registration::{
    deregister_cache_backend, deregister_leader_election_backend, deregister_lock_backend,
    deregister_service_discovery_backend, register_cache_backend, register_leader_election_backend,
    register_lock_backend, register_service_discovery_backend,
};
pub use restart::{RestartableWatch, RestartingWatch, RetryPolicy};

/// Compile-time assertion that `$trait_` is dyn-compatible (object-safe).
///
/// Apply once per backend trait. If a future change makes the trait
/// dyn-incompatible, the reference to `dyn $trait_` here fails to compile, so
/// the breakage is caught at build time rather than at a downstream `dyn` use
/// site — keeping the plugin contract stable across versions.
///
/// # Examples
/// ```
/// use cluster_sdk::assert_dyn_compatible;
///
/// trait MyBackend: Send + Sync {
///     fn ping(&self) -> bool;
/// }
/// assert_dyn_compatible!(MyBackend);
///
/// // The trait is usable as a trait object:
/// fn call(b: &dyn MyBackend) -> bool {
///     b.ping()
/// }
/// ```
// @cpt-dod:cpt-cf-clst-dod-sdk-foundation-dyn-compat:p1
#[macro_export]
macro_rules! assert_dyn_compatible {
    ($trait_:path) => {
        const _: fn() = || {
            let _: ::core::option::Option<&dyn $trait_> = ::core::option::Option::None;
        };
    };
}

#[cfg(test)]
mod tests {
    // A dyn-compatible trait must pass the harness (and so the crate compiles).
    trait SampleBackend: Send + Sync {
        fn ping(&self) -> bool;
    }
    crate::assert_dyn_compatible!(SampleBackend);

    #[test]
    fn harnessed_trait_is_usable_as_trait_object() {
        struct Stub;
        impl SampleBackend for Stub {
            fn ping(&self) -> bool {
                true
            }
        }
        let backend: &dyn SampleBackend = &Stub;
        assert!(backend.ping());
    }
}
