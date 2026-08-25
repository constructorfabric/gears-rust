//! The provider registry the config-driven wiring dispatches on.
//!
//! The four provider traits live in the SDK (so plugins implement them depending
//! on `cluster-sdk` only). This registry is the wiring-side lookup: a host
//! assembles it from the provider impls in the plugin crates it links, and
//! [`ClusterWiring::from_config`](crate::ClusterWiring::from_config) resolves each
//! profile's per-primitive `provider` string against it.
//!
//! A profile must bind a cache provider; the other three primitives may be left
//! unbound (SDK default over the cache) or bound to their own provider. Each
//! primitive's registry is independent — a K8s plugin can register only
//! `ClusterLeaderElectionProvider` without a cache provider.

use std::collections::HashMap;
use std::sync::Arc;

use cluster_sdk::{ClusterCacheProvider, ClusterLeaderElectionProvider, ClusterLockProvider};

/// Name → provider lookup for all three primitives, assembled once at startup and
/// passed to [`ClusterWiring::from_config`](crate::ClusterWiring::from_config).
#[derive(Default)]
pub struct ProviderRegistry {
    cache: HashMap<&'static str, Arc<dyn ClusterCacheProvider>>,
    leader_election: HashMap<&'static str, Arc<dyn ClusterLeaderElectionProvider>>,
    lock: HashMap<&'static str, Arc<dyn ClusterLockProvider>>,
}

impl ProviderRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a cache provider. A later registration for the same name
    /// replaces the earlier one.
    #[must_use]
    pub fn with_cache_provider(mut self, provider: Arc<dyn ClusterCacheProvider>) -> Self {
        self.cache.insert(provider.provider(), provider);
        self
    }

    /// Registers a leader-election provider. A later registration for the same
    /// name replaces the earlier one.
    #[must_use]
    pub fn with_leader_election_provider(
        mut self,
        provider: Arc<dyn ClusterLeaderElectionProvider>,
    ) -> Self {
        self.leader_election.insert(provider.provider(), provider);
        self
    }

    /// Registers a distributed-lock provider. A later registration for the same
    /// name replaces the earlier one.
    #[must_use]
    pub fn with_lock_provider(mut self, provider: Arc<dyn ClusterLockProvider>) -> Self {
        self.lock.insert(provider.provider(), provider);
        self
    }

    pub(crate) fn cache_provider(&self, name: &str) -> Option<&Arc<dyn ClusterCacheProvider>> {
        self.cache.get(name)
    }

    pub(crate) fn leader_election_provider(
        &self,
        name: &str,
    ) -> Option<&Arc<dyn ClusterLeaderElectionProvider>> {
        self.leader_election.get(name)
    }

    pub(crate) fn lock_provider(&self, name: &str) -> Option<&Arc<dyn ClusterLockProvider>> {
        self.lock.get(name)
    }
}
