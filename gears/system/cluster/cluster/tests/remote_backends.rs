// Created: 2026-08-13 by Constructor Tech
//! The three remote backend handles against a **real** cluster gear
//! (item `K2`, DESIGN-DEPLOYABLE-GEAR §12.9-12.12).
//!
//! This file lives in the gear crate rather than beside the backends themselves,
//! and it has to: only this crate can serve the four services, because the
//! service impls are the gear's. What it buys is the assertion that matters most
//! for the whole deployable model — **the same trait, the same behaviour, both
//! sides of a socket**. Every test here drives an `Arc<dyn _Backend>` obtained
//! from a `RemoteClusterClient`, which is exactly what a consumer's `resolve()`
//! will hand it once `K4` lands, and several of them assert the remote answer
//! against the *local* backend's answer for the same operation.
//!
//! The standalone plugin backs the server, to stay hermetic (§7.6).

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
use cluster_sdk::cache::{CacheWatchEvent, PutRequest, Ttl};
use cluster_sdk::grpc::stubs;
use cluster_sdk::leader::{LeaderStatus, LeaderWatchEvent};
use cluster_sdk::{CacheConsistency, ClusterClient, ClusterError, RemoteClusterClient};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use toolkit::client_hub::ClientHub;

/// The profile every test addresses.
const PROFILE: &str = "orders";

/// How long a stream assertion waits before declaring the event lost.
///
/// A watch that never delivers hangs the test binary rather than failing it, so
/// every `recv` in this file is wrapped. Generous enough not to flake on a loaded
/// machine, short enough to fail inside the harness timeout.
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

/// A running cluster gear plus a lazy client pointed at it.
struct Fixture {
    client: RemoteClusterClient,
    /// The registry the server dispatches through, so a test can compare the
    /// remote answer with the local backend's own.
    registry: Arc<ProfileRegistry>,
    /// The server's subscription table, so a test can watch it grow (and, once
    /// the sweep runs, watch it stay bounded) - item `S2`.
    subscriptions: Arc<ElectionSubscriptions>,
    handle: ClusterHandle,
    shutdown: tokio::sync::oneshot::Sender<()>,
}

impl Fixture {
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

        // Lazy, so this line proves the client needs no reachable server to
        // build even though one happens to be up (invariant I6).
        let client =
            RemoteClusterClient::connect_lazy(&format!("http://{addr}")).expect("a valid endpoint");

        Self {
            client,
            registry,
            subscriptions,
            handle,
            shutdown,
        }
    }

    /// The remote cache handle, as a consumer would hold it.
    fn cache(&self) -> Arc<dyn cluster_sdk::ClusterCacheBackend> {
        self.client.cache_backend(PROFILE).expect("a handle")
    }

    fn lock(&self) -> Arc<dyn cluster_sdk::DistributedLockBackend> {
        self.client.lock_backend(PROFILE).expect("a handle")
    }

    fn leader(&self) -> Arc<dyn cluster_sdk::LeaderElectionBackend> {
        self.client
            .leader_election_backend(PROFILE)
            .expect("a handle")
    }

    /// The server-side backend for the same profile, for the comparisons that
    /// make "the same trait, both sides of the socket" checkable rather than
    /// asserted.
    fn local_cache(&self) -> Arc<dyn cluster_sdk::ClusterCacheBackend> {
        Arc::clone(&self.registry.resolve(PROFILE).expect("published").cache)
    }

    async fn stop(self) {
        let _sent = self.shutdown.send(());
        self.handle.stop().await;
    }
}

/// Awaits one watch event, failing rather than hanging.
async fn next_cache_event(watch: &mut cluster_sdk::CacheWatch) -> CacheWatchEvent {
    tokio::time::timeout(EVENT_TIMEOUT, watch.recv())
        .await
        .expect("a watch event must arrive inside the timeout")
        .expect("the watch must not close")
}

/// Awaits one election event, failing rather than hanging.
async fn next_leader_event(watch: &mut cluster_sdk::LeaderWatch) -> LeaderWatchEvent {
    tokio::time::timeout(EVENT_TIMEOUT, watch.changed())
        .await
        .expect("an election event must arrive inside the timeout")
}

// ---------------------------------------------------------------------------
// The descriptor, and what it makes the synchronous accessors answer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_descriptor_makes_the_sync_accessors_answer_like_the_real_backend() {
    // §5.5's whole purpose: `consistency()`, `features()` and `provider_name()`
    // are synchronous on a trait plugins implement, and a remote handle answers
    // them out of one `DescribeProfiles` rather than not at all.
    let fixture = Fixture::start().await;
    let cache = fixture.cache();
    let local = fixture.local_cache();

    // Before the fetch the handle fails safe - the weaker reading in every case.
    assert_eq!(cache.consistency(), CacheConsistency::EventuallyConsistent);
    assert_eq!(cache.provider_name(), "unknown");

    let descriptor = fixture
        .client
        .descriptor(PROFILE)
        .await
        .expect("the profile is bound");
    assert_eq!(descriptor.name, PROFILE);

    // ...and afterwards it agrees with the backend on the other side.
    assert_eq!(
        cache.consistency(),
        local.consistency(),
        "the remote handle must declare what the real backend declares"
    );
    assert_eq!(cache.features().prefix_watch, local.features().prefix_watch);
    assert_eq!(
        cache.provider_name(),
        "standalone",
        "the *server-side* provider, not the remote handle's own type"
    );

    fixture.stop().await;
}

#[tokio::test]
async fn an_unbound_profile_is_profile_not_bound_from_the_descriptor() {
    let fixture = Fixture::start().await;

    let err = fixture
        .client
        .descriptor("nowhere")
        .await
        .expect_err("the server binds no such profile");
    assert!(
        matches!(err, ClusterError::ProfileNotBound { .. }),
        "expected ProfileNotBound, got: {err}"
    );

    fixture.stop().await;
}

#[tokio::test]
async fn a_call_against_an_unbound_profile_reports_profile_not_bound() {
    // The factory succeeded for a profile the server does not bind; the *call* is
    // where that is reported, and it comes back as the frozen model's existing
    // variant (invariant I3).
    let fixture = Fixture::start().await;
    let cache = fixture.client.cache_backend("nowhere").expect("a handle");

    let err = cache.get("k").await.expect_err("no such profile");
    assert!(
        matches!(err, ClusterError::ProfileNotBound { .. }),
        "expected ProfileNotBound, got: {err}"
    );

    fixture.stop().await;
}

// ---------------------------------------------------------------------------
// The cache
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_cache_round_trips_every_unary_operation() {
    let fixture = Fixture::start().await;
    let cache = fixture.cache();

    assert!(cache.get("ledger").await.expect("get").is_none());
    assert!(!cache.contains("ledger").await.expect("contains"));

    cache
        .put(PutRequest {
            key: "ledger",
            value: b"41",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");

    let entry = cache
        .get("ledger")
        .await
        .expect("get")
        .expect("just written");
    assert_eq!(entry.value, b"41");
    assert!(cache.contains("ledger").await.expect("contains"));

    // The write is visible to the *server's own* backend: the wire moved it, not
    // a client-side cache.
    let local = fixture.local_cache();
    assert_eq!(
        local
            .get("ledger")
            .await
            .expect("get")
            .expect("present")
            .value,
        b"41"
    );

    let swapped = cache
        .compare_and_swap("ledger", entry.version, b"42", Ttl::Indefinite)
        .await
        .expect("cas on the current version");
    assert_eq!(swapped.value, b"42");

    // A stale version is a typed `CasConflict`, reconstructed through the trailer
    // rather than inferred from the gRPC code (§6.9).
    let conflict = cache
        .compare_and_swap("ledger", entry.version, b"43", Ttl::Indefinite)
        .await
        .expect_err("the version moved");
    assert!(
        matches!(conflict, ClusterError::CasConflict { ref key, .. } if key == "ledger"),
        "expected CasConflict, got: {conflict}"
    );

    // `put_if_absent` on a present key creates nothing.
    assert!(
        cache
            .put_if_absent(PutRequest {
                key: "ledger",
                value: b"99",
                ttl: Ttl::Indefinite,
            })
            .await
            .expect("put_if_absent")
            .is_none()
    );

    // A value-guarded delete against the wrong value is a no-op, not an error.
    assert!(
        !cache
            .compare_and_delete("ledger", b"wrong")
            .await
            .expect("cad")
    );
    assert!(
        cache
            .compare_and_delete("ledger", b"42")
            .await
            .expect("cad")
    );
    assert!(!cache.contains("ledger").await.expect("contains"));

    assert!(!cache.delete("ledger").await.expect("delete an absent key"));

    fixture.stop().await;
}

#[tokio::test]
async fn scan_prefix_reassembles_every_page() {
    // The wire is paginated and the trait is not (§6.4). The server's default page
    // size is 256, so 300 keys is the smallest count that proves the loop rather
    // than the first page.
    let fixture = Fixture::start().await;
    let cache = fixture.cache();

    for index in 0..300 {
        cache
            .put(PutRequest {
                key: &format!("orders/{index:04}"),
                value: b"x",
                ttl: Ttl::Indefinite,
            })
            .await
            .expect("put");
    }

    let mut keys = cache.scan_prefix("orders/").await.expect("scan");
    keys.sort();
    assert_eq!(keys.len(), 300, "every page must be reassembled");
    assert_eq!(keys.first().map(String::as_str), Some("orders/0000"));
    assert_eq!(keys.last().map(String::as_str), Some("orders/0299"));

    fixture.stop().await;
}

#[tokio::test]
async fn a_cache_watch_delivers_the_servers_events() {
    let fixture = Fixture::start().await;
    let cache = fixture.cache();

    let mut watch = cache.watch("ledger").await.expect("the watch opens");

    cache
        .put(PutRequest {
            key: "ledger",
            value: b"1",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");

    let event = next_cache_event(&mut watch).await;
    assert!(
        matches!(
            event,
            CacheWatchEvent::Event(cluster_sdk::CacheEvent::Changed { ref key }) if key == "ledger"
        ),
        "expected Changed(ledger), got: {event:?}"
    );

    cache.delete("ledger").await.expect("delete");
    let event = next_cache_event(&mut watch).await;
    assert!(
        matches!(
            event,
            CacheWatchEvent::Event(cluster_sdk::CacheEvent::Deleted { ref key }) if key == "ledger"
        ),
        "expected Deleted(ledger), got: {event:?}"
    );

    fixture.stop().await;
}

// ---------------------------------------------------------------------------
// The lock
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_lock_guard_renews_and_releases_over_the_wire() {
    // The guard's fields are private, so the token lives in the pump's closure
    // (§12.11). Renewing and releasing through the guard is what proves the pump
    // is actually holding it.
    let fixture = Fixture::start().await;
    let lock = fixture.lock();

    let guard = lock
        .try_lock("ledger", Duration::from_secs(30))
        .await
        .expect("the lock is free");
    assert_eq!(guard.name(), "ledger");

    // Held: a second acquisition is refused with the typed contention error.
    let contended = lock
        .try_lock("ledger", Duration::from_secs(30))
        .await
        .expect_err("the lock is held");
    assert!(
        matches!(contended, ClusterError::LockContended { ref name } if name == "ledger"),
        "expected LockContended, got: {contended}"
    );

    guard
        .renew(Duration::from_mins(1))
        .await
        .expect("the holder can renew");
    guard.release().await.expect("the holder can release");

    // Released: the next acquisition succeeds.
    let next = lock
        .try_lock("ledger", Duration::from_secs(30))
        .await
        .expect("the lock is free again");
    next.release().await.expect("release");

    fixture.stop().await;
}

#[tokio::test]
async fn a_lock_release_is_idempotent_by_absence() {
    // §6.10: a token matching nothing has already achieved what its caller
    // wanted, so the release is `Ok` — which is also what makes a token
    // unprobeable, since both answers are the same `Ok` (§5.8.1).
    let fixture = Fixture::start().await;
    let lock = fixture.lock();

    let token = lock
        .acquire(
            "ledger",
            "ignored-the-server-mints-it",
            Duration::from_secs(30),
        )
        .await
        .expect("acquired");
    lock.release(&token).await.expect("the first release");
    lock.release(&token)
        .await
        .expect("and the second, against nothing");

    // A renewal of the same gone lease is *not* `Ok`: the caller has to learn it
    // lost the lease, which is the one place idempotency stops at the wire.
    let err = lock
        .renew(&token, Duration::from_secs(30))
        .await
        .expect_err("the lease is gone");
    assert!(
        matches!(err, ClusterError::LockExpired { .. }),
        "expected LockExpired, got: {err}"
    );

    fixture.stop().await;
}

#[tokio::test]
async fn a_blocking_lock_times_out_with_the_wait_the_server_measured() {
    // The server does the waiting (§6.5), and `waited` is populated server-side
    // because the server is what did it (§6.9).
    let fixture = Fixture::start().await;
    let lock = fixture.lock();

    let held = lock
        .try_lock("ledger", Duration::from_secs(30))
        .await
        .expect("acquired");

    let err = lock
        .lock(
            "ledger",
            Duration::from_secs(30),
            Duration::from_millis(200),
        )
        .await
        .expect_err("the lock is held for longer than the timeout");
    assert!(
        matches!(err, ClusterError::LockTimeout { ref name, .. } if name == "ledger"),
        "expected LockTimeout, got: {err}"
    );

    held.release().await.expect("release");
    fixture.stop().await;
}

// ---------------------------------------------------------------------------
// Leader election
// ---------------------------------------------------------------------------

#[tokio::test]
async fn electing_reports_leadership_and_resigning_gives_it_back() {
    let fixture = Fixture::start().await;
    let leader = fixture.leader();

    let mut watch = leader
        .elect("primary")
        .await
        .expect("the election is joined");

    let event = next_leader_event(&mut watch).await;
    assert!(
        matches!(event, LeaderWatchEvent::Status(LeaderStatus::Leader)),
        "the sole candidate must be told it leads, got: {event:?}"
    );
    assert!(watch.is_leader(), "and the cached snapshot must agree");

    watch.resign().await.expect("the leader can step down");

    // The claim is back: a fresh election wins it.
    let mut next = leader.elect("primary").await.expect("re-elected");
    let event = next_leader_event(&mut next).await;
    assert!(
        matches!(event, LeaderWatchEvent::Status(LeaderStatus::Leader)),
        "the resigned claim must be available again, got: {event:?}"
    );

    fixture.stop().await;
}

#[tokio::test]
async fn a_second_candidate_follows_rather_than_failing() {
    // Losing an election is an ordinary outcome, not an error (§6.6). A follower
    // must read `initial_status` and never the token's shape — it receives the
    // zero token, because `LeaderJoined.token` is not optional on the wire.
    let fixture = Fixture::start().await;
    let leader = fixture.leader();

    let mut first = leader.elect("primary").await.expect("joined");
    let event = next_leader_event(&mut first).await;
    assert!(matches!(
        event,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));

    let mut second = leader.elect("primary").await.expect("joined");
    let event = next_leader_event(&mut second).await;
    assert!(
        matches!(event, LeaderWatchEvent::Status(LeaderStatus::Follower)),
        "the second candidate must follow, got: {event:?}"
    );
    assert!(!second.is_leader());

    fixture.stop().await;
}

#[tokio::test]
async fn the_lease_half_of_the_election_round_trips() {
    // `join`/`renew`/`resign` as the token-keyed operations they are — the half a
    // serving gear uses, and the half that makes a claim survive the replica it
    // was made through (invariant I7).
    let fixture = Fixture::start().await;
    let leader = fixture.leader();
    let config = cluster_sdk::ElectionConfig::default();

    let token = leader
        .join("primary", "ignored-the-server-mints-it", config)
        .await
        .expect("join")
        .expect("the sole candidate wins");

    leader
        .renew(&token, config.ttl())
        .await
        .expect("the holder can renew");

    // A second candidate loses, and says so with `None` rather than an error.
    assert!(
        leader
            .join("primary", "another", config)
            .await
            .expect("join")
            .is_none(),
        "a contended election is Ok(None), never an error"
    );

    leader.resign(&token).await.expect("resign");
    leader
        .resign(&token)
        .await
        .expect("and again, against nothing - absence is Ok");

    fixture.stop().await;
}

#[tokio::test]
async fn dropping_the_watch_releases_the_claim_best_effort() {
    // The Profile 3 mirror of `defaults::leader_tests::
    // dropping_watch_releases_claim_best_effort`, and an invariant I1 assertion:
    // one consumer source file, one observable behaviour. Profile 1 asserted this
    // and Profile 3 never did, which is exactly how the two came to disagree - the
    // remote pump wrote its resign arm as `Some(responder) = resigns.recv()`, and
    // a `select!` branch whose pattern fails is *disabled* rather than taken, so
    // the `None` a dropped watch produces was discarded and the claim was renewed
    // forever.
    //
    // The TTL is far longer than this test and the renewal cadence far shorter
    // (25 s / 249 missed renewals = a 100 ms tick), so a claim merely *lapsing*
    // cannot be what frees the election. Only a pump that stopped and resigned
    // can.
    let fixture = Fixture::start().await;
    let leader = fixture.leader();
    let config = cluster_sdk::ElectionConfig::new(Duration::from_secs(25), 249)
        .expect("a long TTL on a fast cadence");

    let mut watch = leader
        .elect_with_config("primary", config)
        .await
        .expect("the sole candidate leads");
    let event = next_leader_event(&mut watch).await;
    assert!(
        matches!(event, LeaderWatchEvent::Status(LeaderStatus::Leader)),
        "expected leadership before dropping it, got: {event:?}"
    );

    drop(watch);

    // Poll rather than guess: the pump wakes on the closed resign channel and
    // issues one resign RPC, and what is asserted is that this completes in far
    // less than the 25 s the claim would otherwise be held for.
    let freed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(Some(token)) = leader.join("primary", "successor", config).await {
                return token;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    let token = freed.expect(
        "a dropped watch must free the election without waiting out the TTL - the pump \
         has to stop renewing and best-effort resign, as the in-process one does",
    );
    assert_eq!(token.name, "primary");

    fixture.stop().await;
}

// ---------------------------------------------------------------------------
// The follower pump's subscription leak, and the sweep that bounds it (`S2`)
// ---------------------------------------------------------------------------

/// A fast election: `renewal_interval = ttl / (max_missed_renewals + 1)`, so a
/// 300 ms TTL with the default budget of 2 puts the pump on a 100 ms cadence and
/// several intervals fit inside a test.
fn fast_election() -> cluster_sdk::ElectionConfig {
    cluster_sdk::ElectionConfig::new(Duration::from_millis(300), 2).expect("a valid config")
}

/// The pump's cadence for [`fast_election`].
const FAST_RENEWAL: Duration = Duration::from_millis(100);

#[tokio::test]
async fn a_follower_pump_mints_one_subscription_per_renewal_interval() {
    // The measurement item `S2` exists for, reproduced before it is fixed. A
    // follower re-`join`s on the renewal cadence because the server announces no
    // leadership (section 6.6), `join` opens a subscription unconditionally, and
    // the pump keeps its *original* `election_id` - so every re-claim attempt
    // leaves an unattached entry behind and nothing closes it.
    //
    // This is also the mutation check for the whole item: break `join`'s `open`
    // and this stops growing.
    let fixture = Fixture::start().await;
    let leader = fixture.leader();
    let config = fast_election();

    let _held = leader
        .elect_with_config("primary", config)
        .await
        .expect("the first candidate leads");
    let _follows = leader
        .elect_with_config("primary", config)
        .await
        .expect("the second candidate follows");

    // Two `elect`s, two subscriptions, both attached.
    let settled = fixture.subscriptions.len();
    assert_eq!(settled, 2, "one subscription per `elect`, and no more yet");

    tokio::time::sleep(FAST_RENEWAL * 5).await;

    let grown = fixture.subscriptions.len();
    assert!(
        grown >= settled + 3,
        "a steady-state follower must leak one unattached subscription per renewal \
         interval: started at {settled}, after five intervals {grown}"
    );

    fixture.stop().await;
}

#[tokio::test]
async fn the_sweep_bounds_the_follower_pumps_unattached_subscriptions() {
    // The same drive as above with the sweep running, and the assertion inverted:
    // the population stops at the two attached subscriptions plus whatever the
    // grace window has yet to age off, instead of climbing forever (section
    // 5.4.1).
    //
    // The cadence is scaled down from the shipped 5 s so the test finishes; the
    // *ratio* is the shipped one, since the grace window is a multiple of the
    // interval. That is the property under test - the shape of the bound, not
    // the size of the constants.
    let fixture = Fixture::start().await;
    let sweeping = CancellationToken::new();
    let interval = FAST_RENEWAL * 2;
    let _sweep = cluster::api::grpc::spawn_subscription_sweep(
        Arc::clone(&fixture.subscriptions),
        interval,
        cluster::api::grpc::SubscriptionMetrics::global(),
        sweeping.clone(),
    );

    let leader = fixture.leader();
    let config = fast_election();
    let held = leader
        .elect_with_config("primary", config)
        .await
        .expect("the first candidate leads");
    let follows = leader
        .elect_with_config("primary", config)
        .await
        .expect("the second candidate follows");

    // Long enough that the unswept version of this test would be well past
    // twenty leaked subscriptions.
    tokio::time::sleep(FAST_RENEWAL * 25).await;

    // Two attached, plus at most one grace window's worth of not-yet-aged
    // arrivals and the pass they are waiting on.
    let ceiling = 2 + (cluster::api::grpc::SWEEP_GRACE_MULTIPLIER as usize + 1) * 2;
    let bounded = fixture.subscriptions.len();
    assert!(
        bounded <= ceiling,
        "the sweep must hold the table near its live population: {bounded} entries \
         after twenty-five renewal intervals, ceiling {ceiling}"
    );

    // And it bounded rather than broke: both participants still hold their feeds.
    assert!(
        held.is_leader(),
        "the leader kept its claim across the sweep - a subscription is not a lease"
    );
    assert!(!follows.is_leader());

    sweeping.cancel();
    fixture.stop().await;
}
