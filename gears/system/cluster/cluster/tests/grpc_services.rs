// Created: 2026-08-12 by Constructor Tech
//! The four gRPC services over a **real** tonic server and a real channel
//! (item `S1`, DESIGN-DEPLOYABLE-GEAR §6.1).
//!
//! The unit tests beside each service call the generated `*_server` trait
//! directly, which covers the handlers' logic and nothing about the transport.
//! This file covers what only a socket can show: that all four services are
//! actually routed and served, that the credential travels the way §4.6 says it
//! does, and — the one `S1` exit criterion that cannot be asserted any other way —
//! that a **watch stream carries no RPC timeout**.
//!
//! The standalone plugin backs it, to stay hermetic (§7.6).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

use std::sync::Arc;
use std::time::Duration;

use cluster::api::grpc::{
    CacheService, CallerResolver, ClusterProfileService, DistributedLockService,
    ElectionSubscriptions, LeaderElectionService, ServiceContext,
};
use cluster::{ClusterConfig, ClusterHandle, ClusterWiring, ProfileRegistry, ProviderRegistry};
use cluster_sdk::grpc::stubs;
use tokio::net::TcpListener;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Channel, Server};
use toolkit::client_hub::ClientHub;
use toolkit_transport_grpc::InternalAuthInterceptor;

/// The per-unary-call deadline this test stands in for.
///
/// The deployed value is `GrpcClientConfig::rpc_timeout` (30 s by default), which
/// is far too long to hold a test against. What the number has to be is *some*
/// real RPC timeout, so that a stream held past it demonstrates the property; the
/// magnitude is not what is being asserted.
const RPC_TIMEOUT: Duration = Duration::from_millis(250);

/// A running cluster gear, serving all four services on a real port.
struct Server_ {
    endpoint: String,
    handle: ClusterHandle,
    shutdown: tokio::sync::oneshot::Sender<()>,
    registry: Arc<ProfileRegistry>,
    /// The subscription table, so a test can play item `S5`'s part.
    subscriptions: Arc<ElectionSubscriptions>,
}

impl Server_ {
    async fn start() -> Self {
        let cfg: ClusterConfig =
            serde_saphyr::from_str("profiles:\n  orders:\n    cache: { provider: standalone }\n")
                .expect("config parses");
        let providers = ProviderRegistry::new()
            .with_cache_provider(Arc::new(standalone_cluster_plugin::StandaloneCacheProvider));
        let (handle, bound) =
            ClusterWiring::from_config(Arc::new(ClientHub::new()), &cfg, &providers)
                .await
                .expect("wiring starts");

        let registry = Arc::new(ProfileRegistry::new());
        registry.publish(bound);

        let ctx = ServiceContext::new(Arc::clone(&registry), CallerResolver::trusted_network());
        let subscriptions = Arc::new(ElectionSubscriptions::new());
        let served_subscriptions = Arc::clone(&subscriptions);

        // Port 0, so concurrent test binaries never collide.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
        let addr = listener.local_addr().expect("has an address");
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            Server::builder()
                .add_service(
                    stubs::cache::cluster_cache_api_server::ClusterCacheApiServer::new(
                        CacheService::new(ctx.clone()),
                    ),
                )
                .add_service(
                    stubs::lock::distributed_lock_api_server::DistributedLockApiServer::new(
                        DistributedLockService::new(ctx.clone()),
                    ),
                )
                .add_service(
                    stubs::leader::leader_election_api_server::LeaderElectionApiServer::new(
                        LeaderElectionService::new(ctx.clone(), served_subscriptions),
                    ),
                )
                .add_service(
                    stubs::profile::cluster_profile_api_server::ClusterProfileApiServer::new(
                        ClusterProfileService::new(ctx),
                    ),
                )
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _stopped = shutdown_rx.await;
                })
                .await
                .expect("the server runs");
        });

        Self {
            endpoint: format!("http://{addr}"),
            handle,
            shutdown,
            registry,
            subscriptions,
        }
    }

    /// A channel with the per-call deadline unary calls carry.
    async fn channel_with_rpc_timeout(&self) -> Channel {
        Channel::from_shared(self.endpoint.clone())
            .expect("a valid endpoint")
            .timeout(RPC_TIMEOUT)
            .connect()
            .await
            .expect("connects")
    }

    /// A channel with **no** deadline — the shape a watch must be opened on.
    async fn channel(&self) -> Channel {
        Channel::from_shared(self.endpoint.clone())
            .expect("a valid endpoint")
            .connect()
            .await
            .expect("connects")
    }

    async fn stop(self) {
        let _sent = self.shutdown.send(());
        self.handle.stop().await;
    }
}

/// The outbound half of §4.6's platform plane: the credential rides
/// `x-toolkit-internal-token`, attached at the channel by the shipped
/// interceptor. Never `x-secctx-bin`.
fn credential() -> InternalAuthInterceptor {
    InternalAuthInterceptor::from_token(secrecy::SecretString::from("a-projected-sa-token"))
}

#[tokio::test]
async fn all_four_services_serve() {
    // `S1`'s first exit criterion, over a socket: each of the four is routed,
    // reachable and answers its own contract.
    let server = Server_::start().await;

    let mut cache = stubs::cache::cluster_cache_api_client::ClusterCacheApiClient::with_interceptor(
        server.channel_with_rpc_timeout().await,
        credential(),
    );
    cache
        .put(stubs::cache::PutRequest {
            profile: "orders".to_owned(),
            key: "ledger".to_owned(),
            value: b"41".to_vec(),
            ttl_ms: None,
            client_request_id: None,
        })
        .await
        .expect("cache.Put is served");
    let entry = cache
        .get(stubs::cache::GetRequest {
            profile: "orders".to_owned(),
            key: "ledger".to_owned(),
        })
        .await
        .expect("cache.Get is served")
        .into_inner()
        .entry
        .expect("the key was just written");
    assert_eq!(entry.value, b"41");

    let mut lock =
        stubs::lock::distributed_lock_api_client::DistributedLockApiClient::with_interceptor(
            server.channel_with_rpc_timeout().await,
            credential(),
        );
    let token = lock
        .try_lock(stubs::lock::TryLockRequest {
            profile: "orders".to_owned(),
            name: "ledger".to_owned(),
            ttl_ms: 30_000,
            client_request_id: None,
        })
        .await
        .expect("lock.TryLock is served")
        .into_inner()
        .token
        .expect("an acquisition mints a token");
    lock.release(stubs::lock::LeaseRef {
        profile: "orders".to_owned(),
        token: Some(token),
        ttl_ms: None,
        client_request_id: None,
    })
    .await
    .expect("lock.Release is served");

    let mut leader =
        stubs::leader::leader_election_api_client::LeaderElectionApiClient::with_interceptor(
            server.channel_with_rpc_timeout().await,
            credential(),
        );
    let joined = leader
        .join(stubs::leader::JoinRequest {
            profile: "orders".to_owned(),
            name: "ledger".to_owned(),
            ttl_ms: 30_000,
            max_missed_renewals: None,
            client_request_id: None,
        })
        .await
        .expect("leader.Join is served")
        .into_inner();
    assert_eq!(
        joined.initial_status,
        i32::from(stubs::leader::LeaderStatusDto::Leader)
    );

    let mut profiles =
        stubs::profile::cluster_profile_api_client::ClusterProfileApiClient::with_interceptor(
            server.channel_with_rpc_timeout().await,
            credential(),
        );
    let described = profiles
        .describe_profiles(stubs::profile::DescribeProfilesRequest { profiles: vec![] })
        .await
        .expect("profile.DescribeProfiles is served")
        .into_inner();
    assert_eq!(described.profiles.len(), 1);
    assert_eq!(described.generation, server.registry.generation());

    server.stop().await;
}

#[tokio::test]
async fn an_unknown_profile_returns_the_not_found_mapped_profile_not_bound() {
    // `S1`'s second exit criterion, over a socket, and with the typed variant
    // reconstructed rather than the code merely inspected — because what a
    // consumer branches on is `ClusterError`, not `tonic::Code` (§6.9).
    use cluster_sdk::{ClusterError, LeaseContext, to_cluster_error};
    use toolkit_canonical_errors::Problem;
    use toolkit_transport_grpc::extract_problem;

    let server = Server_::start().await;
    let mut cache = stubs::cache::cluster_cache_api_client::ClusterCacheApiClient::with_interceptor(
        server.channel_with_rpc_timeout().await,
        credential(),
    );

    let status = cache
        .get(stubs::cache::GetRequest {
            profile: "not-a-profile".to_owned(),
            key: "ledger".to_owned(),
        })
        .await
        .expect_err("an unbound profile is refused");

    assert_eq!(status.code(), tonic::Code::NotFound);
    let problem: Problem = extract_problem(status.metadata())
        .expect("the trailer decodes")
        .expect("a cluster status carries the problem trailer");
    let decoded = to_cluster_error(problem, LeaseContext::None).expect("a typed error");
    assert!(
        matches!(decoded, ClusterError::ProfileNotBound { .. }),
        "expected ProfileNotBound, got: {decoded:?}"
    );

    server.stop().await;
}

#[tokio::test]
async fn a_watch_stream_outlives_an_rpc_timeout() {
    // `S1`'s fourth exit criterion, held literally: a watch stream is kept idle
    // well past `rpc_timeout` and must still deliver.
    //
    // It is asserted three times, once per way a deadline can reach the call,
    // because **which one matters was measured rather than assumed** — see the
    // note on `MEASURED` below.
    let server = Server_::start().await;

    let untimed = || async {
        stubs::cache::cluster_cache_api_client::ClusterCacheApiClient::with_interceptor(
            server.channel().await,
            credential(),
        )
    };

    // 1. A channel carrying the same `rpc_timeout` the unary calls use.
    let mut on_timed_channel =
        stubs::cache::cluster_cache_api_client::ClusterCacheApiClient::with_interceptor(
            server.channel_with_rpc_timeout().await,
            credential(),
        );
    let channel_timed = on_timed_channel
        .watch(watch_request())
        .await
        .expect("the watch subscribes")
        .into_inner();

    // 2. An explicit per-call deadline, which is what sets the `grpc-timeout`
    //    header on the wire.
    let mut with_header = untimed().await;
    let mut request = tonic::Request::new(watch_request());
    request.set_timeout(RPC_TIMEOUT);
    let header_timed = with_header
        .watch(request)
        .await
        .expect("the watch subscribes")
        .into_inner();

    // 3. No deadline at all — the shape §6.10 says a watch must be opened with.
    let mut plain = untimed().await;
    let no_deadline = plain
        .watch(watch_request())
        .await
        .expect("the watch subscribes")
        .into_inner();

    // Held idle far past the deadline. Real sleeps, not virtual: this is a
    // property of a socket and a server, and a paused clock would move neither.
    tokio::time::sleep(RPC_TIMEOUT * 4).await;

    let mut writer = untimed().await;
    writer
        .put(stubs::cache::PutRequest {
            profile: "orders".to_owned(),
            key: "ledger".to_owned(),
            value: b"41".to_vec(),
            ttl_ms: None,
            client_request_id: None,
        })
        .await
        .expect("put succeeds");

    for (shape, mut stream) in [
        ("a channel-level rpc_timeout", channel_timed),
        ("an explicit grpc-timeout header", header_timed),
        ("no deadline at all", no_deadline),
    ] {
        let event = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap_or_else(|_| panic!("{shape}: the stream never delivered"))
            .unwrap_or_else(|| panic!("{shape}: the stream was closed"))
            .unwrap_or_else(|status| panic!("{shape}: the stream failed: {status}"));
        assert_eq!(
            event.kind,
            i32::from(stubs::cache::CacheWatchEventKind::Changed),
            "{shape}: a stream held far past rpc_timeout still delivers - the server \
             sets no deadline of its own"
        );
    }

    server.stop().await;
}

/// **MEASURED, and it corrects DESIGN section 6.10.**
///
/// Section 6.10 warns that "getting this wrong — an RPC timeout on a watch stream
/// — would sever every watch on a fixed interval". That hazard does **not**
/// reproduce at the tonic 0.14 layer, and the test above is what establishes it:
/// a stream opened on a channel carrying `.timeout(RPC_TIMEOUT)`, and a stream
/// opened with `Request::set_timeout(RPC_TIMEOUT)`, both stay open and both
/// deliver an event long after the deadline elapses.
///
/// The reason is that a deadline governs the *response future* — which resolves
/// once the response headers arrive — and not the body that follows it, and tonic's
/// server does not enforce the `grpc-timeout` header on a streaming response
/// either. So the rule stands as a rule (a watch must not carry an RPC timeout, and
/// item `K2` must not set one), but it is not load-bearing against tonic today, and
/// a test asserting the *severing* would assert a behaviour that does not exist.
///
/// What is asserted instead is the property `S1` actually owns and can break: **the
/// server imposes no deadline of its own.** If a timeout layer were ever added in
/// front of these services, all three cases above fail.
const _MEASURED: () = ();

/// The watch every case above opens.
fn watch_request() -> stubs::cache::WatchRequest {
    stubs::cache::WatchRequest {
        profile: "orders".to_owned(),
        key: "ledger".to_owned(),
    }
}

#[tokio::test]
async fn a_cancelled_subscription_becomes_reapable_by_the_sweep() {
    // Item `S2` / section 5.4.1: the case decision 18 was written about - a
    // client that goes away without unsubscribing. What makes it detectable is
    // that the server's stream task selects on its outbound channel closing
    // rather than parking on `recv()`. Parked, it would hold the subscription's
    // receiver alive and the entry would read as live forever, so the sweep would
    // bound the follower-pump leak and miss this one entirely.
    //
    // Driven over a raw stream because dropping it is the cancellation: the
    // typed `LeaderWatch` will not do, since its pump keeps renewing (and keeps
    // the stream open) after the watch is dropped.
    let server = Server_::start().await;

    let mut leader =
        stubs::leader::leader_election_api_client::LeaderElectionApiClient::with_interceptor(
            server.channel().await,
            credential(),
        );
    let joined = leader
        .join(stubs::leader::JoinRequest {
            profile: "orders".to_owned(),
            name: "ledger".to_owned(),
            ttl_ms: 30_000,
            max_missed_renewals: None,
            client_request_id: None,
        })
        .await
        .expect("join succeeds")
        .into_inner();

    let stream = leader
        .await_change(stubs::leader::AwaitChangeRequest {
            profile: "orders".to_owned(),
            election_id: joined.election_id,
        })
        .await
        .expect("the subscription opens")
        .into_inner();
    assert_eq!(server.subscriptions.len(), 1);

    // Held, so no window is short enough to reap it.
    assert_eq!(
        server
            .subscriptions
            .sweep(Duration::from_millis(1))
            .reaped_total(),
        0,
        "a subscription its client is holding is never abandoned"
    );

    drop(stream);
    settle().await;

    assert_eq!(
        server
            .subscriptions
            .sweep(Duration::from_millis(1))
            .reaped_total(),
        1,
        "a cancelled stream leaves an entry no reader is holding, which the sweep \
         reaps once its grace window is out"
    );
    assert!(server.subscriptions.is_empty());

    server.stop().await;
}

/// Waits for the server to observe a client-side cancellation.
///
/// The stream reset travels over the loopback socket and wakes a task on the
/// server, and there is no signal the test can await from its own side — so this
/// is a bound rather than a handshake. Two orders of magnitude above a loopback
/// round trip, and the assertion that follows it fails loudly rather than hanging
/// if it is ever not enough.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn an_election_subscription_also_carries_no_rpc_timeout() {
    // The same rule for the other push-shaped operation (§6.10). `await_change`
    // is quieter than a cache watch by nature — an election emits transitions,
    // not mutations — so a subscription that went idle past a deadline and was
    // then severed would be one behaving exactly as designed.
    let server = Server_::start().await;

    let mut leader =
        stubs::leader::leader_election_api_client::LeaderElectionApiClient::with_interceptor(
            server.channel_with_rpc_timeout().await,
            credential(),
        );
    let joined = leader
        .join(stubs::leader::JoinRequest {
            profile: "orders".to_owned(),
            name: "ledger".to_owned(),
            ttl_ms: 30_000,
            max_missed_renewals: None,
            client_request_id: None,
        })
        .await
        .expect("join succeeds")
        .into_inner();

    let mut stream = leader
        .await_change(stubs::leader::AwaitChangeRequest {
            profile: "orders".to_owned(),
            election_id: joined.election_id,
        })
        .await
        .expect("the subscription opens")
        .into_inner();

    tokio::time::sleep(RPC_TIMEOUT * 4).await;

    // The gear drains. Item `S5` owns the ordering; what matters here is that the
    // subscription was still there to receive it after all that idle time.
    server
        .subscriptions
        .broadcast_terminal(&cluster_sdk::leader::LeaderWatchEvent::Closed(
            cluster_sdk::ClusterError::Shutdown,
        ));

    let event = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("the event arrives inside the test's own bound")
        .expect("the subscription is still open")
        .expect("and it is not a transport error");
    assert_eq!(
        event.kind,
        i32::from(stubs::leader::LeaderWatchEventKind::Closed),
        "a subscription held far past rpc_timeout still receives its terminal event"
    );

    server.stop().await;
}
