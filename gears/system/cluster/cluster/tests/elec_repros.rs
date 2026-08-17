// Created: 2026-08-13 by Constructor Tech
//! The election pump's two independent signal paths, over the wire — `ELEC-1`
//! and `SEAM-3` (DESIGN-DEPLOYABLE-GEAR ADR-003, §5.4.1, §6.6, §6.8, §5.8.2).
//!
//! # What these assert, and why they live here
//!
//! A `LeaderWatch` carries two paths that must not be wired together: the
//! *subscription*, which conveys whether events are flowing, and the *renewal
//! task*, which conveys whether the claim is still valid. ADR-003 states it
//! outright — *"A `Closed(ConnectionLost)` on a `LeaderWatch` is a subscription
//! event. State validity is determined by the renewal-task path"* — and §6.6
//! prices it: *"losing it costs a re-subscribe, not a leadership change"*.
//!
//! Profile 1 has always kept them apart (`defaults/leader.rs`, `None =>
//! cache_watch = None`). Profile 3 drove both from one `select!` and let either
//! end the pump, so any rolling restart, replica kill, LB drain or GOAWAY cost
//! every remote leader its claim for a full TTL. That is the "asserted at one
//! level, never at the other" pattern `c937bf72`'s post-mortem named: the
//! server-side half was covered (`api/grpc/leader_tests.rs`, which proves `renew`
//! still works after the subscription is dropped or swept), the end-to-end half
//! was not.
//!
//! These tests are the end-to-end half, and they need this crate because only it
//! can serve the gear's own services — the same reason `remote_backends.rs` lives
//! here.
//!
//! # Timing
//!
//! Every timing assertion is sized so that the *fixed* margin and the *broken*
//! margin are both comfortable; the per-test comments give both. All of them run
//! on a multi-threaded runtime on purpose: on the single-threaded default the
//! contender's poll loop and the pump under test share one thread, and starving
//! the pump would fail these tests for a reason that has nothing to do with what
//! they assert.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::use_debug,
    clippy::err_expect,
    reason = "integration tests: a setup failure IS the test failure, and the \
              measured timings are part of the output these tests exist to show"
)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use cluster::api::grpc::{
    CacheService, CallerResolver, ClusterProfileService, DistributedLockService,
    ElectionSubscriptions, LeaderElectionService, ServiceContext,
};
use cluster::{ClusterConfig, ClusterHandle, ClusterWiring, ProfileRegistry, ProviderRegistry};
use cluster_sdk::dto;
use cluster_sdk::grpc::stubs;
use cluster_sdk::leader::{LeaderStatus, LeaderWatchEvent};
use cluster_sdk::{ClusterClient, RemoteClusterClient};
use tokio::net::{TcpListener, TcpStream};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use toolkit::client_hub::ClientHub;

/// The profile every test addresses.
const PROFILE: &str = "orders";

/// The election these tests hold.
const ELECTION: &str = "primary";

/// The TTL every election here runs on.
///
/// With the default budget of 2 this puts the pump on a
/// `ttl / (max_missed_renewals + 1)` = **500 ms** cadence, so a claim that stops
/// being renewed becomes takeable 1.5 s later and a pump that *is* renewing has
/// to be starved for a full second before it could lose one. That second of slack
/// is what keeps these tests honest on a loaded machine; the older 900 ms/300 ms
/// pairing left only 600 ms and was too tight to run in CI forever.
const TTL: Duration = Duration::from_millis(1500);

/// The pump's renewal cadence for [`TTL`].
const CADENCE: Duration = Duration::from_millis(500);

/// How long a watch assertion waits before declaring an event lost. A watch that
/// never delivers hangs the binary rather than failing it, so every wait is
/// wrapped.
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

fn election_config() -> cluster_sdk::ElectionConfig {
    cluster_sdk::ElectionConfig::new(TTL, 2).expect("a valid config")
}

// ---------------------------------------------------------------------------
// A real gear, and a cuttable relay in front of it
// ---------------------------------------------------------------------------

/// A running cluster gear plus the pieces a test needs to reach around it.
struct Fixture {
    /// Where the gear listens, so a relay can be put in front of it and a
    /// contender can dial it directly at the same time.
    addr: SocketAddr,
    /// The server's subscription table, so a test can reap an entry itself
    /// rather than waiting out §5.4.1's real 15 s grace window.
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
        let served = Arc::clone(&subscriptions);

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
                        LeaderElectionService::new(ctx.clone(), served),
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
            addr,
            subscriptions,
            handle,
            shutdown,
        }
    }

    /// An election handle that dials the gear **directly**, so cutting a relay
    /// cannot disturb it.
    fn leader(&self) -> Arc<dyn cluster_sdk::LeaderElectionBackend> {
        leader_at(self.addr)
    }

    async fn stop(self) {
        let _sent = self.shutdown.send(());
        self.handle.stop().await;
    }
}

fn leader_at(addr: SocketAddr) -> Arc<dyn cluster_sdk::LeaderElectionBackend> {
    RemoteClusterClient::connect_lazy(&format!("http://{addr}"))
        .expect("a valid endpoint")
        .leader_election_backend(PROFILE)
        .expect("a handle")
}

/// A TCP relay whose *live* connections a test can sever while the gear behind
/// it keeps running.
///
/// This is the whole reason these tests can say anything: it reproduces what a
/// rolling restart, an LB drain or a GOAWAY does to a long-lived `await_change`
/// stream — the connection carrying it dies, the server and its subscription
/// table live on, and the client's next unary call reconnects through a fresh
/// one. Killing the *server* instead would confound the two variables under
/// test, because it would take the lease store down with the subscription.
///
/// Deliberately kept inline rather than promoted to `tests/common/`: it has one
/// consumer today, and `tests/common/mod.rs` is currently about stub *backends*
/// rather than transport plumbing. If a second test file needs it — a cache-watch
/// or lock-renewal equivalent would — that module is the natural home, and this
/// type moves there whole.
struct CuttableRelay {
    addr: SocketAddr,
    live: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
}

impl CuttableRelay {
    async fn in_front_of(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
        let addr = listener.local_addr().expect("has an address");
        let live: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let accepting = Arc::clone(&live);
        tokio::spawn(async move {
            while let Ok((mut inbound, _peer)) = listener.accept().await {
                let relay = tokio::spawn(async move {
                    let Ok(mut outbound) = TcpStream::connect(upstream).await else {
                        return;
                    };
                    let _copied = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                });
                accepting
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(relay.abort_handle());
            }
        });
        Self { addr, live }
    }

    /// Severs every connection currently open through the relay. New ones are
    /// still accepted, so this breaks streams without making the gear
    /// unreachable — which is exactly the distinction under test.
    fn cut(&self) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for relay in live.drain(..) {
            relay.abort();
        }
    }

    fn leader(&self) -> Arc<dyn cluster_sdk::LeaderElectionBackend> {
        leader_at(self.addr)
    }
}

/// Polls `join` until it takes the election, or `window` elapses.
///
/// `Some(elapsed)` is the moment the incumbent's claim stopped being renewed
/// hard enough for a contender to take it. The claim is handed straight back, so
/// the probe never becomes the thing that broke the election.
async fn taken_within(
    contender: &Arc<dyn cluster_sdk::LeaderElectionBackend>,
    window: Duration,
) -> Option<Duration> {
    let config = election_config();
    let started = tokio::time::Instant::now();
    while started.elapsed() < window {
        if let Ok(Some(token)) = contender.join(ELECTION, "contender", config).await {
            let at = started.elapsed();
            let _best_effort = contender.resign(&token).await;
            return Some(at);
        }
        // 50 ms rather than a tight spin: the steal is still located to within
        // one poll, and the probe stays cheap enough that it cannot itself be
        // what delays the pump it is measuring.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

/// Elects, and asserts the initial status really is leadership.
async fn lead(leader: &Arc<dyn cluster_sdk::LeaderElectionBackend>) -> cluster_sdk::LeaderWatch {
    let mut watch = leader
        .elect_with_config(ELECTION, election_config())
        .await
        .expect("the sole candidate leads");
    let first = tokio::time::timeout(EVENT_TIMEOUT, watch.changed())
        .await
        .expect("an initial status arrives");
    assert!(
        matches!(first, LeaderWatchEvent::Status(LeaderStatus::Leader)),
        "expected leadership before testing what keeps it, got: {first:?}"
    );
    watch
}

// ---------------------------------------------------------------------------
// `ELEC-1` — a subscription-level close must not touch the renewal task
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn losing_the_subscription_stops_the_renewal_task_profile3() {
    // ADR-003's "Watch task and renewal task: independent signal paths", as an
    // end-to-end property: cutting the connection that carries `await_change`
    // must cost a re-subscribe and nothing else (§6.6), because §5.8.2 makes
    // "rolling the pod leaves every leader claim exactly where it was" a gate.
    //
    // The control loop runs first and is not decoration: without it, the
    // post-cut `None` could pass for the wrong reason (a contender that never
    // works, a fixture that never elects). It proves the probe *can* observe a
    // steal and that the pump is genuinely renewing across two full TTLs before
    // anything is broken.
    //
    // Margins. Broken: the pump returns at the cut, so the claim lapses ~1.0-1.5 s
    // later and the probe sees it inside a 3 s window — detected with >2x room.
    // Fixed: the pump renews every 500 ms against a 1500 ms TTL, so it would have
    // to be starved for a full second to flake.
    let fixture = Fixture::start().await;
    let relay = CuttableRelay::in_front_of(fixture.addr).await;
    let leader = relay.leader();
    let contender = fixture.leader();

    let watch = lead(&leader).await;

    let control = taken_within(&contender, TTL * 2).await;
    assert_eq!(
        control, None,
        "control: with its subscription healthy the pump renews, so no contender can \
         take the election inside two TTLs"
    );

    relay.cut();

    let stolen = taken_within(&contender, TTL * 2).await;
    assert_eq!(
        stolen, None,
        "ADR-003/6.6/5.8.2: losing the subscription must not cost the claim - but a \
         contender took the election {stolen:?} after the subscription closed, because \
         the pump that renews it returned when the stream did"
    );

    drop(watch);
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_restart_cycle_cannot_readopt_the_orphaned_claim() {
    // §5.8.3 states it as a gate: "a killed replica costs subscribers one
    // `RestartingWatch` cycle and no lease". So a consumer doing exactly what the
    // design tells it to do after a broken feed - give up the handle and
    // re-`elect` - must come back leader promptly, not queue behind a claim its
    // own previous pump walked away from without resigning.
    //
    // Margins, and why the bound is absolute rather than a fraction of the TTL.
    // Fixed, the wait is one resign round trip plus one poll (~20 ms) *whatever*
    // the TTL is, because the pump gives the claim back on its way out. Broken,
    // it cannot be less than `TTL - CADENCE` = 1000 ms, because that is the
    // shortest a claim whose last renewal already happened can take to lapse. A
    // 400 ms bound therefore sits ~20x above the fixed case and ~2.5x below the
    // broken one.
    let fixture = Fixture::start().await;
    let relay = CuttableRelay::in_front_of(fixture.addr).await;
    let leader = relay.leader();

    let watch = lead(&leader).await;

    relay.cut();
    // Long enough that the pump's first re-attach (due one backoff step after the
    // close) has already resolved, so this measures the teardown resign and not a
    // race against an in-flight re-subscribe.
    tokio::time::sleep(CADENCE).await;
    drop(watch);

    // Polled rather than measured in one shot: the teardown resign is best-effort
    // and off the caller's path in *both* profiles, so a single immediate
    // re-elect would be racing it. What the design forbids is being stuck behind
    // the orphaned claim for its TTL, and that is what this measures.
    let started = tokio::time::Instant::now();
    let (waited, again) = loop {
        let candidate = leader
            .elect_with_config(ELECTION, election_config())
            .await
            .expect("the consumer re-elects");
        if matches!(candidate.status(), LeaderStatus::Leader) {
            break (started.elapsed(), candidate);
        }
        drop(candidate);
        assert!(
            started.elapsed() < TTL * 3,
            "the re-elect never became leader inside three TTLs"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    println!("[elec-1] the re-elect became leader after {waited:?}");

    assert!(
        waited < Duration::from_millis(400),
        "5.8.2 says the restart costs one RestartingWatch cycle and no lease, but the \
         re-elect was a FOLLOWER behind its own un-resigned claim for {waited:?} (the \
         claim's own {TTL:?} TTL)"
    );

    drop(again);
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broken_feed_shows_the_consumer_reset_and_never_closed() {
    // What the consumer observes across a re-subscribe, which invariant I1 makes
    // a decision rather than an implementation detail.
    //
    // `Reset` is §6.8's own definition - "the server's upstream subscription was
    // re-established" - and the same event ADR-003 has `RestartingWatch`
    // synthesise on every successful resubscribe. `Closed` is the one thing it
    // must *not* be: ADR-003 makes `Closed` terminal ("providers MUST ensure no
    // further items are yielded"), and a terminal event here is precisely what
    // cost the claim. Profile 1 forwards `Reset` on a `LeaderWatch` too
    // (`defaults/leader.rs`, `on_watch_event`), so this is one event vocabulary
    // on both sides of the socket.
    //
    // Not timing-sensitive: the re-attach is due one backoff step (100 ms) after
    // the cut and the wait allows a full TTL.
    let fixture = Fixture::start().await;
    let relay = CuttableRelay::in_front_of(fixture.addr).await;
    let leader = relay.leader();

    let mut watch = lead(&leader).await;

    relay.cut();

    let next = tokio::time::timeout(TTL, watch.changed())
        .await
        .expect("a re-subscribe must be observable inside one TTL");
    println!("[elec-1] the consumer observed: {next:?}");
    assert!(
        matches!(next, LeaderWatchEvent::Reset),
        "a re-established subscription is section 6.8's `Reset`, got: {next:?}"
    );
    assert!(
        watch.is_leader(),
        "and the claim is untouched across it - section 6.6, 'losing it costs a re-subscribe, \
         not a leadership change'"
    );

    drop(watch);
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_permanently_unreattachable_feed_still_keeps_the_claim() {
    // The give-up branch, which is the riskiest thing the fix adds. Once the
    // subscription has been **reaped** (§5.4.1) no re-`attach` can ever succeed:
    // `attach` answers `None` and the server returns `NotFound`, which is not
    // retryable. The pump must stop re-attaching and go on renewing anyway,
    // exactly as Profile 1 does after `None => cache_watch = None`. Stopping
    // instead would be `ELEC-1` again by a slower route, and retrying forever
    // would be a new bug of its own.
    //
    // The sweep is driven by hand with a zero grace window rather than waiting
    // out the shipped 15 s: what is under test is the *client's* response to an
    // unreattachable subscription, not the sweep's own timing, which
    // `remote_backends.rs` already covers.
    let fixture = Fixture::start().await;
    let relay = CuttableRelay::in_front_of(fixture.addr).await;
    let leader = relay.leader();
    let contender = fixture.leader();

    let watch = lead(&leader).await;

    relay.cut();

    // Polled, not slept: the server notices the departed reader through its
    // stream task's `tx.closed()` arm, and how fast that happens is scheduling,
    // not contract.
    let reaped = tokio::time::timeout(EVENT_TIMEOUT, async {
        loop {
            let report = fixture.subscriptions.sweep(Duration::ZERO);
            if report.reaped_total() >= 1 {
                return report.reaped_total();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a broken stream must leave a reader-less entry for the sweep to reap");
    println!("[elec-1] swept {reaped} subscription(s)");

    let stolen = taken_within(&contender, TTL * 2).await;
    assert_eq!(
        stolen, None,
        "a pump with no recoverable feed must keep renewing - the claim is a row in \
         the store and only these renewals sustain it (I7, I8); a contender took it \
         {stolen:?} in"
    );
    assert!(
        watch.is_leader(),
        "and the consumer still believes it, because nothing revoked it"
    );

    drop(watch);
    fixture.stop().await;
}

// ---------------------------------------------------------------------------
// `SEAM-3` — a fake server that wins the join and then refuses the subscription
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Tally {
    joins: AtomicU64,
    resigns: AtomicU64,
    renews: AtomicU64,
}

impl Tally {
    fn get(&self) -> (u64, u64, u64) {
        (
            self.joins.load(Ordering::SeqCst),
            self.resigns.load(Ordering::SeqCst),
            self.renews.load(Ordering::SeqCst),
        )
    }
}

/// Answers `join` with the configured outcome and `await_change` with the bare
/// `NotFound` the shipped server really returns when the subscription is not on
/// the replica serving the call (`api/grpc/leader.rs`).
///
/// A fake rather than the real gear because the real one has no way to fail
/// `await_change` while succeeding `join` — which is exactly the split a mesh
/// like Linkerd or Istio produces, and the condition `SEAM-3` lives in.
struct RefusesTheSubscription {
    tally: Arc<Tally>,
    /// `Leader` for the winner arm, `Follower` for the control arm.
    status: dto::LeaderStatusDto,
}

#[tonic::async_trait]
impl stubs::leader::leader_election_api_server::LeaderElectionApi for RefusesTheSubscription {
    async fn join(
        &self,
        _request: tonic::Request<stubs::leader::JoinRequest>,
    ) -> Result<tonic::Response<stubs::leader::LeaderJoined>, tonic::Status> {
        self.tally.joins.fetch_add(1, Ordering::SeqCst);
        let token = match self.status {
            dto::LeaderStatusDto::Leader => dto::LeaseToken {
                name: ELECTION.to_owned(),
                owner: "unauthenticated".to_owned(),
                fence: 7,
            },
            // The zero token a follower receives, because `LeaderJoined.token`
            // is not optional on the wire (§6.6, Appendix A).
            _ => dto::LeaseToken {
                name: String::new(),
                owner: String::new(),
                fence: 0,
            },
        };
        Ok(tonic::Response::new(stubs::leader::LeaderJoined::from(
            dto::LeaderJoined {
                token,
                election_id: "sub-1".to_owned(),
                initial_status: self.status,
            },
        )))
    }

    async fn renew(
        &self,
        _request: tonic::Request<stubs::leader::LeaseRef>,
    ) -> Result<tonic::Response<stubs::leader::RenewResponse>, tonic::Status> {
        self.tally.renews.fetch_add(1, Ordering::SeqCst);
        Ok(tonic::Response::new(stubs::leader::RenewResponse::from(
            dto::RenewResponse { generation: 1 },
        )))
    }

    async fn resign(
        &self,
        request: tonic::Request<stubs::leader::LeaseRef>,
    ) -> Result<tonic::Response<stubs::leader::ResignResponse>, tonic::Status> {
        let lease = dto::LeaseRef::from(request.into_inner());
        println!(
            "[seam-3] resign for token name={:?} owner={:?} fence={}",
            lease.token.name, lease.token.owner, lease.token.fence
        );
        self.tally.resigns.fetch_add(1, Ordering::SeqCst);
        Ok(tonic::Response::new(stubs::leader::ResignResponse::from(
            dto::ResignResponse { generation: 1 },
        )))
    }

    type AwaitChangeStream = tokio_stream::wrappers::ReceiverStream<
        Result<stubs::leader::LeaderWatchEventDto, tonic::Status>,
    >;

    async fn await_change(
        &self,
        _request: tonic::Request<stubs::leader::AwaitChangeRequest>,
    ) -> Result<tonic::Response<Self::AwaitChangeStream>, tonic::Status> {
        Err(tonic::Status::not_found("unknown election_id"))
    }
}

async fn serve_fake(
    status: dto::LeaderStatusDto,
) -> (
    Arc<Tally>,
    Arc<dyn cluster_sdk::LeaderElectionBackend>,
    tokio::sync::oneshot::Sender<()>,
) {
    let tally = Arc::new(Tally::default());
    let service = RefusesTheSubscription {
        tally: Arc::clone(&tally),
        status,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let addr = listener.local_addr().expect("has an address");
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        Server::builder()
            .add_service(
                stubs::leader::leader_election_api_server::LeaderElectionApiServer::new(service),
            )
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _stopped = shutdown_rx.await;
            })
            .await
            .expect("the fake server runs");
    });
    (tally, leader_at(addr), shutdown)
}

#[tokio::test]
async fn a_won_claim_is_given_back_when_the_subscription_cannot_be_opened() {
    // `SEAM-3`: `enrol` calls `join_once` - which takes the lease server-side -
    // then `subscribe`, then `?`-propagates a subscribe failure. No pump exists
    // yet, so nothing renews and nothing resigns, and the election name is held
    // for a full TTL by a call that already returned an error. Profile 1 has no
    // such window, which makes this a Profile-3-only failure mode (I1).
    let (tally, leader, shutdown) = serve_fake(dto::LeaderStatusDto::Leader).await;

    let outcome = leader.elect_with_config(ELECTION, election_config()).await;
    let error = outcome.err().expect("the subscription failure propagates");
    println!("[seam-3] elect() error:  {error:?}");
    println!("[seam-3] is_retryable(): {}", error.is_retryable());

    // The resign is best-effort and off the caller's path, so it is polled for
    // rather than slept on.
    let _settled = tokio::time::timeout(EVENT_TIMEOUT, async {
        while tally.resigns.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    let (joins, resigns, renews) = tally.get();
    println!("[seam-3] joins observed:   {joins}");
    println!("[seam-3] resigns observed: {resigns}");
    assert_eq!(joins, 1, "exactly one join");
    assert_eq!(
        resigns, 1,
        "a claim won and then abandoned must be given back"
    );
    assert_eq!(
        renews, 0,
        "no pump was ever started, so nothing may be renewing"
    );

    let _stopped = shutdown.send(());
}

#[tokio::test]
async fn a_follower_whose_subscription_fails_resigns_nothing() {
    // The control for the test above, and the hazard Appendix A records: a
    // follower receives the *zero* token (empty name and owner, `fence: 0`)
    // because `LeaderJoined.token` is not optional on the wire. Resigning
    // unconditionally would send that zero token to the server on every lost
    // election, so the winner check must read `initial_status` and never the
    // token's shape (§6.6).
    //
    // This passes both before and after the fix, on purpose: it is what stops the
    // `SEAM-3` fix from being "resign always", which would look just as green.
    let (tally, leader, shutdown) = serve_fake(dto::LeaderStatusDto::Follower).await;

    let outcome = leader.elect_with_config(ELECTION, election_config()).await;
    assert!(
        outcome.is_err(),
        "the subscription failure still propagates"
    );

    // Long enough that a stray resign would have landed.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let (joins, resigns, _renews) = tally.get();
    println!("[seam-3] follower resigns observed: {resigns}");
    assert_eq!(joins, 1, "exactly one join");
    assert_eq!(
        resigns, 0,
        "a follower holds no claim, so it must send no resign - the zero token must \
         never reach the server"
    );

    let _stopped = shutdown.send(());
}
