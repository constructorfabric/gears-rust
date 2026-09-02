//! [`RemoteClusterClient`] — Profile 3's half of the process seam
//! (DESIGN.md).
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
//!
//! What the endpoint *does* now carry is **HTTP/2 keepalive** (`connect_lazy`
//! below): a channel-level liveness probe, not a per-call deadline. It lets the
//! transport discover a half-open connection and fail its RPCs promptly instead
//! of hanging them, which matters most for the renewal pump — but it is defence
//! in depth, because the pump bounds each `renew`/`join_once` itself and holds
//! leadership only to the lease deadline regardless (B6). Keepalive is
//! configured here rather than in `toolkit-transport-grpc`, which cluster-sdk
//! does not modify.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};
use toolkit_contract::runtime::config::InternalTokenProvider;
use toolkit_transport_grpc::InternalAuthInterceptor;

use crate::cache::ClusterCacheBackend;
use crate::client::ClusterClient;
use crate::client::backends::{RemoteCacheBackend, RemoteLeaderElectionBackend, RemoteLockBackend};
use crate::descriptors::DescriptorCache;
use crate::dto::{DescribeProfilesRequest, DescribeProfilesResponse, ProfileDescriptor};
use crate::error::ClusterError;
use crate::grpc::stubs;
use crate::intern::intern_existing;
use crate::leader::LeaderElectionBackend;
use crate::lock::DistributedLockBackend;

/// How long a connection attempt may take before it is abandoned.
///
/// A *connection* bound, not a request deadline — see the [module docs](self).
/// It exists so a wedged endpoint fails an RPC promptly instead of hanging it,
/// and it is generous enough that a cold DNS lookup plus TLS handshake inside a
/// cluster fits comfortably.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// HTTP/2 keepalive PING interval and its per-PING answer deadline, plus
/// keepalive while the connection is idle.
///
/// Defence in depth for B6, not the load-bearing part of it: the renewal pump
/// already bounds every `renew`/`join_once` with a per-RPC timeout and takes the
/// claim at its deadline regardless, so leadership validity never depends on the
/// transport noticing a dead peer. Keepalive is what lets the *transport* notice
/// a half-open connection promptly — a wedged NAT or a silently-dropped peer —
/// and fail in-flight and future RPCs rather than hanging them until the kernel
/// gives up, which shortens recovery for every call on the channel, watches
/// included. Configured here on the endpoint (cluster-sdk owns this seam);
/// `toolkit-transport-grpc`'s endpoint builder is deliberately not touched.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(5);

/// The one process channel, wrapped so every outbound call passes through the
/// platform-plane credential interceptor.
///
/// The interceptor is applied *unconditionally* — `InternalAuthInterceptor::disabled()`
/// when there is no credential — so the stub types stay uniform. A `Option<...>` in
/// the type here would fork every stub alias, and with it every backend signature,
/// to express something the interceptor already expresses as a value.
pub(crate) type AuthChannel = InterceptedService<Channel, InternalAuthInterceptor>;

/// The generated cache stub, over the one process channel.
pub(crate) type CacheStub =
    stubs::cache::cluster_cache_api_client::ClusterCacheApiClient<AuthChannel>;
/// The generated lock stub.
pub(crate) type LockStub =
    stubs::lock::distributed_lock_api_client::DistributedLockApiClient<AuthChannel>;
/// The generated leader-election stub.
pub(crate) type LeaderStub =
    stubs::leader::leader_election_api_client::LeaderElectionApiClient<AuthChannel>;
/// The generated profile stub.
type ProfileStub = stubs::profile::cluster_profile_api_client::ClusterProfileApiClient<AuthChannel>;

/// Bridge the runtime's credential source to the transport's interceptor.
///
/// The attach *policy* is deliberately not reimplemented here:
/// [`InternalTokenProvider::resolve_for_attach`] is the shared decision the REST
/// and gRPC helpers both delegate to, so cluster behaves identically to every
/// generated client —
///
/// - `None` / `NotConfigured` → attach nothing, silently. A legitimate Profile 1
///   deployment, not a fault.
/// - `Available` → attach the token.
/// - `Unavailable` → attach nothing but **warn**. A broken credential source is
///   not the same thing as an intentional opt-out, and failing the call instead
///   would take the whole fleet down when a token file goes briefly empty.
///
/// The provider is invoked per request rather than captured once, so a rotating
/// (projected service-account) token is picked up without rebuilding the channel.
fn interceptor_for(provider: Option<&InternalTokenProvider>) -> InternalAuthInterceptor {
    match provider {
        Some(provider) => {
            let provider = provider.clone();
            InternalAuthInterceptor::new(move || {
                InternalTokenProvider::resolve_for_attach(Some(&provider), RPC_LABEL)
            })
        }
        None => InternalAuthInterceptor::disabled(),
    }
}

/// Names this client in the `Unavailable` warning `resolve_for_attach` emits. The
/// interceptor runs before the method is known, so there is no per-RPC label.
const RPC_LABEL: &str = "cluster";

/// The placeholder an unbound profile name resolves to when it was never interned
/// from the process's own configured set. A caller looping over made-up profile
/// names must not grow the leaked intern table (invariant I15), so the name is
/// looked up rather than promoted — see [`intern_existing`](crate::intern::intern_existing).
const UNKNOWN_PROFILE: &str = "<unknown>";

/// [`ClusterClient`] over a gRPC channel to the deployed cluster gear (§12.9).
///
/// Registered under `dyn ClusterClient` by cluster-sdk's `ConsumerRegistration`
/// (item `K3`) — unless a local implementation is already there, in which case
/// local wins and no channel is ever built (§4.9.3).
#[derive(Clone)]
pub struct RemoteClusterClient {
    channel: Channel,
    /// The process's platform-plane credential source, as an interceptor. Held
    /// here rather than wrapped around `channel` once, because each stub is built
    /// from its own channel clone.
    interceptor: InternalAuthInterceptor,
    /// Shared with every backend handle this client produces, so one
    /// `DescribeProfiles` serves all three primitives of every profile (§5.5).
    descriptors: Arc<DescriptorCache>,
}

/// Hand-written because [`InternalAuthInterceptor`] is deliberately not `Debug`:
/// it closes over the credential source, and deriving would put a type holding a
/// secret one careless `{:?}` away from a log line. The field is reported as
/// present, never as a value.
impl std::fmt::Debug for RemoteClusterClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteClusterClient")
            .field("channel", &self.channel)
            .field("interceptor", &"<platform-plane credential interceptor>")
            .field("descriptors", &self.descriptors)
            .finish()
    }
}

impl RemoteClusterClient {
    /// Builds a client against `endpoint` **without connecting** (invariant I6).
    ///
    /// `endpoint` is an origin such as `http://cluster.platform.svc.cluster.local:9090`;
    /// deriving it is `K3`'s job, not this type's (§4.5, §4.9.2 — cluster owns no
    /// endpoint configuration key, invariant I9).
    ///
    /// `internal_token_provider` is the process's platform-plane credential source
    /// (`cpt-cf-adr-two-plane-auth`), threaded in by the runtime through
    /// `wiring::wire`. Every method on the cluster contract is platform-plane, so
    /// the credential is attached to *every* outbound call, by an interceptor on
    /// the channel: no call site can forget it, and streaming RPCs are covered like
    /// unary ones. `None` (Profile 1 / in-process, or no credential configured)
    /// attaches nothing.
    ///
    /// # Errors
    /// [`ClusterError::InvalidConfig`] if `endpoint` is not a usable URI. That is
    /// the only failure mode there is: everything after parsing is lazy.
    pub fn connect_lazy(
        endpoint: &str,
        internal_token_provider: Option<&InternalTokenProvider>,
    ) -> Result<Self, ClusterError> {
        let channel = Endpoint::from_shared(endpoint.to_owned())
            .map_err(|err| ClusterError::InvalidConfig {
                reason: format!("cluster endpoint `{endpoint}` is not a valid URI: {err}"),
            })?
            .connect_timeout(CONNECT_TIMEOUT)
            .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
            .keep_alive_timeout(KEEPALIVE_TIMEOUT)
            .keep_alive_while_idle(true)
            .connect_lazy();
        Ok(Self {
            channel,
            interceptor: interceptor_for(internal_token_provider),
            descriptors: Arc::new(DescriptorCache::new()),
        })
    }

    /// The intercepted channel each stub is built over. Cloning the channel is a
    /// refcount, not a connection; cloning the interceptor is two more.
    fn auth_channel(&self) -> AuthChannel {
        InterceptedService::new(self.channel.clone(), self.interceptor.clone())
    }

    /// The cache stub.
    pub(crate) fn cache_stub(&self) -> CacheStub {
        CacheStub::new(self.auth_channel())
    }

    /// The lock stub.
    pub(crate) fn lock_stub(&self) -> LockStub {
        LockStub::new(self.auth_channel())
    }

    /// The leader-election stub.
    pub(crate) fn leader_stub(&self) -> LeaderStub {
        LeaderStub::new(self.auth_channel())
    }

    /// The profile stub, which only [`descriptor`](Self::descriptor) uses.
    fn profile_stub(&self) -> ProfileStub {
        ProfileStub::new(self.auth_channel())
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
/// The fallible decode is the only `Proto -> Rust` path a `via_string`-bearing
/// DTO has: a malformed wire string would otherwise let a peer take a consumer's
/// process down with one bad response, so `#[derive(ProtoBridge)]` emits no
/// infallible `From<Proto>` for such a type. The generated client makes the same
/// choice for the same reason.
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
                // Wire-facing: look the requested name up, never promote it.
                profile: intern_existing(profile).unwrap_or(UNKNOWN_PROFILE),
            })
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod remote_tests;
