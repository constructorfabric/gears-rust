//! `ClusterCapabilities` usage (`DESIGN.md:596,739-768`). Per
//! `docs/ADR/0007-service-decomposition.md` D2/D3, this wraps the platform
//! `cluster` gear's real `cluster-sdk` facades directly - no bespoke
//! coordination abstraction is introduced. There is no separate pub/sub
//! primitive; notification semantics are realized on top of
//! `ClusterCacheV1::put` + `ClusterCacheV1::watch`
//! (`infra::cluster::notifications`).

use cluster_sdk::{
    ClusterCacheV1, ClusterError, ClusterProfile, DistributedLockV1, LeaderElectionV1,
    ServiceDiscoveryV1,
};
use toolkit::client_hub::ClientHub;

/// Every Event Broker cache key / lock name / election name is scoped under
/// this prefix (`DESIGN.md:756-764`'s `evbk.*` cache-key table).
pub const CLUSTER_SCOPE_PREFIX: &str = "evbk";

/// The cluster-gear profile the event broker binds to. Operators configure
/// `modules.cluster.profiles.event-broker` (cache/lock/discovery provider
/// selection) to bind a backend to it - not decided in
/// `docs/ADR/0007-service-decomposition.md` (its "Not decided here" list),
/// resolved here as the first ticket to exercise real cluster-mode wiring.
#[derive(Debug, Clone, Copy)]
struct EventBrokerProfile;

impl ClusterProfile for EventBrokerProfile {
    const NAME: &'static str = "event-broker";
}

/// Resolved handle onto the platform `cluster` gear's four coordination
/// primitives, scoped for the Event Broker. Both standalone and cluster
/// modes resolve the same facade types (`DESIGN.md:2208`'s "variants of the
/// same module wiring") - standalone is simply backed by the
/// zero-dependency `standalone` cluster-gear provider instead of a
/// network-backed one.
pub struct EventBrokerCluster {
    pub cache: ClusterCacheV1,
    pub leader_election: LeaderElectionV1,
    pub lock: DistributedLockV1,
    pub service_discovery: ServiceDiscoveryV1,
}

impl EventBrokerCluster {
    /// Resolves and `"evbk"`-scopes all four primitives from `ClientHub`,
    /// bound to the `event-broker` cluster-gear profile (whatever backend -
    /// `standalone` or `postgres` - the operator configured for it).
    ///
    /// # Errors
    /// Returns [`ClusterError::ProfileNotBound`] if no cluster-gear provider
    /// is registered for the `event-broker` profile (the `cluster` gear must
    /// run first and declare it), or another [`ClusterError`] if a primitive
    /// fails to resolve.
    pub fn resolve(hub: &ClientHub) -> Result<Self, ClusterError> {
        Ok(Self {
            cache: ClusterCacheV1::resolver(hub)
                .profile(EventBrokerProfile)
                .resolve()?
                .scoped(CLUSTER_SCOPE_PREFIX)?,
            leader_election: LeaderElectionV1::resolver(hub)
                .profile(EventBrokerProfile)
                .resolve()?
                .scoped(CLUSTER_SCOPE_PREFIX)?,
            lock: DistributedLockV1::resolver(hub)
                .profile(EventBrokerProfile)
                .resolve()?
                .scoped(CLUSTER_SCOPE_PREFIX)?,
            service_discovery: ServiceDiscoveryV1::resolver(hub)
                .profile(EventBrokerProfile)
                .resolve()?
                .scoped(CLUSTER_SCOPE_PREFIX)?,
        })
    }
}
