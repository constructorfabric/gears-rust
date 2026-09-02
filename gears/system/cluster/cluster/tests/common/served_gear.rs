//! One cluster gear, served over a real socket — the fixture every
//! over-the-wire test in this directory needs.
//!
//! [`served_gear`] wires a standalone profile, publishes it into a
//! [`ProfileRegistry`], binds `127.0.0.1:0` and serves the four gRPC services.
//! [`ServedGear::stop`] shuts both halves down; `ClusterHandle` panics if dropped
//! without it.
//!
//! Its own file beside the in-process stub backends in `common`: those are
//! in-memory objects, this is a running server.

#![allow(
    dead_code,
    reason = "one harness, many test binaries: each uses the part it needs and \
              the compiler sees the rest as unused per-target"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a setup failure IS the test failure"
)]

use std::net::SocketAddr;
use std::sync::Arc;

use cluster::api::grpc::{
    CacheService, ClusterProfileService, DistributedLockService, ElectionSubscriptions,
    LeaderElectionService, ServiceContext,
};
use cluster::{ClusterConfig, ClusterHandle, ClusterWiring, ProfileRegistry, ProviderRegistry};
use cluster_sdk::RemoteClusterClient;
use cluster_sdk::grpc::stubs;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use toolkit::client_hub::ClientHub;
use toolkit::contract_support::runtime::config::InternalTokenProvider;
use toolkit_security::DynInternalAuthenticator;
use toolkit_transport_grpc::InternalAuthGrpcLayer;

/// The profile the default config binds, and the one every caller addresses.
pub const PROFILE: &str = "orders";

/// One standalone-backed profile: enough for every primitive, and hermetic —
/// no Docker, no network (§7.6).
const DEFAULT_CONFIG: &str = "profiles:\n  orders:\n    cache: { provider: standalone }\n";

/// Which of the four services the server registers.
///
/// A subset is not an optimisation: a test that asserts what happens when a
/// method is *unimplemented* needs the service genuinely absent, so
/// this is a per-service choice rather than an all-or-nothing switch.
///
/// A set rather than four `bool` fields, so the call sites read as what they
/// select — `Services::LEADER`, `Services::ALL.without(Services::CACHE)` — with
/// no positional booleans to mix up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Services(u8);

impl Services {
    pub const CACHE: Self = Self(1 << 0);
    pub const LOCK: Self = Self(1 << 1);
    pub const LEADER: Self = Self(1 << 2);
    pub const PROFILE: Self = Self(1 << 3);

    /// All four, as a deployed gear serves them.
    pub const ALL: Self = Self(0b1111);
    /// None — the base for `Services::NONE.with(Services::LEADER)`.
    pub const NONE: Self = Self(0);

    /// This set plus `other`.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// This set minus `other`.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Whether every service in `other` is in this set.
    #[must_use]
    const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// Builder for [`ServedGear`]. Start one with [`served_gear`].
pub struct Builder {
    config_yaml: String,
    services: Services,
    /// The platform-plane layer wrapping the served services, mirroring
    /// `grpc-hub`'s `serve_tcp`. Default [`InternalAuthGrpcLayer::disabled`], so
    /// no identity is stamped and every caller is `UNAUTHENTICATED_CALLER` — the
    /// pre-retrofit behaviour every existing test relies on.
    auth_layer: InternalAuthGrpcLayer,
}

/// A running cluster gear on an ephemeral port.
///
/// Call [`stop`](ServedGear::stop) at the end of every test: `ClusterHandle`
/// panics when dropped without it, on purpose.
pub struct ServedGear {
    /// The bound address, for a test that builds its own channel or proxy.
    pub addr: SocketAddr,
    /// `http://{addr}`, ready for `connect_lazy` or `Channel::from_shared`.
    pub endpoint: String,
    /// The registry the server dispatches through, so a test can compare the
    /// remote answer against the local backend's own.
    pub registry: Arc<ProfileRegistry>,
    /// The server's subscription table, so a test can watch it grow — and, with
    /// the sweep running, watch it stay bounded.
    pub subscriptions: Arc<ElectionSubscriptions>,
    client: RemoteClusterClient,
    handle: ClusterHandle,
    shutdown: tokio::sync::oneshot::Sender<()>,
}

/// A gear serving all four services with platform-plane enforcement **disabled**
/// (no identity stamped → every caller is `UNAUTHENTICATED_CALLER`).
#[must_use]
pub fn served_gear() -> Builder {
    Builder {
        config_yaml: DEFAULT_CONFIG.to_owned(),
        services: Services::ALL,
        auth_layer: InternalAuthGrpcLayer::disabled(),
    }
}

impl Builder {
    /// Register only some of the four services.
    #[must_use]
    pub fn services(mut self, services: Services) -> Self {
        self.services = services;
        self
    }

    /// Wrap the served services in an **enforcing** platform-plane layer built on
    /// `authenticator`, mirroring `grpc-hub` with `internal_auth` configured.
    ///
    /// Enforcement is [`InternalAuthEnforcement::Required`](toolkit_transport_grpc::InternalAuthEnforcement::Required)
    /// by default (an RPC with no credential is rejected before the handler); an
    /// authenticator that maps distinct tokens to distinct identity names is what
    /// makes the `owns()` cross-check observable over the wire.
    #[must_use]
    pub fn authenticator(mut self, authenticator: DynInternalAuthenticator) -> Self {
        self.auth_layer = InternalAuthGrpcLayer::new(authenticator);
        self
    }

    /// Override the profile config.
    #[must_use]
    pub fn config_yaml(mut self, yaml: &str) -> Self {
        yaml.clone_into(&mut self.config_yaml);
        self
    }

    /// Wire the gear, bind an ephemeral port and serve.
    pub async fn start(self) -> ServedGear {
        let cfg: ClusterConfig = serde_saphyr::from_str(&self.config_yaml).expect("config parses");
        let providers = ProviderRegistry::new()
            .with_cache_provider(Arc::new(standalone_cluster_plugin::StandaloneCacheProvider));
        let (handle, bound) =
            ClusterWiring::from_config(Arc::new(ClientHub::new()), &cfg, &providers)
                .await
                .expect("wiring starts");

        let registry = Arc::new(ProfileRegistry::new());
        registry.publish(bound);

        let ctx = ServiceContext::new(Arc::clone(&registry));
        let auth_layer = self.auth_layer;
        let subscriptions = Arc::new(ElectionSubscriptions::new());
        let served_subscriptions = Arc::clone(&subscriptions);

        // Port 0, so concurrent test binaries never collide.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
        let addr = listener.local_addr().expect("has an address");
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();

        let services = self.services;
        tokio::spawn(async move {
            // `add_optional_service` rather than a conditional chain: the four
            // arms then have one shape, and an absent service is absent from the
            // routing table exactly as it would be on a gear that never
            // registered it.
            Server::builder()
                // The platform-plane layer wraps every service, exactly as
                // `grpc-hub`'s `serve_tcp` installs `effective_auth_layer`.
                .layer(auth_layer)
                .add_optional_service(services.contains(Services::CACHE).then(|| {
                    stubs::cache::cluster_cache_api_server::ClusterCacheApiServer::new(
                        CacheService::new(ctx.clone()),
                    )
                }))
                .add_optional_service(services.contains(Services::LOCK).then(|| {
                    stubs::lock::distributed_lock_api_server::DistributedLockApiServer::new(
                        DistributedLockService::new(ctx.clone()),
                    )
                }))
                .add_optional_service(services.contains(Services::LEADER).then(|| {
                    stubs::leader::leader_election_api_server::LeaderElectionApiServer::new(
                        LeaderElectionService::new(ctx.clone(), served_subscriptions),
                    )
                }))
                .add_optional_service(services.contains(Services::PROFILE).then(|| {
                    stubs::profile::cluster_profile_api_server::ClusterProfileApiServer::new(
                        ClusterProfileService::new(ctx),
                    )
                }))
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _stopped = shutdown_rx.await;
                })
                .await
                .expect("the server runs");
        });

        let endpoint = format!("http://{addr}");
        // Lazy, so building it proves the client needs no reachable server even
        // though one happens to be up (invariant I6).
        let client = RemoteClusterClient::connect_lazy(&endpoint, None).expect("a valid endpoint");

        ServedGear {
            addr,
            endpoint,
            registry,
            subscriptions,
            client,
            handle,
            shutdown,
        }
    }
}

impl ServedGear {
    /// The shared client, carrying no platform credential.
    ///
    /// One client per fixture on purpose: the descriptor cache lives on it, so a
    /// test that built a fresh client per call would silently re-fetch and stop
    /// testing the cache it meant to exercise.
    #[must_use]
    pub fn client(&self) -> &RemoteClusterClient {
        &self.client
    }

    /// A separate client carrying `provider`, for the tests that assert what
    /// reaches the server.
    #[must_use]
    pub fn client_with(&self, provider: Option<&InternalTokenProvider>) -> RemoteClusterClient {
        RemoteClusterClient::connect_lazy(&self.endpoint, provider).expect("a valid endpoint")
    }

    /// Stop serving, then stop the gear.
    ///
    /// Both halves matter: dropping `ClusterHandle` without `stop()` is a
    /// deliberate panic, and leaving the server task running leaks a port for the
    /// rest of the binary.
    pub async fn stop(self) {
        let _sent = self.shutdown.send(());
        self.handle.stop().await;
    }
}
