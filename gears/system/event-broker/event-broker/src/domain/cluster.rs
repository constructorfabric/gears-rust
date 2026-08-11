//! `ClusterCapabilities` usage (`DESIGN.md:596,739-768`). Per
//! `docs/ADR/0007-service-decomposition.md` D2/D3, this wraps the platform
//! `cluster` gear's real `cluster-sdk` facades directly - no bespoke
//! coordination abstraction is introduced. There is no separate pub/sub
//! primitive; notification semantics are realized on top of
//! `ClusterCacheV1::put` + `ClusterCacheV1::watch`
//! (`infra::cluster::notifications`).

use cluster_sdk::{
    ClusterCacheV1, ClusterError, DistributedLockV1, LeaderElectionV1, ServiceDiscoveryV1,
};
use toolkit::client_hub::ClientHub;

/// Every Event Broker cache key / lock name / election name is scoped under
/// this prefix (`DESIGN.md:756-764`'s `evbk.*` cache-key table).
pub const CLUSTER_SCOPE_PREFIX: &str = "evbk";

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
    /// Resolves and `"evbk"`-scopes all four primitives from `ClientHub`.
    /// Wiring the actual resolver-builder calls (profile selection,
    /// capability requirements) is deferred to whichever ticket first
    /// exercises real cluster-mode wiring (see design.md Open Questions).
    ///
    /// # Errors
    /// Returns [`ClusterError`] if any primitive fails to resolve (missing
    /// cluster-gear profile binding, capability mismatch) once implemented.
    pub fn resolve(_hub: &ClientHub) -> Result<Self, ClusterError> {
        todo!(
            "resolve cluster-sdk facades once a cluster-gear profile is bound (see design.md Open Questions)"
        )
    }
}
