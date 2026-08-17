// Created: 2026-08-13 by Constructor Tech
//! [`RemoteClusterClient`] — Profile 3's half of the process seam
//! (DESIGN-DEPLOYABLE-GEAR §3.1, §12.9).
//!
//! The counterpart of the gear's `LocalClusterClient`: the same
//! [`ClusterClient`] trait, the same three factory methods, and the same
//! `descriptor()`. What differs is only what a factory call produces — a
//! `Remote*Backend` over this client's channel instead of the profile's real
//! backend `Arc`. A consumer's source file cannot tell the two apart, which is
//! invariant I1.
//!
//! # Nothing here touches the network
//!
//! [`connect_lazy`](RemoteClusterClient::connect_lazy) builds a lazy channel and
//! the factory methods clone handles. **Startup never blocks on cluster
//! reachability** (invariant I6): the registration that builds this client runs
//! in the framework's wiring phase, and a cluster that is not up yet must not
//! stop a consumer from starting. The first RPC is what connects.
//!
//! One measured caveat for `K3`: `connect_lazy` needs a Tokio **reactor context**
//! and panics without one, because hyper-util's connector asks for the runtime
//! handle at construction. It performs no I/O — the tests enter a runtime and
//! never drive it — but the registration replay must run inside a runtime, which
//! the host's wiring phase does.
//!
//! # One channel, four stubs
//!
//! A [`Channel`] multiplexes over HTTP/2, so one per process serves every
//! profile and every primitive; the stub clients are thin wrappers over it and
//! cloning one is a refcount. That is why the profile rides on each *request*
//! rather than being wired per profile (§3.1): nothing here is per-profile except
//! the interned name a backend handle carries.
//!
//! # No RPC deadline is set, and that is deliberate
//!
//! The endpoint carries a **connect** timeout, which bounds establishing the
//! TCP/TLS connection and nothing else. No per-call deadline is set, for two
//! reasons that pull the same way:
//!
//! - `Lock` waits **server-side** for up to the caller's `timeout_ms` (§6.5), so
//!   any client deadline shorter than that would sever an acquisition the server
//!   was about to grant;
//! - a watch is long-lived and must carry no RPC timeout at all (§6.10).
//!
//! A default unary deadline belongs with the policy stack §12.9 sketches, which
//! is #4084's to supply and is not wired here yet.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tonic::transport::{Channel, Endpoint};

use crate::cache::ClusterCacheBackend;
use crate::client::ClusterClient;
use crate::client::backends::{RemoteCacheBackend, RemoteLeaderElectionBackend, RemoteLockBackend};
use crate::descriptors::DescriptorCache;
use crate::dto::{DescribeProfilesRequest, DescribeProfilesResponse, ProfileDescriptor};
use crate::error::ClusterError;
use crate::grpc::stubs;
use crate::intern::intern;
use crate::leader::LeaderElectionBackend;
use crate::lock::DistributedLockBackend;

/// How long a connection attempt may take before it is abandoned.
///
/// A *connection* bound, not a request deadline — see the [module docs](self).
/// It exists so a wedged endpoint fails an RPC promptly instead of hanging it,
/// and it is generous enough that a cold DNS lookup plus TLS handshake inside a
/// cluster fits comfortably.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The generated cache stub, over the one process channel.
pub(crate) type CacheStub = stubs::cache::cluster_cache_api_client::ClusterCacheApiClient<Channel>;
/// The generated lock stub.
pub(crate) type LockStub =
    stubs::lock::distributed_lock_api_client::DistributedLockApiClient<Channel>;
/// The generated leader-election stub.
pub(crate) type LeaderStub =
    stubs::leader::leader_election_api_client::LeaderElectionApiClient<Channel>;
/// The generated profile stub.
type ProfileStub = stubs::profile::cluster_profile_api_client::ClusterProfileApiClient<Channel>;

/// [`ClusterClient`] over a gRPC channel to the deployed cluster gear (§12.9).
///
/// Registered under `dyn ClusterClient` by cluster-sdk's `ConsumerRegistration`
/// (item `K3`) — unless a local implementation is already there, in which case
/// local wins and no channel is ever built (§4.9.3).
#[derive(Debug, Clone)]
pub struct RemoteClusterClient {
    channel: Channel,
    /// Shared with every backend handle this client produces, so one
    /// `DescribeProfiles` serves all three primitives of every profile (§5.5).
    descriptors: Arc<DescriptorCache>,
}

impl RemoteClusterClient {
    /// Builds a client against `endpoint` **without connecting** (invariant I6).
    ///
    /// `endpoint` is an origin such as `http://cluster.platform.svc.cluster.local:9090`;
    /// deriving it is `K3`'s job, not this type's (§4.5, §4.9.2 — cluster owns no
    /// endpoint configuration key, invariant I9).
    ///
    /// # Errors
    /// [`ClusterError::InvalidConfig`] if `endpoint` is not a usable URI. That is
    /// the only failure mode there is: everything after parsing is lazy.
    pub fn connect_lazy(endpoint: &str) -> Result<Self, ClusterError> {
        let channel = Endpoint::from_shared(endpoint.to_owned())
            .map_err(|err| ClusterError::InvalidConfig {
                reason: format!("cluster endpoint `{endpoint}` is not a valid URI: {err}"),
            })?
            .connect_timeout(CONNECT_TIMEOUT)
            .connect_lazy();
        Ok(Self {
            channel,
            descriptors: Arc::new(DescriptorCache::new()),
        })
    }

    /// The cache stub. Cloning the channel is a refcount, not a connection.
    pub(crate) fn cache_stub(&self) -> CacheStub {
        CacheStub::new(self.channel.clone())
    }

    /// The lock stub.
    pub(crate) fn lock_stub(&self) -> LockStub {
        LockStub::new(self.channel.clone())
    }

    /// The leader-election stub.
    pub(crate) fn leader_stub(&self) -> LeaderStub {
        LeaderStub::new(self.channel.clone())
    }

    /// The profile stub, which only [`descriptor`](Self::descriptor) uses.
    fn profile_stub(&self) -> ProfileStub {
        ProfileStub::new(self.channel.clone())
    }

    /// Fetches the whole bound-profile set and refreshes the cache (§5.5).
    ///
    /// The **whole** set, never the one profile asked for: a client resolving one
    /// profile almost always resolves its siblings too, the response is a handful
    /// of small messages, and populating wholesale is what lets the cache drop a
    /// profile the server no longer binds (§5.6 phase C).
    ///
    /// # Errors
    /// Whatever the RPC reports, decoded through the one codec.
    async fn fetch_all_descriptors(&self) -> Result<(), ClusterError> {
        let request = stubs::profile::DescribeProfilesRequest::from(DescribeProfilesRequest {
            profiles: Vec::new(),
        });
        let response = self
            .profile_stub()
            .describe_profiles(request)
            .await
            .map_err(|status| crate::convert::from_status(&status))?;
        let described = decode::<DescribeProfilesResponse, _>(response.into_inner())?;
        self.descriptors
            .populate(described.generation, described.profiles);
        Ok(())
    }
}

/// Decodes a proto response into its DTO, fallibly.
///
/// The fallible decode rather than the infallible `From`: the latter panics on a
/// malformed `via_string` field, which would let a peer take a consumer's process
/// down with one bad response. The generated client makes the same choice for the
/// same reason.
pub(crate) fn decode<D, P>(proto: P) -> Result<D, ClusterError>
where
    D: toolkit_contract::grpc_repr::TryFromProto<P>,
{
    D::try_from_proto_wire(proto).map_err(|err| ClusterError::Provider {
        kind: crate::error::ProviderErrorKind::Other,
        message: format!("cluster returned an undecodable response: {err}"),
    })
}

#[async_trait]
impl ClusterClient for RemoteClusterClient {
    /// Sync and pure: an `Arc` clone, a stub clone and an interned name. Nothing
    /// is validated here and nothing is fetched — a profile the server does not
    /// bind produces a handle whose first call reports `ProfileNotBound`, which is
    /// the same answer a bound-then-removed profile gives (§5.6).
    fn cache_backend(&self, profile: &str) -> Result<Arc<dyn ClusterCacheBackend>, ClusterError> {
        Ok(Arc::new(RemoteCacheBackend::new(
            self.cache_stub(),
            profile,
            Arc::clone(&self.descriptors),
        )))
    }

    fn lock_backend(&self, profile: &str) -> Result<Arc<dyn DistributedLockBackend>, ClusterError> {
        Ok(Arc::new(RemoteLockBackend::new(
            self.lock_stub(),
            profile,
            Arc::clone(&self.descriptors),
        )))
    }

    fn leader_election_backend(
        &self,
        profile: &str,
    ) -> Result<Arc<dyn LeaderElectionBackend>, ClusterError> {
        Ok(Arc::new(RemoteLeaderElectionBackend::new(
            self.leader_stub(),
            profile,
            Arc::clone(&self.descriptors),
        )))
    }

    /// Re-reads the whole bound set, replacing the cache **only** once the answer
    /// is in hand.
    ///
    /// The readiness contributor's only lever on a cache that is otherwise never
    /// re-read (see the trait's docs), and it bypasses `descriptor()`'s cache
    /// short-circuit by calling the fetch directly — which is the entirety of what
    /// this method has to do. It must not empty the cache first: the sync
    /// accessors on every live handle read it, so a cleared cache makes
    /// `consistency()`, `features()` and `provider_name()` answer with the
    /// fail-safe reading of a profile that is working perfectly well. ADR-011
    /// accepts that answer only before the first descriptor lands, where no
    /// consumer respecting `/readyz` can observe it; a poll on a pod already in
    /// rotation is exactly where it can. `populate` replaces the set wholesale, so
    /// a successful fetch needs no clearing and a failed one must leave the last
    /// good answers standing.
    ///
    /// # Errors
    /// [`ClusterError::Provider`] when the fetch fails. The cache is untouched in
    /// that case.
    async fn refresh_descriptors(&self) -> Result<(), ClusterError> {
        self.fetch_all_descriptors().await
    }

    /// The profile's descriptor, from the cache or from one `DescribeProfiles`.
    ///
    /// The sole `async` member of the trait and the only thing `resolve()` awaits
    /// — on a bounded timeout, never on cluster becoming reachable (§4.7.1,
    /// invariant I6). The bound is `K4`'s to apply; this method's obligation is
    /// to make at most one round trip and to be cheap thereafter.
    ///
    /// A cache miss after a successful fetch is [`ClusterError::ProfileNotBound`]:
    /// the server answered with its whole bound set and this profile was not in
    /// it. That is the same verdict the local client gives for the same reason,
    /// and it needs no new variant (invariant I3).
    ///
    /// # Errors
    /// [`ClusterError::ProfileNotBound`] when the server does not bind `profile`,
    /// or [`ClusterError::Provider`] when the fetch fails.
    async fn descriptor(&self, profile: &str) -> Result<ProfileDescriptor, ClusterError> {
        if let Some(cached) = self.descriptors.get(profile) {
            return Ok(cached);
        }
        self.fetch_all_descriptors().await?;
        self.descriptors
            .get(profile)
            .ok_or_else(|| ClusterError::ProfileNotBound {
                profile: intern(profile),
            })
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod remote_tests;
