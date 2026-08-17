//! End-to-end lifecycle test for the `cluster` gear: drive `init` → `start` →
//! `stop` through a mock `GearCtx` and assert backends register under the
//! configured profile and unbind on stop.
//!
//! Also `S3`'s capability set. Its exit criterion — that neither
//! `get_grpc_services` nor `healthcheck()` captures a backend — is asserted the
//! only way that can actually fail: both are collected **before** `start`, then
//! the *same* server and the *same* check are observed changing answer once
//! `start` publishes the registry under them.

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::grpc::stubs;
use cluster_sdk::{
    ClusterCacheBackend, ClusterCacheV1, ClusterClient, ClusterError, ClusterProfile,
};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Channel, Server};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::contracts::{GrpcServiceCapability, RestApiCapability, RunnableCapability};
use toolkit::{ConfigProvider, Gear, GearCtx, HealthcheckStatus};

use super::ClusterGear;

/// Returns the `cluster` gear's config entry for `cluster`, and nothing else.
struct MockConfig(serde_json::Value);

impl ConfigProvider for MockConfig {
    fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
        (gear_name == "cluster").then_some(&self.0)
    }
}

#[derive(Clone, Copy)]
struct DefaultProfile;
impl ClusterProfile for DefaultProfile {
    const NAME: &'static str = "default";
}

/// A `GearCtx` carrying one standalone-cache profile named `default`.
fn ctx_with_default_profile(hub: &Arc<ClientHub>) -> GearCtx {
    let provider = Arc::new(MockConfig(serde_json::json!({
        "config": { "profiles": { "default": { "cache": { "provider": "standalone" } } } }
    })));
    GearCtx::new(
        "cluster",
        uuid::Uuid::new_v4(),
        provider,
        Arc::clone(hub),
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn gear_lifecycle_registers_then_unbinds() {
    let hub = Arc::new(ClientHub::default());
    // The provider returns the gear entry; `ctx.config()` reads its `config` field.
    let provider = Arc::new(MockConfig(serde_json::json!({
        "config": { "profiles": { "default": { "cache": { "provider": "standalone" } } } }
    })));
    let ctx = GearCtx::new(
        "cluster",
        uuid::Uuid::new_v4(),
        provider,
        Arc::clone(&hub),
        CancellationToken::new(),
    );

    let gear = ClusterGear::default();
    gear.init(&ctx)
        .await
        .expect("init parses config and captures the hub");

    // The registry exists after `init` and is empty: the gear's services and
    // healthcheck are collected in this window, so they capture it rather than a
    // backend, and anything that resolves through it here is refused
    // (DESIGN §4.2, §5.2).
    let profiles = Arc::clone(gear.profiles.get().expect("init creates the registry"));
    assert_eq!(profiles.generation(), 0, "init publishes nothing");
    assert!(matches!(
        profiles.resolve("default"),
        Err(ClusterError::ProfileNotBound { .. })
    ));

    gear.start(CancellationToken::new())
        .await
        .expect("start wires backends from config");

    // The configured cache (and the omit-default trio over it) resolves.
    assert!(
        ClusterCacheV1::resolver(&hub)
            .profile(DefaultProfile)
            .resolve()
            .await
            .is_ok(),
        "the standalone cache is registered for the `default` profile"
    );
    // ...and the same profile is now addressable by name through the registry,
    // which is what the wire services and `LocalClusterClient` dispatch through.
    assert_eq!(profiles.generation(), 1, "start publishes the bound set");
    let bound = profiles
        .resolve("default")
        .expect("the configured profile is addressable after start");
    assert_eq!(bound.descriptor().cache.provider, "standalone");

    gear.stop(CancellationToken::new())
        .await
        .expect("stop tears the wiring down");

    assert!(
        matches!(
            ClusterCacheV1::resolver(&hub)
                .profile(DefaultProfile)
                .resolve()
                .await,
            Err(ClusterError::ProfileNotBound { .. })
        ),
        "stop deregisters the profile's backends"
    );
    assert!(
        matches!(
            profiles.resolve("default"),
            Err(ClusterError::ProfileNotBound { .. })
        ),
        "and the registry no longer routes to the backends being torn down"
    );
    assert_eq!(
        profiles.generation(),
        2,
        "the clearing swap is a generation"
    );
}

#[tokio::test(start_paused = true)]
async fn start_runs_the_subscription_sweep_and_stop_ends_it() {
    // Item `S2` / section 5.4.1's production wiring, which the table's own tests
    // and the integration tests both step around - they call the sweep directly
    // or spawn their own. What is asserted here is that the *shipped gear*
    // spawns one on its own cadence and cancels it, because a sweep nothing
    // starts is a table that grows forever with every test still green.
    let hub = Arc::new(ClientHub::default());
    let ctx = ctx_with_default_profile(&hub);
    let gear = ClusterGear::default();

    gear.init(&ctx).await.expect("init");
    let subscriptions = gear.subscriptions().expect("init created the table");

    // Abandoned from the moment it is opened: `join` registers, `await_change`
    // never comes. This is the follower pump's entry.
    let _abandoned = subscriptions.open("event-broker", "ledger", "default");
    assert_eq!(subscriptions.len(), 1);

    gear.start(CancellationToken::new()).await.expect("start");

    // Nothing yet: the entry is inside its grace window, and a sweep that reaped
    // on the first pass would close the `join`-to-`await_change` window.
    tokio::time::advance(crate::api::grpc::SWEEP_INTERVAL).await;
    tokio::task::yield_now().await;
    assert_eq!(
        subscriptions.len(),
        1,
        "reaped inside its grace window, which no client could survive"
    );

    tokio::time::advance(crate::api::grpc::sweep_grace(
        crate::api::grpc::SWEEP_INTERVAL,
    ))
    .await;
    tokio::task::yield_now().await;
    assert!(
        subscriptions.is_empty(),
        "the gear's own sweep must reap an abandoned subscription past its window"
    );

    gear.stop(CancellationToken::new()).await.expect("stop");

    // And it is gone: a fresh abandoned entry outlives every window there is.
    let _after_stop = subscriptions.open("event-broker", "ledger", "default");
    tokio::time::advance(crate::api::grpc::SWEEP_INTERVAL * 20).await;
    tokio::task::yield_now().await;
    assert_eq!(
        subscriptions.len(),
        1,
        "the sweep must not outlive the gear that spawned it"
    );
}

#[tokio::test]
async fn gear_with_no_config_starts_empty() {
    // No `cluster` entry → default (empty) config → start binds nothing, no panic.
    let hub = Arc::new(ClientHub::default());
    let provider = Arc::new(MockConfig(serde_json::json!({})));
    let ctx = GearCtx::new(
        "other-gear",
        uuid::Uuid::new_v4(),
        provider,
        Arc::clone(&hub),
        CancellationToken::new(),
    );

    let gear = ClusterGear::default();
    gear.init(&ctx).await.expect("init");
    gear.start(CancellationToken::new())
        .await
        .expect("start with empty config");

    // No profile was bound — an empty config must not register anything.
    assert!(
        matches!(
            ClusterCacheV1::resolver(&hub)
                .profile(DefaultProfile)
                .resolve()
                .await,
            Err(ClusterError::ProfileNotBound { .. })
        ),
        "an empty config must bind no profile"
    );

    gear.stop(CancellationToken::new()).await.expect("stop");
}

// ---------------------------------------------------------------------------
// `S3` — the capability set
// ---------------------------------------------------------------------------

/// The four wire service names, in the order `get_grpc_services` returns them.
///
/// Spelled out rather than read back from the generated constants: these strings
/// are the wire contract routing keys on, and a test that derives them from the
/// same source it is checking would pass through a rename that breaks every
/// client. Four packages, not one `cluster.v1` — see `cluster-sdk/src/grpc.rs`.
const SERVICE_NAMES: [&str; 4] = [
    "cluster.cache.v1.ClusterCacheApi",
    "cluster.lock.v1.DistributedLockApi",
    "cluster.leader.v1.LeaderElectionApi",
    "cluster.profile.v1.ClusterProfileApi",
];

#[tokio::test]
async fn the_gear_exports_the_four_coordination_services_before_start() {
    // Collected in the gRPC registration phase, which runs *before* `start` -- so
    // succeeding here at all is the criterion: a `get_grpc_services` that needed a
    // backend could not return, because no backend exists yet (DESIGN section 4.2).
    let hub = Arc::new(ClientHub::default());
    let ctx = ctx_with_default_profile(&hub);
    let gear = ClusterGear::default();
    gear.init(&ctx).await.expect("init");

    let services = gear
        .get_grpc_services(&ctx)
        .await
        .expect("services are collected without any backend existing");

    let names: Vec<&str> = services.iter().map(|s| s.service_name).collect();
    assert_eq!(names, SERVICE_NAMES);
}

#[tokio::test]
async fn each_service_installer_is_reusable() {
    // `RegisterGrpcServiceFn::register` is an `Fn`, not an `FnOnce`: the framework
    // may install into more than one builder. Running each twice proves the clone
    // path rather than assuming a single call.
    let hub = Arc::new(ClientHub::default());
    let ctx = ctx_with_default_profile(&hub);
    let gear = ClusterGear::default();
    gear.init(&ctx).await.expect("init");
    let services = gear.get_grpc_services(&ctx).await.expect("services");

    for round in 0..2 {
        let mut routes = tonic::service::RoutesBuilder::default();
        for service in &services {
            (service.register)(&mut routes);
        }
        assert!(
            routes.routes().into_axum_router().has_routes(),
            "round {round}: every installer must add its service"
        );
    }
}

#[tokio::test]
async fn services_and_healthcheck_follow_the_registry_across_start() {
    // The exit criterion, end to end and over a real socket. One server, built
    // once from the pre-`start` registration, and one healthcheck object: both
    // must change answer when `start` publishes the registry beneath them. A
    // captured backend could not do that, and a captured *snapshot* of the
    // registry could not either.
    let hub = Arc::new(ClientHub::default());
    let ctx = ctx_with_default_profile(&hub);
    let gear = ClusterGear::default();
    gear.init(&ctx).await.expect("init");

    let services = gear.get_grpc_services(&ctx).await.expect("services");
    let check = gear
        .healthcheck(&ctx)
        .expect("the gear contributes a readiness check");

    let mut routes = tonic::service::RoutesBuilder::default();
    for service in &services {
        (service.register)(&mut routes);
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let endpoint = format!("http://{}", listener.local_addr().expect("addressed"));
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(
        Server::builder()
            .add_routes(routes.routes())
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _stopped = shutdown_rx.await;
            }),
    );

    let channel = Channel::from_shared(endpoint)
        .expect("a valid endpoint")
        .timeout(Duration::from_secs(5))
        .connect()
        .await
        .expect("connects");
    let mut cache =
        stubs::cache::cluster_cache_api_client::ClusterCacheApiClient::new(channel.clone());

    // --- before `start`: routed, reachable, and refusing ---
    let refused = cache
        .get(stubs::cache::GetRequest {
            profile: "default".to_owned(),
            key: "ledger".to_owned(),
        })
        .await
        .expect_err("nothing is bound before start");
    assert_eq!(
        refused.code(),
        tonic::Code::NotFound,
        "an unbound profile is the NotFound-mapped ProfileNotBound, not a transport failure"
    );

    let starting = check.check().await;
    assert_eq!(starting.status, HealthcheckStatus::Unhealthy);
    assert_eq!(
        starting.code.as_deref(),
        Some("starting"),
        "which the framework renders as `starting`/503"
    );

    // --- `start` publishes the registry under both of them ---
    gear.start(CancellationToken::new()).await.expect("start");

    cache
        .put(stubs::cache::PutRequest {
            profile: "default".to_owned(),
            key: "ledger".to_owned(),
            value: b"41".to_vec(),
            ttl_ms: None,
            client_request_id: None,
        })
        .await
        .expect("the same server now serves the published profile");
    let entry = cache
        .get(stubs::cache::GetRequest {
            profile: "default".to_owned(),
            key: "ledger".to_owned(),
        })
        .await
        .expect("cache.Get is served")
        .into_inner()
        .entry
        .expect("the key was just written");
    assert_eq!(entry.value, b"41");

    let ready = check.check().await;
    assert_eq!(
        ready.status,
        HealthcheckStatus::Healthy,
        "the same check object reports Healthy once the profile is bound and probing"
    );

    let _sent = shutdown.send(());
    gear.stop(CancellationToken::new()).await.expect("stop");
}

#[tokio::test]
async fn the_healthcheck_reports_starting_when_collected_before_init() {
    // Unreachable in the framework's phase order, which is why it must not be a
    // silent `None`: opting out of readiness would report the pod ready with
    // nothing bound. A fresh empty registry is permanently at generation 0, so the
    // fail-safe verdict is `Starting`.
    let hub = Arc::new(ClientHub::default());
    let ctx = ctx_with_default_profile(&hub);
    let gear = ClusterGear::default();

    let check = gear
        .healthcheck(&ctx)
        .expect("a check is contributed even before init");
    let result = check.check().await;
    assert_eq!(result.status, HealthcheckStatus::Unhealthy);
    assert_eq!(result.code.as_deref(), Some("starting"));
}

#[tokio::test]
async fn rest_registration_adds_no_routes() {
    // The coordination data plane is gRPC only; no primitive is ever exposed over
    // REST (DESIGN section 2.2). `S4` adds the admin routes here, and this asserts
    // the surface is empty until it does.
    let hub = Arc::new(ClientHub::default());
    let ctx = ctx_with_default_profile(&hub);
    let gear = ClusterGear::default();
    gear.init(&ctx).await.expect("init");

    let router = gear
        .register_rest(&ctx, axum::Router::new(), &NoOpenApi)
        .expect("registration succeeds");
    assert!(
        !router.has_routes(),
        "cluster exposes no REST route until S4's admin plane"
    );
}

// ---------------------------------------------------------------------------
// `R5` — Profile 1's half of the seam
// ---------------------------------------------------------------------------

#[tokio::test]
async fn init_claims_the_client_and_start_makes_it_answer() {
    // `R5`'s exit criterion, literally: `hub.get::<dyn ClusterClient>()` succeeds
    // in a Profile 1 process after the cluster gear's `start`, routing to the
    // profile `start` actually wired.
    //
    // Since `K3` the claim happens one phase earlier, in `init`, and the two halves
    // are asserted separately below because they answer different questions.
    // **`init` registering the client is what keeps a cluster-hosting process from
    // wiring a remote client to its own socket**: cluster-sdk's
    // `ConsumerRegistration` is replayed in the proxy-wiring phase, which sits
    // between `init` and `start`, so a client that only arrived with `start` would
    // arrive too late for the local-wins check (DESIGN section 4.9.3 step 1).
    let hub = Arc::new(ClientHub::default());
    let ctx = ctx_with_default_profile(&hub);
    let gear = ClusterGear::default();
    gear.init(&ctx).await.expect("init");

    // Registered, and registered *locally* - the property the wiring phase probes.
    assert!(
        hub.try_get_local::<dyn ClusterClient>().is_some(),
        "init must claim `dyn ClusterClient` locally, before the proxy-wiring phase runs"
    );
    // But it answers nothing yet, because the registry it dispatches through is
    // empty until `start` publishes. `ProfileNotBound` is the correct answer for
    // that window and needs no new variant (invariant I3).
    let before_start = hub
        .get::<dyn ClusterClient>()
        .expect("init registered a client");
    assert!(
        matches!(
            before_start.cache_backend("default"),
            Err(ClusterError::ProfileNotBound { .. })
        ),
        "a client registered before `start` must report ProfileNotBound, not a backend"
    );

    gear.start(CancellationToken::new()).await.expect("start");

    let client = hub
        .get::<dyn ClusterClient>()
        .expect("start registers the local client under the trait consumers resolve through");
    let descriptor = client
        .descriptor("default")
        .await
        .expect("the wired profile is described");
    assert_eq!(descriptor.cache.provider, "standalone");

    // It must be visible to the *local*-wins probe specifically, not merely
    // present: a registration that read as a remote proxy would let a consumer
    // build a second, remote client alongside it (DESIGN section 4.9.3).
    assert!(
        hub.try_get_local::<dyn ClusterClient>().is_some(),
        "the local client must win over any remote proxy"
    );

    // And it hands back the same backend instance the hub already holds under
    // `cluster:default` - one object per process, no wrapper (invariant I14).
    // The scope string is spelled out because it *is* the stable contract the
    // resolvers key on, and `profile_scope` is `pub(crate)` in the SDK.
    let via_client = client.cache_backend("default").expect("bound");
    let via_hub: Arc<dyn ClusterCacheBackend> = hub
        .get_scoped(&ClientScope::new("cluster:default"))
        .expect("the scoped registration is still there");
    assert!(
        Arc::ptr_eq(&via_client, &via_hub),
        "both paths must reach the one real backend"
    );

    gear.stop(CancellationToken::new()).await.expect("stop");

    // `stop` leaves the registration in place on purpose: the cleared registry is
    // what refuses the call, and it can name the profile while an absent client
    // could not (see `start`).
    let after_stop = hub
        .get::<dyn ClusterClient>()
        .expect("the client outlives the wiring");
    assert!(matches!(
        after_stop.cache_backend("default"),
        Err(ClusterError::ProfileNotBound { .. })
    ));
}

/// An `OpenApiRegistry` that records nothing — `register_rest` registers no route,
/// so it is never called.
struct NoOpenApi;

impl toolkit::contracts::OpenApiRegistry for NoOpenApi {
    fn register_operation(&self, _spec: &toolkit::api::OperationSpec) {}

    fn ensure_schema_raw(
        &self,
        root_name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        root_name.to_owned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
