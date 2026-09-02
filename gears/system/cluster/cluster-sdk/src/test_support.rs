//! A configurable [`ClusterClient`] stub for the SDK's own tests.
//!
//! Since `K4` the resolvers bind through the process's single `dyn ClusterClient`
//! rather than through per-profile hub registrations, so a test that wants a
//! resolvable profile registers one of these rather than a backend. That is the
//! same shape a real process has — `LocalClusterClient` in the gear, a
//! `RemoteClusterClient` in a consumer — with the profile set and the descriptor
//! written by hand.
//!
//! The descriptor is **derived from the bound backends** by default, exactly as
//! the gear's wiring derives it from the real ones, so capability validation over
//! a stub answers what the stub's backends declare and a test does not have to
//! keep two sources of truth in step.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit::client_hub::ClientHub;

use crate::cache::{CacheConsistency, CacheFeatures, ClusterCacheBackend};
use crate::client::ClusterClient;
use crate::dto::{
    CacheDescriptor, LeaderElectionDescriptor, LockDescriptor, ProfileDescriptor, ProfileHealth,
};
use crate::error::{ClusterError, ProviderErrorKind};
use crate::leader::{LeaderElectionBackend, LeaderElectionFeatures};
use crate::lock::{DistributedLockBackend, LockFeatures};

/// The provider name a derived descriptor reports: the backend's own where there
/// is one, and a placeholder where the primitive is unbound.
fn provider_of(name: Option<&'static str>) -> String {
    name.unwrap_or("stub").to_owned()
}

/// Where [`StubClusterClient::descriptor`] answers from.
enum DescriptorSource {
    /// Derived from whichever backends are bound — the default.
    Derived,
    /// A descriptor written by the test, for asserting on a server-side view that
    /// differs from the local backends (the remote case).
    Explicit(Box<ProfileDescriptor>),
    /// The descriptor cannot be fetched, and the failure is transient — which is
    /// what puts `resolve()` on the deferred-validation path (DESIGN.md).
    Unavailable,
}

/// A [`ClusterClient`] over hand-supplied backends for one profile.
pub struct StubClusterClient {
    profile: &'static str,
    cache: Option<Arc<dyn ClusterCacheBackend>>,
    lock: Option<Arc<dyn DistributedLockBackend>>,
    leader: Option<Arc<dyn LeaderElectionBackend>>,
    descriptor: DescriptorSource,
}

impl StubClusterClient {
    /// A client binding `profile` and nothing else: every primitive is unbound
    /// until a `with_*` call supplies it.
    pub fn for_profile(profile: &'static str) -> Self {
        Self {
            profile,
            cache: None,
            lock: None,
            leader: None,
            descriptor: DescriptorSource::Derived,
        }
    }

    /// Binds the cache primitive.
    #[must_use]
    pub fn with_cache(mut self, backend: Arc<dyn ClusterCacheBackend>) -> Self {
        self.cache = Some(backend);
        self
    }

    /// Binds the distributed-lock primitive.
    #[must_use]
    pub fn with_lock(mut self, backend: Arc<dyn DistributedLockBackend>) -> Self {
        self.lock = Some(backend);
        self
    }

    /// Binds the leader-election primitive.
    #[must_use]
    pub fn with_leader_election(mut self, backend: Arc<dyn LeaderElectionBackend>) -> Self {
        self.leader = Some(backend);
        self
    }

    /// Answers `descriptor()` with `descriptor` instead of deriving it — the
    /// remote shape, where what the server declares is not read off a local
    /// backend.
    #[must_use]
    pub fn with_descriptor(mut self, descriptor: ProfileDescriptor) -> Self {
        self.descriptor = DescriptorSource::Explicit(Box::new(descriptor));
        self
    }

    /// Makes `descriptor()` fail transiently, which is the cold-start condition
    /// that defers validation to readiness.
    #[must_use]
    pub fn without_descriptor(mut self) -> Self {
        self.descriptor = DescriptorSource::Unavailable;
        self
    }

    /// Registers this client in `hub` under `dyn ClusterClient`, as the gear's
    /// `start` and the consumer wiring both do.
    pub fn register(self, hub: &ClientHub) {
        hub.register::<dyn ClusterClient>(Arc::new(self));
    }

    fn not_bound(&self) -> ClusterError {
        ClusterError::ProfileNotBound {
            profile: self.profile,
        }
    }

    /// The descriptor the bound backends declare — the client-side mirror of the
    /// gear's `describe_profile`. An unbound primitive declares nothing, which
    /// reads as "cannot satisfy any requirement".
    fn derived(&self) -> ProfileDescriptor {
        let cache = self.cache.as_ref();
        let lock = self.lock.as_ref();
        let leader = self.leader.as_ref();
        ProfileDescriptor {
            name: self.profile.to_owned(),
            cache: CacheDescriptor {
                consistency: cache
                    .map_or(CacheConsistency::EventuallyConsistent, |b| b.consistency())
                    .into(),
                features: cache
                    .map_or_else(|| CacheFeatures::new(false), |b| b.features())
                    .into(),
                provider: provider_of(cache.map(|b| b.provider_name())),
            },
            lock: LockDescriptor {
                features: lock
                    .map_or_else(|| LockFeatures::new(false), |b| b.features())
                    .into(),
                provider: provider_of(lock.map(|b| b.provider_name())),
            },
            leader_election: LeaderElectionDescriptor {
                features: leader
                    .map_or_else(|| LeaderElectionFeatures::new(false), |b| b.features())
                    .into(),
                provider: provider_of(leader.map(|b| b.provider_name())),
            },
            health: ProfileHealth::Serving,
        }
    }
}

#[async_trait]
impl ClusterClient for StubClusterClient {
    fn cache_backend(&self, profile: &str) -> Result<Arc<dyn ClusterCacheBackend>, ClusterError> {
        match &self.cache {
            Some(backend) if profile == self.profile => Ok(Arc::clone(backend)),
            _ => Err(self.not_bound()),
        }
    }

    fn lock_backend(&self, profile: &str) -> Result<Arc<dyn DistributedLockBackend>, ClusterError> {
        match &self.lock {
            Some(backend) if profile == self.profile => Ok(Arc::clone(backend)),
            _ => Err(self.not_bound()),
        }
    }

    fn leader_election_backend(
        &self,
        profile: &str,
    ) -> Result<Arc<dyn LeaderElectionBackend>, ClusterError> {
        match &self.leader {
            Some(backend) if profile == self.profile => Ok(Arc::clone(backend)),
            _ => Err(self.not_bound()),
        }
    }

    async fn descriptor(&self, profile: &str) -> Result<ProfileDescriptor, ClusterError> {
        if profile != self.profile {
            return Err(self.not_bound());
        }
        match &self.descriptor {
            DescriptorSource::Derived => Ok(self.derived()),
            DescriptorSource::Explicit(descriptor) => Ok((**descriptor).clone()),
            DescriptorSource::Unavailable => Err(ClusterError::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                message: "stub: cluster unreachable".to_owned(),
            }),
        }
    }
}

/// Runs `f` in a process where no cluster endpoint can be derived.
///
/// **Not a convenience — a race fix.** With `grpc-client` on, the resolve path reads
/// `POD_NAMESPACE` (`wiring::derive_endpoint`) and *self-constructs* a remote client
/// when one can be built. `temp_env` serialises only its own callers, so a test that
/// asserts "nothing is wired" while merely *assuming* the variable is unset will race
/// any concurrent test that sets it, self-construct a client, and see a bound facade.
/// Going through `temp_env` here puts both sides under the same lock.
///
/// Observed rather than reasoned about: `resolve_with_nothing_wired_succeeds_...`
/// failed intermittently in the `grpc-client` build once a second `temp_env` caller
/// existed.
///
/// Without the feature there is no self-construction arm to suppress and this is a
/// plain `f.await`, so callers read the same in both builds.
pub async fn with_nothing_derivable<F: std::future::Future>(f: F) -> F::Output {
    #[cfg(feature = "grpc-client")]
    {
        temp_env::async_with_vars([(crate::wiring::POD_NAMESPACE_ENV, None::<&str>)], f).await
    }
    #[cfg(not(feature = "grpc-client"))]
    {
        f.await
    }
}
