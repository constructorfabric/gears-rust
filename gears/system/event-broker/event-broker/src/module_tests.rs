//! Per-mode service/route gating tests: which routes and services each
//! `DeploymentMode` registers, and which it must not.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationSpec;
use toolkit::{ClientHub, Gear, GearCtx, RestApiCapability};
use tower::ServiceExt;
use uuid::Uuid;

use super::{BACKEND_STOP_GRACE, EventBrokerModule, start_backends, stop_backends};
use crate::config::{DeploymentMode, EventBrokerConfig};
use crate::test_support::module_boot::StaticConfigProvider;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, LogClaim, Provisioning, Role,
    TopicPartition, TopicSpec, Wait, Watch,
};
use event_broker_sdk::models::Event;
use event_broker_sdk::{PartitionRange, Position, Sequence, TopicSegment};

struct NoopOpenApiRegistry;

impl OpenApiRegistry for NoopOpenApiRegistry {
    fn register_operation(&self, _spec: &OperationSpec) {}

    fn ensure_schema_raw(
        &self,
        name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        name.to_owned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// A fresh temp-file `SQLite` DB with this crate's migrations applied -
/// `init()` now resolves a real `DBProvider` (`ctx.db_required()`) and runs
/// `SpecificationManager::bulk_load()` against it
/// (eb-single-process-implementation D1), unlike before this change.
async fn test_db_provider() -> toolkit_db::DBProvider<toolkit_db::DbError> {
    test_db_provider_at().await.0
}

/// As [`test_db_provider`], with the DSN too, for a test that reads a table
/// over a connection of its own.
async fn test_db_provider_at() -> (toolkit_db::DBProvider<toolkit_db::DbError>, String) {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-eb-module-test-{}.db", Uuid::now_v7().simple()));
    let mut file = path.to_string_lossy().replace('\\', "/");
    if !file.starts_with('/') {
        file.insert(0, '/');
    }
    let dsn = format!("sqlite://{file}?mode=rwc");
    let opts = toolkit_db::ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = toolkit_db::connect_db(&dsn, opts)
        .await
        .expect("connect sqlite");
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        <crate::infra::storage::migrations::Migrator as sea_orm_migration::MigratorTrait>::migrations(),
    )
    .await
    .expect("migrations");
    (toolkit_db::DBProvider::new(db), dsn)
}

async fn make_ctx(mode: &str) -> GearCtx {
    make_ctx_with_hub(mode, Arc::new(ClientHub::new())).await
}

/// A context for `mode` over `hub`, wired as
/// [`module_ctx`](crate::test_support::module_boot::module_ctx) wires one, with
/// a freshly-migrated temp-file `SQLite` gear database attached.
async fn make_ctx_with_hub(mode: &str, hub: Arc<ClientHub>) -> GearCtx {
    let (db, _dsn) = test_db_provider_at().await;
    make_ctx_with_hub_and_config(mode, hub, &json!({}), db).await
}

/// As [`make_ctx_with_hub`], with `extra` merged into the `event-broker`
/// config and `db` attached.
async fn make_ctx_with_hub_and_config(
    mode: &str,
    hub: Arc<ClientHub>,
    extra: &serde_json::Value,
    db: toolkit_db::DBProvider<toolkit_db::DbError>,
) -> GearCtx {
    crate::test_support::module_boot::module_ctx(mode, hub, extra, db).await
}

/// A component a mode turns on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Component {
    Ingest,
    Delivery,
    Dispatcher,
    Reaper,
}

/// Everything `mode` turns on, as one value: its components and what it is
/// to its storage backends.
fn activation(mode: DeploymentMode) -> (BTreeSet<Component>, Option<Role>) {
    let components = [
        (Component::Ingest, mode.ingest_active()),
        (Component::Delivery, mode.delivery_active()),
        (Component::Dispatcher, mode.dispatcher_active()),
        (Component::Reaper, mode.reaper_active()),
    ]
    .into_iter()
    .filter_map(|(component, active)| active.then_some(component))
    .collect();
    (components, mode.backend_role())
}

#[test]
fn standalone_activates_ingest_delivery_and_reaper_but_no_dispatcher() {
    assert_eq!(
        activation(DeploymentMode::Standalone),
        (
            BTreeSet::from([Component::Ingest, Component::Delivery, Component::Reaper]),
            Some(Role::Standalone)
        )
    );
}

#[test]
fn cluster_ingest_activates_only_ingest_and_reaper() {
    assert_eq!(
        activation(DeploymentMode::ClusterIngest),
        (
            BTreeSet::from([Component::Ingest, Component::Reaper]),
            Some(Role::Ingest)
        )
    );
}

#[test]
fn cluster_delivery_activates_only_delivery_and_reaper() {
    assert_eq!(
        activation(DeploymentMode::ClusterDelivery),
        (
            BTreeSet::from([Component::Delivery, Component::Reaper]),
            Some(Role::Delivery)
        )
    );
}

#[test]
fn cluster_dispatcher_activates_only_the_dispatcher() {
    assert_eq!(
        activation(DeploymentMode::ClusterDispatcher),
        (BTreeSet::from([Component::Dispatcher]), None)
    );
}

/// The registry is built only where the checks prove a deployment and the
/// mode has a backend role; a dispatcher has neither, so `init()` leaves its
/// backends empty.
#[test]
fn cluster_dispatcher_builds_no_backend() {
    let cfg: EventBrokerConfig = serde_json::from_value(json!({
        "mode": "cluster_dispatcher",
        "backends": { "main": { "type": "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~" } },
        "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
    }))
    .expect("test configuration deserializes");
    let deployment = crate::config::checks::check(&cfg, cfg.mode).expect("passes the checks");

    assert_eq!(
        super::registry_to_build(deployment, cfg.mode).map(|(_, role)| role),
        None
    );
}

#[tokio::test]
async fn init_fails_for_unknown_mode() {
    let module = EventBrokerModule::default();
    let ctx = make_ctx("not_a_real_mode").await;

    let err = module
        .init(&ctx)
        .await
        .expect_err("init must reject an unrecognized mode string rather than silently accepting it or panicking");

    let message = format!("{err:#}");
    assert_eq!(
        message,
        "invalid config for gear 'event-broker': unknown variant `not_a_real_mode`, \
         expected one of `standalone`, `cluster_ingest`, `cluster_delivery`, `cluster_dispatcher`"
    );
}

/// Spec "Advertise-address resolution", "No `advertise_addr`, wildcard bind
/// address, cluster mode" - the `init`-level integration (the resolver-level
/// unit tests live in `infra::cluster::advertise_address::tests`).
#[tokio::test]
async fn init_fails_on_wildcard_bind_with_no_advertise_addr_in_cluster_mode() {
    let cfg = json!({
        "event-broker": {
            "config": {
                "mode": "cluster_ingest",
                "backends": { "main": { "type": "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~" } },
                "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
                // `registration` omitted entirely -> defaults to
                // `listen_addr: "0.0.0.0:0"`, `advertise_addr: None`.
            }
        }
    });
    let ctx = GearCtx::new(
        EventBrokerModule::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(StaticConfigProvider { root: cfg }),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    );

    let module = EventBrokerModule::default();
    let err = module
        .init(&ctx)
        .await
        .expect_err("a wildcard bind with no advertise_addr must fail init in cluster mode");

    assert!(
        format!("{err:#}").contains("wildcard address"),
        "error: {err:#}"
    );
}

/// The readiness `/readyz` reports: `Starting` until `serve()` has wired the
/// outbox and the cluster cache, then ready for as long as it serves, and
/// `serve()` returns promptly on a stop.
#[tokio::test]
async fn standalone_turns_ready_once_serve_has_wired_it() {
    let ctx = make_ctx("standalone").await;
    let module = Arc::new(EventBrokerModule::default());
    module.init(&ctx).await.expect("init must succeed");
    let check = module
        .healthcheck(&ctx)
        .expect("the gear always reports readiness");

    let before = check.check().await;
    assert_eq!(before.status, toolkit::HealthcheckStatus::Unhealthy);
    assert_eq!(before.code.as_deref(), Some("starting"));

    let cancel = CancellationToken::new();
    let serve_module = Arc::clone(&module);
    let serve_cancel = cancel.clone();
    let serve_task = tokio::spawn(async move { serve_module.serve(serve_cancel).await });

    let deadline = Instant::now() + Duration::from_secs(5);
    let after = loop {
        let result = check.check().await;
        if result.status == toolkit::HealthcheckStatus::Healthy || Instant::now() >= deadline {
            break result;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(after.status, toolkit::HealthcheckStatus::Healthy);
    assert_eq!(after.message, None);

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), serve_task)
        .await
        .expect("serve must return promptly on a stop")
        .expect("serve task must not panic")
        .expect("serve must return Ok on cancellation");
}

/// De-risks production route registration itself (`eb-rest-handlers` Group
/// 10): `register_ingest_routes`/`register_delivery_routes` register 19
/// `OperationBuilder` operations, three of them (`GET`/`DELETE`/`POST` on
/// `/v1/subscriptions/{id}`) sharing one literal path string across
/// separate `.register()` calls - unlike the `{id}` vs `{seek_id}` case
/// this change's `test_router` had to avoid, axum's `path_router` merges
/// method routers registered against the *same* path string
/// (`axum::routing::path_router::PathRouter::route`), so this is expected
/// to succeed; this test proves it does, not just that it type-checks.
#[tokio::test]
async fn standalone_register_rest_builds_router_and_serves_a_route_with_no_auth_dependency() {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn authz_resolver_sdk::AuthZResolverApi>(Arc::new(
        crate::test_support::authz_doubles::AllowAllAuthZ,
    ));
    hub.register::<dyn types_registry_sdk::PlatformTypesRegistryApi>(Arc::new(
        types_registry_sdk::testing_platform::MockTypesRegistry::new(),
    ));
    let ctx = make_ctx_with_hub("standalone", hub).await;
    let module = EventBrokerModule::default();
    module.init(&ctx).await.expect("init must succeed");

    // Every read handler is security-context-first, so the router the gear
    // registers expects an upstream auth layer to inject a `SecurityContext`.
    // This test stands in for that layer with an anonymous context, so it
    // still exercises route registration and method-router merging
    // end-to-end.
    let router = module
        .register_rest(&ctx, Router::new(), &NoopOpenApiRegistry)
        .expect("registering ingest+delivery routes must not fail")
        .layer(axum::Extension(
            toolkit_security::SecurityContext::anonymous(),
        ));

    let request = Request::builder()
        .method("GET")
        .uri("/event-broker/v1/topics")
        .body(Body::empty())
        .expect("request must build");
    let response = router
        .oneshot(request)
        .await
        .expect("the service itself must not error");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("body must be JSON");
    assert_eq!(json["items"], serde_json::json!([]));
}

/// A context whose `event-broker` config is exactly `config`, with a migrated
/// database attached, for `init` tests that stop at a startup check.
async fn ctx_with_config(config: serde_json::Value) -> GearCtx {
    GearCtx::new(
        EventBrokerModule::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(StaticConfigProvider {
            root: json!({ "event-broker": { "config": config } }),
        }),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    )
    .with_db(test_db_provider().await)
}

/// The startup checks run before anything is wired, and a failing one stops
/// `init` with its own message (feature 0005 section 2.4, C4).
#[tokio::test]
async fn init_fails_on_a_configuration_breaking_a_startup_check() {
    let ctx = ctx_with_config(json!({
        "mode": "standalone",
        "backends": { "main": { "type": "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~" } },
        "backend_routing": {},
    }))
    .await;

    let err = EventBrokerModule::default()
        .init(&ctx)
        .await
        .expect_err("C4 stops the start");

    assert_eq!(
        err.to_string(),
        "backend_routing: there is no route for `gts.cf.core.events.topic.v1~`; it is required so that every topic resolves to a registry entry (feature 0005 section 2.4, C4)"
    );
}

/// A registry entry no linked provider serves stops `init` (AC-1, C1).
#[tokio::test]
async fn init_fails_on_a_backend_type_no_linked_provider_serves() {
    let ctx = ctx_with_config(json!({
        "mode": "standalone",
        "backends": { "main": { "type": "gts.cf.core.events.backend.v1~vendor.eb.unknown.backend.v1~" } },
        "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
    }))
    .await;

    let err = EventBrokerModule::default()
        .init(&ctx)
        .await
        .expect_err("C1 stops the start");

    assert_eq!(
        err.to_string(),
        "backends: entry 'main' names backend type 'gts.cf.core.events.backend.v1~vendor.eb.unknown.backend.v1~', which no provider linked into this build serves; it links [\"gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~\", \"gts.cf.core.events.backend.v1~cf.core.backend.mysql.v1~\", \"gts.cf.core.events.backend.v1~cf.core.backend.postgres.v1~\", \"gts.cf.core.events.backend.v1~cf.core.backend.kafka.v1~\", \"gts.cf.core.events.backend.v1~cf.core.backend.sqlite.v1~\"]; every entry's type is served by a linked provider (feature 0005 section 2.4, C1)"
    );
}

/// C10 on the gear's own database: every cluster mode refuses a `SQLite` one
/// at `init`, and `standalone` runs on it (feature 0005 section 2.4).
#[tokio::test]
async fn init_refuses_a_sqlite_gear_database_outside_standalone() {
    for mode in ["cluster_ingest", "cluster_delivery", "cluster_dispatcher"] {
        let err = EventBrokerModule::default()
            .init(&make_ctx(mode).await)
            .await
            .expect_err("C10 stops the start");
        assert_eq!(
            err.to_string(),
            "database: the gear's database is SQLite; a SQLite database serves a single process, so only `standalone` mode runs on one (feature 0005 section 2.4, C10)",
            "{mode}"
        );
    }

    EventBrokerModule::default()
        .init(&make_ctx("standalone").await)
        .await
        .expect("standalone runs on a SQLite gear database");
}

/// `serve()` registers the `default` outbox group's queue with the count
/// `outbox_groups.default.partitions` configures, the same count `init()`
/// hands the ingest service.
#[tokio::test]
async fn serve_registers_the_default_outbox_group_with_its_configured_partitions() {
    let (db, dsn) = test_db_provider_at().await;
    let ctx = make_ctx_with_hub_and_config(
        "standalone",
        Arc::new(ClientHub::new()),
        &json!({ "outbox_groups": { "default": { "partitions": 4 } } }),
        db,
    )
    .await;
    let module = Arc::new(EventBrokerModule::default());
    module.init(&ctx).await.expect("init must succeed");

    let cancel = CancellationToken::new();
    let serve_module = Arc::clone(&module);
    let serve_cancel = cancel.clone();
    let serve_task = tokio::spawn(async move { serve_module.serve(serve_cancel).await });

    // Polled: the queue is registered once `serve()` reaches the pipeline,
    // which depends on how loaded the machine is. The deadline only bounds a
    // failure.
    let deadline = Instant::now() + Duration::from_secs(5);
    let registered = loop {
        let found = crate::test_support::outbox_queue_partitions(&dsn).await;
        if found > 0 || Instant::now() >= deadline {
            break found;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(registered, 4);

    cancel.cancel();
    serve_task
        .await
        .expect("serve task must not panic")
        .expect("serve must return Ok on cancellation");
}

/// AC-11: a backend unreachable at start does not stop the process. The
/// module initializes and serves, the reachable entry's topics are unaffected,
/// and the unreachable entry's topics answer `Unavailable`.
#[tokio::test]
async fn an_unreachable_backend_does_not_stop_the_start_and_its_topics_are_unavailable() {
    use event_broker_sdk::backend::{BackendError, Partition, TopicPartition};
    use event_broker_sdk::models::PartitionRange;

    const AUDIT_TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.audit.topic.v1";
    const ORDERS_TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.orders.topic.v1";
    let sections = json!({
        "backends": {
            "main": { "type": "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~" },
            // Port 1 on loopback: nothing listens there.
            "audit": {
                "type": "gts.cf.core.events.backend.v1~cf.core.backend.postgres.v1~",
                "url": "postgres://eb@127.0.0.1:1/eb",
                "schema": "evbk",
            },
        },
        "backend_routing": {
            "gts.cf.core.events.topic.v1~": "main",
            AUDIT_TOPIC: "audit",
        },
    });
    let (db, _dsn) = test_db_provider_at().await;
    let ctx =
        make_ctx_with_hub_and_config("standalone", Arc::new(ClientHub::new()), &sections, db).await;
    let module = Arc::new(EventBrokerModule::default());
    module
        .init(&ctx)
        .await
        .expect("an unreachable backend does not fail init");

    let cancel = CancellationToken::new();
    let serve_module = Arc::clone(&module);
    let serve_cancel = cancel.clone();
    let serve_task = tokio::spawn(async move { serve_module.serve(serve_cancel).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !serve_task.is_finished(),
        "serve keeps running with one backend unreachable"
    );

    let mut cfg = json!({ "mode": "standalone" });
    for (key, value) in sections.as_object().expect("an object literal") {
        cfg[key] = value.clone();
    }
    let cfg: EventBrokerConfig =
        serde_json::from_value(cfg).expect("test configuration deserializes");
    let routing = crate::config::checks::routing(&cfg).expect("routes every topic");
    let resolver = module
        .backend_resolver
        .get()
        .expect("init built the registry");
    let query = |id: &'static str| {
        let id = toolkit_gts::GtsInstanceId::try_new(id).expect("a valid topic id");
        let settings = crate::domain::resolution::resolve(
            &cfg,
            &routing,
            &id,
            &crate::domain::resolution::Declaration::default(),
        )
        .expect("resolves");
        let topic = crate::domain::model::Topic {
            id: id.clone(),
            description: "a topic".to_owned(),
            retention: None,
            settings,
        };
        let backend = resolver.resolve(&topic);
        async move {
            backend
                .query(
                    &TopicPartition::new(id, Partition::new(0)),
                    PartitionRange {
                        start_offset: None,
                        end_offset: None,
                        limit: 10,
                    },
                )
                .await
        }
    };

    assert!(
        matches!(
            query(AUDIT_TOPIC).await,
            Err(BackendError::Unavailable { .. })
        ),
        "the unreachable entry's topic answers Unavailable"
    );
    assert!(
        !matches!(
            query(ORDERS_TOPIC).await,
            Err(BackendError::Unavailable { .. })
        ),
        "the reachable entry's topic is unaffected"
    );

    cancel.cancel();
    serve_task
        .await
        .expect("serve task must not panic")
        .expect("serve must return Ok on cancellation");
}

/// A stop while the startup registry load waits on a registry that never
/// answers ends the start promptly: a process stopped right after launch
/// exits instead of hanging on the load.
#[tokio::test]
async fn a_stop_during_the_startup_registry_load_ends_the_start_promptly() {
    let hub = Arc::new(ClientHub::new());
    let ctx = make_ctx_with_hub("standalone", Arc::clone(&hub)).await;
    let module = Arc::new(EventBrokerModule::default());
    module.init(&ctx).await.expect("init must succeed");
    // After `init`: `serve()` resolves the registry for the startup load. Its
    // list calls never answer, as a registry that is unreachable without
    // refusing the connection would.
    let registry = Arc::new(types_registry_sdk::testing_platform::MockTypesRegistry::new());
    registry.inject(
        types_registry_sdk::testing_platform::Fault::on(
            types_registry_sdk::testing_platform::Call::List,
        )
        .delay(Duration::from_secs(3600)),
    );
    hub.register::<dyn types_registry_sdk::PlatformTypesRegistryApi>(
        Arc::clone(&registry) as Arc<dyn types_registry_sdk::PlatformTypesRegistryApi>
    );

    let cancel = CancellationToken::new();
    let serve_task = crate::test_support::module_boot::spawn_serve(&module, &cancel);
    tokio::time::timeout(Duration::from_secs(2), async {
        while registry.calls(types_registry_sdk::testing_platform::Call::List) < 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the startup load must reach the registry's list call within 2s");
    assert!(
        !serve_task.is_finished(),
        "the start waits on the registry load until the stop"
    );

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), serve_task)
        .await
        .expect("serve must return within 2s of the stop")
        .expect("serve task must not panic")
        .expect("serve must return Ok on a stop");
}

/// Bounds each await of the backend start and stop tests, so a stop that
/// never returns fails the test instead of hanging it.
const BOUND: Duration = Duration::from_secs(1);

/// A `start` or `stop` a [`LifecycleBackend`] saw, naming the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    Start(u8),
    Stop(u8),
}

type Journal = Arc<std::sync::Mutex<Vec<(Lifecycle, Duration)>>>;

/// Records each `start` and `stop` with the time it returned, measured from
/// `epoch`. Its `start` fails when `fails_start`; its `stop` returns only once
/// the deadline is cancelled when `waits_for_deadline`, and at once otherwise.
struct LifecycleBackend {
    id: u8,
    fails_start: bool,
    waits_for_deadline: bool,
    epoch: tokio::time::Instant,
    journal: Journal,
}

impl LifecycleBackend {
    fn record(&self, call: Lifecycle) {
        self.journal
            .lock()
            .expect("journal lock")
            .push((call, self.epoch.elapsed()));
    }
}

fn not_called() -> BackendError {
    BackendError::Internal("only start and stop are called".to_owned())
}

#[async_trait::async_trait]
impl Backend for LifecycleBackend {
    async fn provision(&self, _: &TopicSpec, _: &LogClaim) -> Result<Provisioning, BackendError> {
        Err(not_called())
    }
    async fn append(&self, _: &AppendRequest<'_>) -> Result<Appended, BackendError> {
        Err(not_called())
    }
    fn dedup(&self) -> Dedup {
        Dedup::Fenced
    }
    fn max_events_per_append(&self) -> usize {
        1
    }
    async fn read(
        &self,
        _: &TopicPartition,
        _: Sequence,
        _: usize,
        _: Wait,
    ) -> Result<Vec<Event>, BackendError> {
        Err(not_called())
    }
    async fn resolve(&self, _: &TopicPartition, _: Position) -> Result<Sequence, BackendError> {
        Err(not_called())
    }
    fn watch(&self) -> Watch<'_> {
        Watch::Poll(Duration::from_secs(1))
    }
    async fn query(
        &self,
        _: &TopicPartition,
        _: PartitionRange,
    ) -> Result<Vec<TopicSegment>, BackendError> {
        Err(not_called())
    }
    async fn start(&self) -> Result<(), BackendError> {
        self.record(Lifecycle::Start(self.id));
        if self.fails_start {
            return Err(BackendError::Internal("the medium refused".to_owned()));
        }
        Ok(())
    }
    async fn stop(&self, deadline: CancellationToken) {
        if self.waits_for_deadline {
            deadline.cancelled().await;
        }
        self.record(Lifecycle::Stop(self.id));
    }
}

fn lifecycle_backends(ids: &[(u8, bool, bool)], journal: &Journal) -> Vec<Arc<dyn Backend>> {
    let epoch = tokio::time::Instant::now();
    ids.iter()
        .map(|&(id, fails_start, waits_for_deadline)| {
            Arc::new(LifecycleBackend {
                id,
                fails_start,
                waits_for_deadline,
                epoch,
                journal: Arc::clone(journal),
            }) as Arc<dyn Backend>
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_failed_backend_start_stops_the_started_ones_in_reverse() {
    let journal = Journal::default();
    let backends = lifecycle_backends(
        &[(1, false, false), (2, false, false), (3, true, false)],
        &journal,
    );

    let result = tokio::time::timeout(BOUND, start_backends(&backends))
        .await
        .expect("the start returns in time");

    let Err(error) = result else {
        panic!("a failed backend start fails the start");
    };
    assert_eq!(
        error.to_string(),
        "could not start the storage backend: internal: the medium refused"
    );
    assert_eq!(
        *journal.lock().expect("journal lock"),
        vec![
            (Lifecycle::Start(1), Duration::ZERO),
            (Lifecycle::Start(2), Duration::ZERO),
            (Lifecycle::Start(3), Duration::ZERO),
            (Lifecycle::Stop(2), Duration::ZERO),
            (Lifecycle::Stop(1), Duration::ZERO),
        ]
    );
}

/// One grace covers the whole stop: backends stopping later get only what
/// remains of it, possibly none, before the shared deadline is cancelled.
#[tokio::test(start_paused = true)]
async fn stopping_backends_waits_out_one_shared_grace_then_cancels() {
    let journal = Journal::default();
    let backends = lifecycle_backends(&[(1, false, true), (2, false, true)], &journal);
    let epoch = tokio::time::Instant::now();

    tokio::time::timeout(BACKEND_STOP_GRACE + BOUND, stop_backends(&backends))
        .await
        .expect("the stop returns once the grace elapses");

    let elapsed = epoch.elapsed();
    assert!(
        (BACKEND_STOP_GRACE..BACKEND_STOP_GRACE + Duration::from_millis(1)).contains(&elapsed),
        "the stop returns when the grace elapses, not before: {elapsed:?}"
    );
    let journal = journal.lock().expect("journal lock").clone();
    let [(Lifecycle::Stop(2), second), (Lifecycle::Stop(1), first)] = journal.as_slice() else {
        panic!("both backends stop, in reverse: {journal:?}");
    };
    assert_eq!(
        (*second, *first),
        (elapsed, elapsed),
        "the cancelled deadline ends every waiting stop at once"
    );
}
