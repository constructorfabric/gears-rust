//! # Cluster SDK foundation
//!
//! `cluster_sdk` is the shared, serde-free, dyn-safe contract foundation every
//! cluster coordination primitive (cache, leader election, distributed lock)
//! builds on. It provides the cross-cutting types and
//! helpers that let the public contract evolve independently of any backend:
//!
//! - [`ClusterError`] — the unified error model, plus [`ProviderErrorKind`]
//!   for programmatic retryability classification.
//! - [`ClusterClient`] — the one cluster object per process: a factory for the
//!   three backend traits, satisfied in-process by the cluster gear and remotely
//!   by the gRPC client (DESIGN-DEPLOYABLE-GEAR §3.1). Unfeatured, because
//!   Profile 1 needs the trait in a process that links no transport.
//! - [`ProfileDescriptor`] — what a profile's three bindings declare, which is
//!   what makes the backends' synchronous accessors answerable remotely (§5.5).
//! - [`ClusterProfile`] — the typed profile marker (the sole consumer-facing
//!   profile path; internal `profile_scope` resolution is `pub(crate)`), with the
//!   [`validate_cluster_name`] helper for validating coordination names.
//! - [`assert_dyn_compatible!`] — a compile-time dyn-compatibility assertion
//!   harness applied per backend trait so any change that breaks
//!   dyn-compatibility fails the build.

#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

/// What a resolved facade binds to, and the four steps that bind it (§4.9.3).
///
/// Private: a consumer holds a facade, and the unbound backends this module
/// defines are reachable only as `Arc<dyn _Backend>` behind one — the same rule
/// invariant I4 states for the remote handles.
mod binding;
#[allow(
    clippy::module_name_repetitions,
    reason = "cache domain types intentionally share the `Cache*` prefix mandated by DESIGN §3.1"
)]
pub mod cache;
pub mod client;
/// The `cluster.v1` contract traits — the wire projection of the backend traits.
///
/// Behind `grpc-client` because Profile 1 links no transport and no consumer names
/// these types (DESIGN-DEPLOYABLE-GEAR §3.4, §6.2). The DTOs and the error codec
/// are **not** gated: the gear needs them with no client linked.
#[cfg(feature = "grpc-client")]
pub mod contract;
pub mod convert;
/// The client-side descriptor cache (§5.5) — what makes a remote backend's
/// synchronous accessors answerable. Private for now; item `K5` widens it if its
/// readiness gate needs to read the cached health directly.
#[cfg(feature = "grpc-client")]
mod descriptors;
pub mod dto;
pub mod error;
/// The gRPC projection of the contract — generated stubs plus the generated
/// client (§6.1). Behind `grpc-client` for the same reason as
/// [`contract`](crate::contract).
#[cfg(feature = "grpc-client")]
pub mod grpc;
pub mod gts;
pub mod intern;
#[allow(
    clippy::module_name_repetitions,
    reason = "leader-election domain types intentionally share the `Leader*`/`LeaderElection*` prefix mandated by DESIGN §3.1"
)]
pub mod leader;
pub mod lease;
#[allow(
    clippy::module_name_repetitions,
    reason = "lock domain types intentionally share the `Lock*`/`DistributedLock*` prefix mandated by DESIGN §3.1"
)]
pub mod lock;
pub mod observability;
pub mod profile;
pub mod provider;
pub mod registration;
// `K5`'s requirement registry and readiness contributor. Unfeatured: Profile 1 needs
// it, and the consumer registration that would otherwise own it never runs there.
pub mod requirements;
pub mod restart;
mod scope;
// Cluster's hand-written `ConsumerRegistration`, replayed by the framework's
// proxy-wiring phase (item `K3`, §4.9.3). Behind the feature because the only
// thing it can register is the remote client; a Profile 1 process needs no
// registration at all - its local client wins by being there first.
/// A configurable `ClusterClient` stub the SDK's own tests resolve through.
///
/// Test-only and unexported: `K6`'s `cluster_sdk::testing` feature is what makes
/// an equivalent available to consumers, over a real wired hub rather than stubs.
#[cfg(test)]
mod test_support;
#[cfg(feature = "grpc-client")]
pub mod wiring;

pub use cache::{
    CacheCapability, CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, CacheResolverBuilder,
    CacheWatch, CacheWatchEvent, CacheWatchSender, CacheWatchTrySendError, ClusterCacheBackend,
    ClusterCacheV1, PollingPrefixWatch, validate_cache_capabilities,
    validate_cache_capabilities_from,
};
pub use client::ClusterClient;
// The remote implementation of that trait. Public because `K3`'s consumer
// registration constructs one; the backends it produces are not (invariant I4).
#[cfg(feature = "grpc-client")]
pub use client::remote::RemoteClusterClient;
pub use convert::{CLUSTER_ERROR_DOMAIN, ClusterWireError, LeaseContext, to_cluster_error};
// The two directions of the same codec across a `tonic::Status`: `to_status` for
// the gear's service impls, the other two for the remote backends (§6.9). Behind
// the feature because they name `tonic::Status`.
#[cfg(feature = "grpc-client")]
pub use convert::{from_lease_status, from_status, to_status};
// Only the descriptor family is re-exported here. The request/response DTOs stay
// behind `dto::` on purpose: no consumer names them (§6.2), and `dto::LockRequest`
// would collide with the SDK's existing `lock::LockRequest` guard-command enum.
pub use dto::{
    CacheConsistencyDto, CacheDescriptor, CacheFeaturesDto, LeaderElectionDescriptor,
    LeaderElectionFeaturesDto, LockDescriptor, LockFeaturesDto, ProfileDescriptor, ProfileHealth,
};
pub use error::{ClusterError, ProviderErrorKind};
pub use gts::ClusterPluginSpecV1;
pub use intern::{intern, intern_existing};
pub use leader::{
    ElectionConfig, LeaderElectionBackend, LeaderElectionCapability, LeaderElectionFeatures,
    LeaderElectionResolverBuilder, LeaderElectionV1, LeaderStatus, LeaderWatch, LeaderWatchEvent,
    LeaderWatchSender, ResignReceiver, ResignResponder, validate_leader_election_capabilities,
    validate_leader_election_capabilities_from,
};
// The native, serde-free lease types. `dto::LeaseToken` is their wire mirror and
// stays behind `dto::` with the rest of the projection (see `lease::LeaseToken`).
pub use lease::{FENCE_RETENTION_DEFAULT, LeaseClock, LeaseRecord, LeaseToken};
pub use lock::{
    DistributedLockBackend, DistributedLockV1, LockCapability, LockCommandReceiver, LockFeatures,
    LockGuard, LockRequest, LockResolverBuilder, LockResponder, validate_lock_capabilities,
    validate_lock_capabilities_from,
};
pub use observability::{ClusterMetrics, InstrumentedCache, NoopMetrics};
pub use profile::{
    CLUSTER_NAME_RULE, ClusterProfile, RegisteredProfile, is_valid_cluster_name,
    registered_profiles, validate_cluster_name,
};
pub use provider::{
    ClusterCacheProvider, ClusterLeaderElectionProvider, ClusterLockProvider, StopHook,
};
pub use registration::{
    deregister_cache_backend, deregister_leader_election_backend, deregister_lock_backend,
    register_cache_backend, register_leader_election_backend, register_lock_backend,
};
pub use requirements::{Verdict, cluster_readiness, requirements};
pub use restart::{RestartableWatch, RestartingWatch, RetryPolicy};
// Re-exported solely so `register_cluster_profile!` can expand to
// `$crate::inventory::submit!` and a consumer crate needs no `inventory`
// dependency of its own - the same reason `toolkit` re-exports it for
// `#[toolkit::gear]`. Not part of the SDK's intended surface otherwise.
#[doc(hidden)]
pub use toolkit::inventory;

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
