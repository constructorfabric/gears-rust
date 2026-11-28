//! Per-mode service/route gating tests: which routes and services each
//! `DeploymentMode` registers, and which it must not.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use cluster_sdk::cache::ClusterCacheBackend;
use cluster_sdk::dto::{ProfileDescriptor, ProfileHealth};
use cluster_sdk::leader::LeaderElectionBackend;
use cluster_sdk::lock::DistributedLockBackend;
use cluster_sdk::{ClusterClient, ClusterError};
use http_body_util::BodyExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationSpec;
use toolkit::client_hub::ClientHubError;
use toolkit::directory::{
    DirectoryClient, RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo,
};
use toolkit::{ClientHub, Gear, GearCtx, HealthcheckStatus, RestApiCapability};
use tower::ServiceExt;
use uuid::Uuid;

use super::{BACKEND_STOP_GRACE, EventBrokerModule, Supervisor, start_backends, stop_backends};
use crate::ExternallyManagedDirectoryPresence;
use crate::config::{DeploymentMode, EventBrokerConfig, RegistrationConfig};
use crate::domain::cluster::EventBrokerCluster;
use crate::test_support::gear_db::ScratchSqlite;
use crate::test_support::module_boot::StaticConfigProvider;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, LogClaim, Provisioning, Role,
    TopicPartition, TopicSpec, Wait, Watch,
};
use event_broker_sdk::models::Event;
use event_broker_sdk::{PartitionRange, Position, Sequence, TopicSegment};

#[test]
fn the_broker_publishes_exactly_its_two_sdk_base_schemas() -> anyhow::Result<()> {
    let descriptors: Vec<_> = toolkit::gts_publication::linked_descriptors()
        .into_iter()
        .filter(|descriptor| descriptor.gear == "event-broker")
        .collect();
    let [descriptor] = descriptors.as_slice() else {
        anyhow::bail!("expected one event-broker publication descriptor, found {descriptors:?}");
    };
    let manifest = descriptor.build_gts_manifest(&StaticConfigProvider { root: json!({}) })?;
    assert_eq!(manifest.publisher_name(), "event-broker");
    assert_eq!(manifest.publisher_version(), env!("CARGO_PKG_VERSION"));
    let documents: Vec<_> = manifest
        .documents()
        .iter()
        .map(|(id, schema)| (id.id(), schema.clone()))
        .collect();
    assert_eq!(
        documents,
        [
            (
                "gts.cf.core.events.event.v1~",
                serde_json::from_str(include_str!(
                    "../../docs/schemas/gts.cf.core.events.event.v1~.schema.json"
                ))?,
            ),
            (
                "gts.cf.core.events.topic.v1~",
                serde_json::from_str(include_str!(
                    "../../docs/schemas/gts.cf.core.events.topic.v1~.schema.json"
                ))?,
            ),
        ]
    );
    Ok(())
}

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

/// A fresh temp-file `SQLite` gear database with this crate's migrations
/// applied, for `init()` to resolve (`ctx.db_required()`) and `serve()` to
/// load the specification cache into.
async fn test_db() -> ScratchSqlite {
    ScratchSqlite::open("cf-eb-module-test").await
}

async fn make_ctx(mode: &str) -> (GearCtx, ScratchSqlite) {
    make_ctx_with_hub(mode, Arc::new(ClientHub::new())).await
}

/// A context for `mode` over `hub`, wired as
/// [`module_ctx`](crate::test_support::module_boot::module_ctx) wires one, with
/// a freshly-migrated temp-file `SQLite` gear database attached, and that
/// database.
async fn make_ctx_with_hub(mode: &str, hub: Arc<ClientHub>) -> (GearCtx, ScratchSqlite) {
    let scratch = test_db().await;
    let ctx =
        make_ctx_with_hub_and_config(mode, hub, &json!({}), scratch.db().as_ref().clone()).await;
    (ctx, scratch)
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
    let (ctx, _scratch) = make_ctx("not_a_real_mode").await;

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

fn directory_presence_context(config: &serde_json::Value, hub: Arc<ClientHub>) -> GearCtx {
    GearCtx::new(
        EventBrokerModule::MODULE_NAME,
        Uuid::from_u128(1),
        Arc::new(StaticConfigProvider {
            root: json!({ "event-broker": { "config": config } }),
        }),
        hub,
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn a_managed_role_reaches_the_database_check_with_the_default_registration_address() {
    let missing_database = "Database is not configured for gear 'event-broker'";
    let wildcard_address = "cannot register with DirectoryService: bound to a wildcard address (0.0.0.0:0) with no `registration.advertise_addr` configured - set one to a routable host";

    for (mode, embedded_data_role) in [
        ("cluster_ingest", true),
        ("cluster_delivery", true),
        ("standalone", false),
        ("cluster_dispatcher", false),
    ] {
        for (local_marker, proxy_marker, invalid_address) in [
            (true, false, false),
            (true, false, true),
            (false, false, false),
            (false, true, false),
        ] {
            let hub = Arc::new(ClientHub::new());
            if local_marker {
                hub.register(Arc::new(ExternallyManagedDirectoryPresence));
            }
            if proxy_marker {
                hub.register_remote_proxy(Arc::new(ExternallyManagedDirectoryPresence));
            }
            let mut config = json!({
                "mode": mode,
                "backends": { "main": { "type": "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~" } },
                "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
            });
            if invalid_address {
                config["registration"] = json!({ "listen_addr": "invalid-address" });
            }
            let ctx = directory_presence_context(&config, hub);
            let error = EventBrokerModule::default()
                .init(&ctx)
                .await
                .expect_err("init stops at the address preflight or the database check");
            let expected = if embedded_data_role && !local_marker {
                wildcard_address
            } else {
                missing_database
            };
            assert_eq!(
                error.to_string(),
                expected,
                "{mode}, local marker: {local_marker}, proxy marker: {proxy_marker}, malformed address: {invalid_address}"
            );
        }
    }

    let hub = Arc::new(ClientHub::new());
    hub.register(Arc::new(ExternallyManagedDirectoryPresence));
    let ctx = directory_presence_context(
        &json!({
            "mode": "cluster_ingest",
            "backends": { "main": { "type": "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~" } },
            "backend_routing": {},
        }),
        hub,
    );
    let error = EventBrokerModule::default()
        .init(&ctx)
        .await
        .expect_err("C4 stops a managed role at startup");
    assert_eq!(
        error.to_string(),
        "backend_routing: there is no route for `gts.cf.core.events.topic.v1~`; it is required so that every topic resolves to a registry entry (feature 0005 section 2.4, C4)"
    );
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DirectoryCall {
    Register {
        gear: String,
        instance_id: String,
        grpc_services: Vec<(String, ServiceEndpoint)>,
        version: Option<String>,
        rest_endpoint: Option<ServiceEndpoint>,
        labels: BTreeMap<String, String>,
    },
    Heartbeat(String, String),
    Deregister(String, String),
}

#[derive(Default)]
struct RecordingDirectory {
    journal: Mutex<Vec<DirectoryCall>>,
    heartbeat: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl DirectoryClient for RecordingDirectory {
    async fn resolve_grpc_service(&self, _: &str) -> anyhow::Result<ServiceEndpoint> {
        panic!("presence never resolves gRPC services")
    }

    async fn resolve_rest_service(&self, _: &str) -> anyhow::Result<ServiceEndpoint> {
        panic!("presence never resolves REST services")
    }

    async fn list_instances(&self, _: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        panic!("presence never lists a gear's instances")
    }

    async fn list_all_instances(&self) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        panic!("presence never lists all instances")
    }

    async fn register_instance(&self, info: RegisterInstanceInfo) -> anyhow::Result<()> {
        self.journal
            .lock()
            .expect("directory journal")
            .push(DirectoryCall::Register {
                gear: info.gear,
                instance_id: info.instance_id,
                grpc_services: info.grpc_services,
                version: info.version,
                rest_endpoint: info.rest_endpoint,
                labels: info.labels,
            });
        Ok(())
    }

    async fn send_heartbeat(&self, gear: &str, instance_id: &str) -> anyhow::Result<()> {
        self.journal
            .lock()
            .expect("directory journal")
            .push(DirectoryCall::Heartbeat(
                gear.to_owned(),
                instance_id.to_owned(),
            ));
        self.heartbeat.notify_one();
        Ok(())
    }

    async fn deregister_instance(&self, gear: &str, instance_id: &str) -> anyhow::Result<()> {
        self.journal
            .lock()
            .expect("directory journal")
            .push(DirectoryCall::Deregister(
                gear.to_owned(),
                instance_id.to_owned(),
            ));
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn embedded_roles_register_heartbeat_and_deregister_with_the_same_identity() {
    let mut prior_instance_id = None;
    for (mode, gear, proxy_marker) in [
        (DeploymentMode::ClusterIngest, "event-broker-ingest", false),
        (
            DeploymentMode::ClusterDelivery,
            "event-broker-delivery",
            true,
        ),
    ] {
        let directory = Arc::new(RecordingDirectory::default());
        let hub = Arc::new(ClientHub::new());
        hub.register::<dyn DirectoryClient>(directory.clone());
        if proxy_marker {
            hub.register_remote_proxy(Arc::new(ExternallyManagedDirectoryPresence));
        }
        let ctx = directory_presence_context(&json!({}), hub);
        let module = EventBrokerModule {
            client_hub: ctx.client_hub().into(),
            registration: RegistrationConfig {
                listen_addr: "127.0.0.1:8080".to_owned(),
                advertise_addr: None,
            }
            .into(),
            ..EventBrokerModule::default()
        };
        let cancel = CancellationToken::new();
        let mut supervisor = Supervisor::new(cancel.child_token());
        let epoch = tokio::time::Instant::now();

        tokio::time::timeout(
            BOUND,
            module.start_directory_presence(&mut supervisor, mode),
        )
        .await
        .expect("presence startup is bounded")
        .expect("embedded registration succeeds");
        let initial_calls = directory.journal.lock().expect("directory journal").clone();
        let [DirectoryCall::Register { instance_id, .. }] = initial_calls.as_slice() else {
            panic!("{mode:?} registers once on entry: {initial_calls:?}");
        };
        let id = Uuid::parse_str(instance_id).expect("the registration carries a UUID");
        assert_eq!(id.get_version(), Some(uuid::Version::Random));
        assert_ne!(Some(id), prior_instance_id);
        prior_instance_id = Some(id);
        let registration = DirectoryCall::Register {
            gear: gear.to_owned(),
            instance_id: instance_id.clone(),
            grpc_services: vec![],
            version: None,
            rest_endpoint: Some(ServiceEndpoint {
                uri: "http://127.0.0.1:8080".to_owned(),
            }),
            // Embedded registration omits the planned `evbk-affinity` label (#5552).
            labels: BTreeMap::new(),
        };
        assert_eq!(
            initial_calls,
            std::slice::from_ref(&registration),
            "{mode:?}"
        );

        // On the paused clock, time advances only after the worker has parked
        // and the timer driver has serviced its immediate tick.
        tokio::time::sleep(Duration::from_secs(14)).await;
        assert_eq!(
            *directory.journal.lock().expect("directory journal"),
            std::slice::from_ref(&registration),
            "{mode:?}"
        );

        let heartbeat_period = Duration::from_secs(15);
        let observed = tokio::time::timeout_at(
            epoch + heartbeat_period + BOUND,
            directory.heartbeat.notified(),
        )
        .await;
        let heartbeat = DirectoryCall::Heartbeat(gear.to_owned(), instance_id.clone());
        assert_eq!(
            *directory.journal.lock().expect("directory journal"),
            [registration.clone(), heartbeat.clone()],
            "{mode:?}"
        );
        observed.expect("the first heartbeat notifies its observer");
        assert!(
            (heartbeat_period..heartbeat_period + Duration::from_millis(1))
                .contains(&epoch.elapsed())
        );

        cancel.cancel();
        tokio::time::timeout(BOUND, supervisor.supervise())
            .await
            .expect("presence shutdown is bounded")
            .expect("presence shuts down cleanly");
        assert_eq!(
            *directory.journal.lock().expect("directory journal"),
            [
                registration,
                heartbeat,
                DirectoryCall::Deregister(gear.to_owned(), instance_id.clone())
            ],
            "{mode:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn nonregistering_roles_keep_a_supervised_cancel_gate_and_make_no_directory_calls() {
    for (mode, marked, with_directory) in [
        (DeploymentMode::ClusterIngest, true, true),
        (DeploymentMode::ClusterDelivery, true, true),
        (DeploymentMode::Standalone, false, true),
        (DeploymentMode::Standalone, true, true),
        (DeploymentMode::ClusterDispatcher, false, true),
        (DeploymentMode::ClusterDispatcher, true, true),
        (DeploymentMode::ClusterIngest, true, false),
    ] {
        let directory = Arc::new(RecordingDirectory::default());
        let hub = Arc::new(ClientHub::new());
        if marked {
            hub.register(Arc::new(ExternallyManagedDirectoryPresence));
        }
        if with_directory {
            hub.register::<dyn DirectoryClient>(directory.clone());
        }
        let ctx = directory_presence_context(&json!({}), hub);
        let mut registration = RegistrationConfig::default();
        if with_directory {
            registration.listen_addr = "127.0.0.1:8080".to_owned();
        }
        let module = EventBrokerModule {
            client_hub: ctx.client_hub().into(),
            registration: registration.into(),
            ..EventBrokerModule::default()
        };
        let cancel = CancellationToken::new();
        let mut supervisor = Supervisor::new(cancel.child_token());
        tokio::time::timeout(
            BOUND,
            module.start_directory_presence(&mut supervisor, mode),
        )
        .await
        .expect("presence startup is bounded")
        .expect("the cancel gate needs no legacy address or directory");
        assert_eq!(
            *directory.journal.lock().expect("directory journal"),
            [],
            "{mode:?}, marker: {marked}"
        );
        let workers = supervisor.tasks.len();
        let mut supervised = std::pin::pin!(supervisor.supervise());
        tokio::select! {
            biased;
            result = &mut supervised => panic!("{mode:?}, marker: {marked}: supervisor completed before cancellation: {result:?}"),
            () = tokio::time::sleep(Duration::from_secs(16)) => {}
        }
        assert_eq!(workers, 1, "{mode:?}, marker: {marked}");
        assert_eq!(
            *directory.journal.lock().expect("directory journal"),
            [],
            "{mode:?}, marker: {marked}"
        );

        cancel.cancel();
        tokio::time::timeout(BOUND, supervised)
            .await
            .expect("cancel gate shutdown is bounded")
            .expect("cancel gate shuts down cleanly");
        assert_eq!(
            *directory.journal.lock().expect("directory journal"),
            [],
            "{mode:?}, marker: {marked}"
        );
    }
}

#[test]
fn dispatcher_still_requires_its_directory_for_routes_when_presence_is_managed() {
    let hub = Arc::new(ClientHub::new());
    hub.register(Arc::new(ExternallyManagedDirectoryPresence));
    let ctx = directory_presence_context(&json!({}), hub.clone());
    let module = EventBrokerModule {
        mode: DeploymentMode::ClusterDispatcher.into(),
        ..EventBrokerModule::default()
    };

    let Err(error) = module.register_rest(&ctx, Router::new(), &NoopOpenApiRegistry) else {
        panic!("dispatcher route registration requires a directory client");
    };
    let Some(ClientHubError::NotFound { type_key }) = error.downcast_ref::<ClientHubError>() else {
        panic!("expected a missing ClientHub dependency, found {error:#}");
    };
    assert_eq!(
        format!("{type_key:?}"),
        std::any::type_name::<dyn DirectoryClient>()
    );

    let directory = Arc::new(RecordingDirectory::default());
    hub.register::<dyn DirectoryClient>(directory.clone());
    let _router = module
        .register_rest(&ctx, Router::new(), &NoopOpenApiRegistry)
        .expect("dispatcher routes build with a directory client");
    assert_eq!(*directory.journal.lock().expect("directory journal"), []);
}

#[test]
fn the_eb_profile_is_declared_for_cluster_wiring() {
    let profiles = cluster_sdk::registered_profiles();
    assert!(profiles.contains(&"event-broker"), "{profiles:?}");
}

#[tokio::test]
async fn a_readiness_check_collected_before_init_reports_starting() {
    let ctx = GearCtx::new(
        EventBrokerModule::MODULE_NAME,
        Uuid::from_u128(0x5eed),
        Arc::new(StaticConfigProvider { root: json!({}) }),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    );
    let module = EventBrokerModule::default();
    let readiness = module
        .healthcheck(&ctx)
        .expect("the gear reports readiness");

    let result = readiness.check().await;
    assert_eq!(
        (
            readiness.name(),
            result.status,
            result.message.as_deref(),
            result.code.as_deref()
        ),
        (
            "event-broker-readiness",
            HealthcheckStatus::Unhealthy,
            Some("event-broker init has not run"),
            Some("starting")
        )
    );
}

struct ReadinessCluster {
    client: Arc<dyn ClusterClient>,
    degraded: AtomicBool,
    refreshes: AtomicUsize,
}

#[async_trait::async_trait]
impl ClusterClient for ReadinessCluster {
    fn cache_backend(&self, profile: &str) -> Result<Arc<dyn ClusterCacheBackend>, ClusterError> {
        self.client.cache_backend(profile)
    }

    fn lock_backend(&self, profile: &str) -> Result<Arc<dyn DistributedLockBackend>, ClusterError> {
        self.client.lock_backend(profile)
    }

    fn leader_election_backend(
        &self,
        profile: &str,
    ) -> Result<Arc<dyn LeaderElectionBackend>, ClusterError> {
        self.client.leader_election_backend(profile)
    }

    async fn descriptor(&self, profile: &str) -> Result<ProfileDescriptor, ClusterError> {
        let mut descriptor = self.client.descriptor(profile).await?;
        if self.degraded.load(Ordering::Relaxed) {
            descriptor.health = ProfileHealth::Degraded;
        }
        Ok(descriptor)
    }

    async fn refresh_descriptors(&self) -> Result<(), ClusterError> {
        self.refreshes.fetch_add(1, Ordering::Relaxed);
        self.client.refresh_descriptors().await
    }
}

#[tokio::test]
async fn the_broker_preserves_storage_precedence_and_reports_cluster_health() {
    let hub = Arc::new(ClientHub::new());
    crate::test_support::register_standalone_cluster_stub(&hub).await;
    let cluster = Arc::new(ReadinessCluster {
        client: hub
            .get::<dyn ClusterClient>()
            .expect("the fixture binds the cluster"),
        degraded: AtomicBool::new(false),
        refreshes: AtomicUsize::new(0),
    });
    hub.register::<dyn ClusterClient>(cluster.clone());
    let (ctx, _db) = make_ctx_with_hub("standalone", hub).await;
    let standalone = EventBrokerModule::default();
    standalone.init(&ctx).await.expect("standalone initializes");

    // Cluster delivery requires a non-SQLite gear database. Reuse the initialized
    // storage to exercise the delivery readiness path without starting a database server.
    let delivery = EventBrokerModule {
        mode: OnceLock::from(DeploymentMode::ClusterDelivery),
        client_hub: OnceLock::from(ctx.client_hub()),
        storage: standalone.storage.clone(),
        ..EventBrokerModule::default()
    };
    let standalone_readiness = standalone
        .healthcheck(&ctx)
        .expect("standalone reports readiness");
    let delivery_readiness = delivery
        .healthcheck(&ctx)
        .expect("delivery reports readiness");
    EventBrokerCluster::resolve(&ctx.client_hub())
        .await
        .expect("the serving profile binds and records the broker requirements");
    cluster.degraded.store(true, Ordering::Relaxed);

    let result = standalone_readiness.check().await;
    assert_eq!(
        (
            standalone_readiness.name(),
            result.status,
            result.message.as_deref(),
            result.code.as_deref()
        ),
        (
            "event-broker-readiness",
            HealthcheckStatus::Unhealthy,
            Some("ingest outbox pipeline not started"),
            Some("starting")
        )
    );
    let result = delivery_readiness.check().await;
    assert_eq!(
        (
            delivery_readiness.name(),
            result.status,
            result.message.as_deref(),
            result.code.as_deref()
        ),
        (
            "event-broker-readiness",
            HealthcheckStatus::Unhealthy,
            Some("cluster cache not wired"),
            Some("starting")
        )
    );
    assert_eq!(cluster.refreshes.load(Ordering::Relaxed), 0);
    delivery
        .wire_cluster_cache()
        .await
        .expect("delivery wires its cache");

    tokio::time::pause();
    for _ in 0..2 {
        let result = delivery_readiness.check().await;
        assert_eq!(
            (
                result.status,
                result.message.as_deref(),
                result.code.as_deref()
            ),
            (
                HealthcheckStatus::Unhealthy,
                Some("profile `event-broker` is degraded server-side"),
                Some("cluster-profile-degraded")
            )
        );
        assert_eq!(cluster.refreshes.load(Ordering::Relaxed), 1);
    }

    cluster.degraded.store(false, Ordering::Relaxed);
    tokio::time::advance(cluster_sdk::requirements::DESCRIPTOR_REFRESH_INTERVAL).await;
    let result = delivery_readiness.check().await;
    assert_eq!(
        (
            result.status,
            result.message.as_deref(),
            result.code.as_deref()
        ),
        (HealthcheckStatus::Healthy, None, None)
    );
    assert_eq!(cluster.refreshes.load(Ordering::Relaxed), 2);
}

/// The readiness `/readyz` reports: `Starting` until `serve()` has wired the
/// outbox and the cluster cache, then ready for as long as it serves, and
/// `serve()` returns promptly on a stop.
#[tokio::test]
async fn standalone_turns_ready_once_serve_has_wired_it() {
    let (ctx, _scratch) = make_ctx("standalone").await;
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

/// De-risks production route registration itself:
/// `register_ingest_routes`/`register_delivery_routes` register 19
/// `OperationBuilder` operations, three of them (`GET`/`DELETE`/`POST` on
/// `/v1/subscriptions/{id}`) sharing one literal path string across
/// separate `.register()` calls. axum's `path_router` merges method routers
/// registered against the *same* path string
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
    let (ctx, _scratch) = make_ctx_with_hub("standalone", hub).await;
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
/// database attached, for `init` tests that stop at a startup check; and that
/// database.
async fn ctx_with_config(config: serde_json::Value) -> (GearCtx, ScratchSqlite) {
    let scratch = test_db().await;
    let ctx = GearCtx::new(
        EventBrokerModule::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(StaticConfigProvider {
            root: json!({ "event-broker": { "config": config } }),
        }),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    )
    .with_db(scratch.db().as_ref().clone());
    (ctx, scratch)
}

/// The startup checks run before anything is wired, and a failing one stops
/// `init` with its own message (feature 0005 section 2.4, C4).
#[tokio::test]
async fn init_fails_on_a_configuration_breaking_a_startup_check() {
    let (ctx, _scratch) = ctx_with_config(json!({
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
    let (ctx, _scratch) = ctx_with_config(json!({
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
        let (ctx, _scratch) = make_ctx(mode).await;
        let err = EventBrokerModule::default()
            .init(&ctx)
            .await
            .expect_err("C10 stops the start");
        assert_eq!(
            err.to_string(),
            "database: the gear's database is SQLite; a SQLite database serves a single process, so only `standalone` mode runs on one (feature 0005 section 2.4, C10)",
            "{mode}"
        );
    }

    let (ctx, _scratch) = make_ctx("standalone").await;
    EventBrokerModule::default()
        .init(&ctx)
        .await
        .expect("standalone runs on a SQLite gear database");
}

/// `serve()` registers the `default` outbox group's queue with the count
/// `outbox_groups.default.partitions` configures, the same count `init()`
/// hands the ingest service.
#[tokio::test]
async fn serve_registers_the_default_outbox_group_with_its_configured_partitions() {
    let scratch = test_db().await;
    let dsn = scratch.dsn().to_owned();
    let ctx = make_ctx_with_hub_and_config(
        "standalone",
        Arc::new(ClientHub::new()),
        &json!({ "outbox_groups": { "default": { "partitions": 4 } } }),
        scratch.db().as_ref().clone(),
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
    let scratch = test_db().await;
    let ctx = make_ctx_with_hub_and_config(
        "standalone",
        Arc::new(ClientHub::new()),
        &sections,
        scratch.db().as_ref().clone(),
    )
    .await;
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
            uuid: Uuid::from_u128(0x7012),
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
    let (ctx, _scratch) = make_ctx_with_hub("standalone", Arc::clone(&hub)).await;
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
