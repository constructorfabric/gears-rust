//! Gear wiring: storage selection (ADR-0018 *Backend Selection*), restart
//! survival on a database, and registry provisioning across boots of a gear on
//! a database. The reconcile itself is tested in `registry_reconcile_tests.rs`.
//!
//! Each test runs `Gear::init` over a real `GearCtx`. The `ClientHub` holds
//! the four clients init resolves; the database, when there is one, is a
//! migrated `SQLite` file, as the runtime's DB phase would leave it.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use credstore_sdk::CredStoreClientV1;
use oagw_sdk::api::ServiceGatewayClientV1;
use serde_json::{Value, json};
use tenant_resolver_sdk::{TenantId, TenantResolverClient};
use toolkit::runtime::{GearManager, GrpcInstallerStore, SystemContext};
use toolkit::{ClientHub, ConfigProvider, GearCtx};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::{DBProvider, Db};
use tracing_test::traced_test;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};
use types_registry_sdk::{
    GtsInstance, GtsTypeSchema, InstanceQuery, RegisterResult, TypeSchemaQuery, TypesRegistryClient,
};
use uuid::Uuid;

use super::*;
use crate::domain::model::ManagedBy;
use crate::domain::services::registry_reconcile::tests::{
    CREATED_ALL, NONE, UNCHANGED_ALL, registry_instances, set_path, sibling_route,
};
use crate::domain::services::registry_reconcile::{
    ProvisioningCounts, Tally, Unattended, provisioning_ctx, reconcile_registry,
};
use crate::domain::test_support::{
    CapturingAuthZResolverClient, MockCredStoreClient, MockTenantResolverClient,
    ProvisioningMetrics,
};
use crate::domain::type_provisioning::{ProvisionedRoute, ProvisionedUpstream};
use crate::infra::storage::testing::SqliteFile;
use toolkit::contracts::RunnableCapability;

const NO_HANDLE_MESSAGE: &str = "gears.oagw.database is configured but no database is available";
const MEMORY_WARNING: &str = "upstreams and routes are kept in memory and lost on restart";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A config provider answering the gear from one `gears.oagw` section.
struct FixedConfig(Value);

impl ConfigProvider for FixedConfig {
    fn get_gear_config(&self, gear: &str) -> Option<&Value> {
        (gear == OutboundApiGatewayGear::MODULE_NAME).then_some(&self.0)
    }
}

/// Types registry that accepts every registration, which is all `init` asks
/// of it. The SDK mock refuses non-empty registrations and ignores a listing's
/// pattern; every other call goes to it.
struct AcceptingTypesRegistry(MockTypesRegistryClient);

#[async_trait]
impl TypesRegistryClient for AcceptingTypesRegistry {
    async fn register(&self, entities: Vec<Value>) -> Result<Vec<RegisterResult>, CanonicalError> {
        Ok(entities
            .iter()
            .map(|e| RegisterResult::Ok {
                gts_id: e["$id"].as_str().unwrap_or_default().to_owned(),
            })
            .collect())
    }
    async fn register_type_schemas(
        &self,
        type_schemas: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.0.register_type_schemas(type_schemas).await
    }
    async fn get_type_schema(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        self.0.get_type_schema(type_id).await
    }
    async fn get_type_schema_by_uuid(
        &self,
        type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        self.0.get_type_schema_by_uuid(type_uuid).await
    }
    async fn get_type_schemas(
        &self,
        type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        self.0.get_type_schemas(type_ids).await
    }
    async fn get_type_schemas_by_uuid(
        &self,
        type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        self.0.get_type_schemas_by_uuid(type_uuids).await
    }
    async fn list_type_schemas(
        &self,
        query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        self.0.list_type_schemas(query).await
    }
    async fn register_instances(
        &self,
        instances: Vec<Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.0.register_instances(instances).await
    }
    async fn get_instance(&self, id: &str) -> Result<GtsInstance, CanonicalError> {
        self.0.get_instance(id).await
    }
    async fn get_instance_by_uuid(&self, uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        self.0.get_instance_by_uuid(uuid).await
    }
    async fn get_instances(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        self.0.get_instances(ids).await
    }
    async fn get_instances_by_uuid(
        &self,
        uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        self.0.get_instances_by_uuid(uuids).await
    }
    async fn list_instances(
        &self,
        query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        // Only the trailing-`*` prefix patterns OAGW lists with.
        let prefix = query
            .pattern
            .as_deref()
            .map(|p| p.trim_end_matches('*').to_owned());
        let mut instances = self.0.list_instances(query).await?;
        if let Some(prefix) = prefix {
            instances.retain(|i| i.id.as_ref().starts_with(&prefix));
        }
        Ok(instances)
    }
}

/// A fresh hub holding the clients init resolves.
fn wired_hub() -> Arc<ClientHub> {
    hub_over(
        MockTenantResolverClient::single_tenant(),
        MockTypesRegistryClient::new(),
    )
}

/// A fresh hub holding the clients init resolves, with these tenants and
/// types-registry content.
fn hub_over(
    tenants: MockTenantResolverClient,
    registry: MockTypesRegistryClient,
) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TenantResolverClient>(Arc::new(tenants));
    hub.register::<dyn CredStoreClientV1>(Arc::new(MockCredStoreClient::empty()));
    hub.register::<dyn AuthZResolverApi>(Arc::new(CapturingAuthZResolverClient::new()));
    hub.register::<dyn TypesRegistryClient>(Arc::new(AcceptingTypesRegistry(registry)));
    hub
}

/// The `gears.oagw` section with the README's opt-in snippet. With a handle
/// from the DB phase, its content no longer matters to the gear.
fn database_section() -> Value {
    json!({
        "config": {},
        "database": { "server": "sqlite_users", "file": "oagw.db" },
    })
}

/// A context over `section` (the whole `gears.oagw` value) and `hub`, with the
/// handle the runtime's DB phase would attach, if any. Also installs the
/// crypto provider the runtime bootstrap would have (`init` builds the
/// upstream engine, whose TLS config fails closed without one under `fips`).
fn context(section: Value, db: Option<Db>, hub: Arc<ClientHub>) -> GearCtx {
    crate::domain::test_support::ensure_crypto_provider();
    let ctx = GearCtx::new(
        OutboundApiGatewayGear::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(FixedConfig(section)),
        hub,
        Default::default(),
    );
    match db {
        Some(db) => ctx.with_db(DBProvider::new(db)),
        None => ctx,
    }
}

async fn init(ctx: &GearCtx) -> anyhow::Result<OutboundApiGatewayGear> {
    let gear = OutboundApiGatewayGear::default();
    gear.init(ctx).await?;
    Ok(gear)
}

/// The Control Plane `init` built.
fn control_plane(gear: &OutboundApiGatewayGear) -> Arc<dyn ControlPlaneService> {
    gear.state
        .load_full()
        .expect("init stores the state")
        .cp
        .clone()
}

fn assert_warned_once_about_memory(lines: &[&str]) -> Result<(), String> {
    let warnings = lines
        .iter()
        .filter(|l| l.contains("WARN") && l.contains(MEMORY_WARNING))
        .count();
    if warnings != 1 {
        return Err(format!("expected one in-memory warning, got {warnings}"));
    }
    if lines.iter().any(|l| l.contains("OAGW storage: database")) {
        return Err("a database was reported without one".to_owned());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Storage selection
// ---------------------------------------------------------------------------

/// ADR-0018: PERS-BOOT-001.
#[tokio::test]
#[traced_test]
async fn without_database_config_init_keeps_memory_storage_and_warns() {
    let ctx = context(json!({ "config": {} }), None, wired_hub());
    init(&ctx).await.expect("init without a database");
    logs_assert(assert_warned_once_about_memory);
}

/// `AuthZ` is a consumed contract, wired after `init` (locally or remotely), so
/// `init` must not look it up.
#[tokio::test]
async fn init_does_not_need_authz_in_the_hub() {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TenantResolverClient>(Arc::new(MockTenantResolverClient::single_tenant()));
    hub.register::<dyn CredStoreClientV1>(Arc::new(MockCredStoreClient::empty()));
    hub.register::<dyn TypesRegistryClient>(Arc::new(AcceptingTypesRegistry(
        MockTypesRegistryClient::new(),
    )));
    init(&context(json!({ "config": {} }), None, hub))
        .await
        .expect("init without AuthZ in the hub");
}

/// ADR-0018: PERS-BOOT-002.
#[tokio::test]
async fn database_section_without_a_handle_fails_init() {
    // Without a global `database:` section the runtime builds no database
    // manager, so the gear sees its key but no handle.
    let section = json!({ "config": {}, "database": { "bogus": 1 } });
    let Err(err) = init(&context(section, None, wired_hub())).await else {
        panic!("init must refuse to fall back to memory");
    };
    assert!(err.to_string().contains(NO_HANDLE_MESSAGE), "got `{err}`");
}

/// ADR-0018: PERS-BOOT-003.
#[tokio::test]
#[traced_test]
async fn null_database_counts_as_absent() {
    let section = json!({ "config": {}, "database": null });
    init(&context(section, None, wired_hub()))
        .await
        .expect("`database: null` is no database");
    logs_assert(assert_warned_once_about_memory);
}

// ---------------------------------------------------------------------------
// Registry provisioning in post_init
// ---------------------------------------------------------------------------

/// `post_init` reconciles the types-registry instances, and an instance that
/// names no tenant lands in the root tenant, registry-managed.
#[tokio::test]
async fn post_init_provisions_registry_instances_into_the_root_tenant() {
    let root = Uuid::new_v4();
    let upstream_id = Uuid::new_v4();
    let route_id = Uuid::new_v4();
    let registry = MockTypesRegistryClient::new().with_instances([
        make_test_instance(
            &format!("{}{upstream_id}", oagw_sdk::gts::UPSTREAM_SCHEMA),
            json!({
                "server": {
                    "endpoints": [{"host": "api.example.com", "port": 443, "scheme": "https"}]
                },
                "protocol": oagw_sdk::HTTP_PROTOCOL_ID,
                "enabled": true,
                "tags": [],
            }),
        ),
        make_test_instance(
            &format!("{}{route_id}", oagw_sdk::gts::ROUTE_SCHEMA),
            json!({
                "upstream_id": upstream_id,
                "match": { "http": { "methods": ["GET"], "path": "/v1" } },
                "enabled": true,
                "tags": [],
                "priority": 0,
            }),
        ),
    ]);
    let hub = hub_over(
        MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
        registry,
    );
    let gear = init(&context(json!({ "config": {} }), None, hub))
        .await
        .expect("init");
    gear.post_init(&SystemContext::new(
        Uuid::new_v4(),
        Arc::new(GearManager::new()),
        Arc::new(GrpcInstallerStore::new()),
    ))
    .await
    .expect("post_init");

    let cp = control_plane(&gear);
    let ctx = provisioning_ctx(root).unwrap();
    let upstream = cp
        .get_upstream(&ctx, upstream_id)
        .await
        .expect("upstream in the root tenant");
    assert_eq!(
        (upstream.tenant_id, upstream.managed_by),
        (root, ManagedBy::Registry)
    );
    let route = cp
        .get_route(&ctx, route_id)
        .await
        .expect("route in the root tenant");
    assert_eq!(
        (route.upstream_id, route.managed_by),
        (upstream_id, ManagedBy::Registry)
    );
}

/// A registry of one upstream with one route.
fn one_upstream_registry(upstream_id: Uuid, route_id: Uuid) -> MockTypesRegistryClient {
    MockTypesRegistryClient::new().with_instances([
        make_test_instance(
            &format!("{}{upstream_id}", oagw_sdk::gts::UPSTREAM_SCHEMA),
            json!({
                "server": {
                    "endpoints": [{"host": "api.example.com", "port": 443, "scheme": "https"}]
                },
                "protocol": oagw_sdk::HTTP_PROTOCOL_ID,
                "enabled": true,
                "tags": [],
            }),
        ),
        make_test_instance(
            &format!("{}{route_id}", oagw_sdk::gts::ROUTE_SCHEMA),
            json!({
                "upstream_id": upstream_id,
                "match": { "http": { "methods": ["GET"], "path": "/v1" } },
                "enabled": true,
                "tags": [],
                "priority": 0,
            }),
        ),
    ])
}

fn system_context() -> SystemContext {
    SystemContext::new(
        Uuid::new_v4(),
        Arc::new(GearManager::new()),
        Arc::new(GrpcInstallerStore::new()),
    )
}

async fn health(gear: &OutboundApiGatewayGear, ctx: &GearCtx) -> toolkit::HealthcheckStatus {
    health_result(gear, ctx).await.status
}

/// The stable code of the registry provisioning healthcheck, if any.
async fn health_code(gear: &OutboundApiGatewayGear, ctx: &GearCtx) -> Option<String> {
    health_result(gear, ctx).await.code
}

async fn health_result(gear: &OutboundApiGatewayGear, ctx: &GearCtx) -> toolkit::HealthcheckResult {
    gear.healthcheck(ctx)
        .expect("OAGW reports registry provisioning")
        .check()
        .await
}

/// On a database, `post_init` leaves the reconcile to the background task
/// `start` spawns, which returns at once; the healthcheck says Degraded
/// (never Unhealthy) until the first run settles, then Healthy; `stop` ends
/// the task.
///
/// ADR-0018: PERS-LOOP-007, PERS-LOOP-008.
#[tokio::test]
async fn on_a_database_start_reconciles_in_the_background() {
    use toolkit::HealthcheckStatus;

    let file = SqliteFile::new();
    let root = Uuid::new_v4();
    let (upstream_id, route_id) = (Uuid::new_v4(), Uuid::new_v4());
    let hub = hub_over(
        MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
        one_upstream_registry(upstream_id, route_id),
    );
    let ctx = context(database_section(), Some(file.open().await), hub);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&system_context()).await.expect("post_init");

    let cp = control_plane(&gear);
    let pctx = provisioning_ctx(root).unwrap();
    assert!(
        cp.get_upstream(&pctx, upstream_id).await.is_err(),
        "post_init does not reconcile on a database"
    );
    assert_eq!(health(&gear, &ctx).await, HealthcheckStatus::Degraded);

    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        gear.start(tokio_util::sync::CancellationToken::new()),
    )
    .await
    .expect("start returns at once")
    .expect("start");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while cp.get_route(&pctx, route_id).await.is_err() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the task reconciles"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    while health(&gear, &ctx).await != HealthcheckStatus::Healthy {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the first run settles"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        gear.stop(tokio_util::sync::CancellationToken::new()),
    )
    .await
    .expect("stop ends the task")
    .expect("stop");
    assert!(gear.registry_task.lock().is_none());
}

/// A registry lease another pod holds never delays startup: `post_init`
/// and `start` return, the pod reports Degraded, and `stop` ends the
/// waiting task.
///
/// ADR-0018: PERS-LOOP-008.
#[tokio::test]
async fn a_held_lease_delays_neither_startup_nor_shutdown() {
    use crate::domain::repo::RegistryStateStore;
    use toolkit::HealthcheckStatus;

    let file = SqliteFile::new();
    let db = file.open().await;
    let state = crate::infra::storage::DbRegistryStateStore::new(DBProvider::new(db.clone()));
    assert!(
        state
            .try_acquire(Uuid::new_v4(), std::time::Duration::from_secs(600), None)
            .await
            .unwrap()
    );
    let root = Uuid::new_v4();
    let upstream_id = Uuid::new_v4();
    let hub = hub_over(
        MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
        one_upstream_registry(upstream_id, Uuid::new_v4()),
    );
    let ctx = context(database_section(), Some(db), hub);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&system_context()).await.expect("post_init");
    gear.start(tokio_util::sync::CancellationToken::new())
        .await
        .expect("start");
    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    assert_eq!(health(&gear, &ctx).await, HealthcheckStatus::Degraded);
    assert_eq!(
        health_code(&gear, &ctx).await.as_deref(),
        Some("registry_provisioning_pending"),
        "waiting on the lease, not failed"
    );
    assert!(
        control_plane(&gear)
            .get_upstream(&provisioning_ctx(root).unwrap(), upstream_id)
            .await
            .is_err(),
        "nothing is written while another pod holds the lease"
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        gear.stop(tokio_util::sync::CancellationToken::new()),
    )
    .await
    .expect("stop ends the waiting task")
    .expect("stop");
}

/// In memory there is nothing to wait for: `post_init` reconciles before
/// the pod serves, the healthcheck is Healthy at once, and `start` spawns
/// nothing.
///
/// ADR-0018: PERS-BOOT-001.
#[tokio::test]
async fn in_memory_post_init_reconciles_and_start_spawns_nothing() {
    use toolkit::HealthcheckStatus;

    let root = Uuid::new_v4();
    let hub = hub_over(
        MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
        one_upstream_registry(Uuid::new_v4(), Uuid::new_v4()),
    );
    let ctx = context(json!({ "config": {} }), None, hub);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&system_context()).await.expect("post_init");
    assert_eq!(health(&gear, &ctx).await, HealthcheckStatus::Healthy);
    gear.start(tokio_util::sync::CancellationToken::new())
        .await
        .expect("start");
    assert!(gear.registry_task.lock().is_none());
}

// ---------------------------------------------------------------------------
// Database storage across a restart
// ---------------------------------------------------------------------------

/// ADR-0018: PERS-BOOT-004.
#[tokio::test]
async fn upstreams_and_routes_survive_a_restart_on_a_database() {
    let file = SqliteFile::new();
    let tenant = Uuid::new_v4();
    let ctx = || provisioning_ctx(tenant).expect("security context");

    // Process A creates through the client it registered, then goes away with
    // its hub and its connection pool.
    let (upstream, route) = {
        let hub = wired_hub();
        let _gear = init(&context(
            database_section(),
            Some(file.open().await),
            Arc::clone(&hub),
        ))
        .await
        .expect("init A");
        let gw = hub
            .get::<dyn ServiceGatewayClientV1>()
            .expect("A registers the client");
        let upstream = gw
            .create_upstream(
                ctx(),
                oagw_sdk::CreateUpstreamRequest::builder(
                    oagw_sdk::Server {
                        endpoints: vec![oagw_sdk::Endpoint {
                            scheme: oagw_sdk::Scheme::Https,
                            host: "api.example.com".into(),
                            port: 443,
                        }],
                    },
                    oagw_sdk::HTTP_PROTOCOL_ID,
                )
                .tags(vec!["a".into(), "b".into()])
                .build(),
            )
            .await
            .expect("create upstream");
        let route = gw
            .create_route(
                ctx(),
                oagw_sdk::CreateRouteRequest::builder(
                    upstream.id,
                    oagw_sdk::MatchRules {
                        http: Some(oagw_sdk::HttpMatch {
                            methods: vec![oagw_sdk::HttpMethod::Get, oagw_sdk::HttpMethod::Post],
                            path: "/v1/things".into(),
                            query_allowlist: vec!["page".into()],
                            path_suffix_mode: oagw_sdk::PathSuffixMode::Append,
                        }),
                        grpc: None,
                    },
                )
                .tags(vec!["r".into()])
                .build(),
            )
            .await
            .expect("create route");
        (upstream, route)
    };

    // Process B: fresh hub, fresh pool, same file.
    let hub = wired_hub();
    let _gear = init(&context(
        database_section(),
        Some(file.open().await),
        Arc::clone(&hub),
    ))
    .await
    .expect("init B");
    let gw = hub
        .get::<dyn ServiceGatewayClientV1>()
        .expect("B registers the client");
    assert_eq!(
        gw.get_upstream(ctx(), upstream.id)
            .await
            .expect("upstream survives"),
        upstream
    );
    assert_eq!(
        gw.get_route(ctx(), route.id).await.expect("route survives"),
        route
    );
}

/// ADR-0018: PERS-BOOT-005.
#[tokio::test]
#[traced_test]
async fn provisioning_after_a_restart_leaves_what_the_database_holds() {
    let file = SqliteFile::new();
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);

    let first = {
        let gear = init(&context(
            database_section(),
            Some(file.open().await),
            wired_hub(),
        ))
        .await
        .expect("init A");
        reconcile(&gear, &upstreams, &routes, root)
            .await
            .expect("first boot provisions")
    };
    assert_eq!(first, CREATED_ALL);

    let gear = init(&context(
        database_section(),
        Some(file.open().await),
        wired_hub(),
    ))
    .await
    .expect("init B");
    let second = reconcile(&gear, &upstreams, &routes, root)
        .await
        .expect("second boot starts");
    // The stored rows round-trip to exactly what the registry holds.
    assert_eq!(second, UNCHANGED_ALL);
    let ctx = provisioning_ctx(root).unwrap();
    let cp = control_plane(&gear);
    let upstream = cp
        .get_upstream(&ctx, upstreams[0].request.id.unwrap())
        .await
        .unwrap();
    let route = cp
        .get_route(&ctx, routes[0].request.id.unwrap())
        .await
        .unwrap();
    assert_eq!(upstream.managed_by, ManagedBy::Registry);
    assert_eq!(route.managed_by, ManagedBy::Registry);
}

/// Two boots through `init`, `post_init` and `start` on one database file.
/// Each settles without logging an error, the second finds the registry
/// content up to date, and the rows read back as the first boot left them,
/// field for field.
///
/// ADR-0018: PERS-BOOT-005.
#[tokio::test]
#[traced_test]
async fn booting_twice_through_the_lifecycle_leaves_the_rows_unchanged() {
    use crate::domain::model::ListQuery;
    use toolkit::HealthcheckStatus;

    let file = SqliteFile::new();
    let root = Uuid::new_v4();
    let (upstream_id, route_id) = (Uuid::new_v4(), Uuid::new_v4());
    let pctx = provisioning_ctx(root).unwrap();
    let all = ListQuery { top: 100, skip: 0 };
    let boot = async || {
        let hub = hub_over(
            MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
            one_upstream_registry(upstream_id, route_id),
        );
        let ctx = context(database_section(), Some(file.open().await), hub);
        let gear = init(&ctx).await.expect("init");
        gear.post_init(&system_context()).await.expect("post_init");
        gear.start(tokio_util::sync::CancellationToken::new())
            .await
            .expect("start");
        eventually("the first run settles", || async {
            health(&gear, &ctx).await == HealthcheckStatus::Healthy
        })
        .await;
        let cp = control_plane(&gear);
        let rows = (
            cp.list_upstreams(&pctx, &all).await.unwrap(),
            cp.list_routes(&pctx, None, &all).await.unwrap(),
        );
        gear.stop(tokio_util::sync::CancellationToken::new())
            .await
            .expect("stop");
        rows
    };

    let first = boot().await;
    assert_eq!(
        (
            first.0.iter().map(|u| u.id).collect::<Vec<_>>(),
            first.1.iter().map(|r| r.id).collect::<Vec<_>>()
        ),
        (vec![upstream_id], vec![route_id])
    );
    assert!(!logs_contain("Registry content up to date"));
    let second = boot().await;
    assert_eq!(second, first);
    assert!(logs_contain("Registry content up to date"));
    logs_assert(
        |lines: &[&str]| match lines.iter().find(|l| l.contains(" ERROR ")) {
            Some(line) => Err(format!("an error was logged: {line}")),
            None => Ok(()),
        },
    );
}

/// The reconcile's write order on a database, one gear per boot: a path
/// hands over between two routes, then their upstream is replaced by one
/// with the same alias.
#[tokio::test]
async fn provisioning_hands_over_and_moves_routes_on_a_database() {
    let file = SqliteFile::new();
    let root = Uuid::new_v4();
    let (mut upstreams, mut routes) = registry_instances(root);
    routes.push(sibling_route(&routes[0], "/v2"));
    let boot = |upstreams: Vec<ProvisionedUpstream>, routes: Vec<ProvisionedRoute>| {
        let file = &file;
        async move {
            let gear = init(&context(
                database_section(),
                Some(file.open().await),
                wired_hub(),
            ))
            .await
            .expect("init");
            reconcile(&gear, &upstreams, &routes, root).await
        }
    };
    boot(upstreams.clone(), routes.clone()).await.unwrap();

    set_path(&mut routes[0], "/v3");
    set_path(&mut routes[1], "/v1");
    let handed_over = boot(upstreams.clone(), routes.clone()).await.unwrap();
    assert_eq!(handed_over.routes, Tally { updated: 2, ..NONE });

    let new_id = Uuid::new_v4();
    upstreams[0].request.id = Some(new_id);
    for route in &mut routes {
        route.request.upstream_id = new_id;
    }
    let moved = boot(upstreams.clone(), routes.clone()).await.unwrap();
    assert_eq!(
        moved,
        ProvisioningCounts {
            upstreams: Tally {
                created: 1,
                removed: 1,
                ..NONE
            },
            routes: Tally { updated: 2, ..NONE },
            passes: 2,
            stopped: false,
        }
    );
    let settled = boot(upstreams, routes).await.unwrap();
    assert_eq!(
        settled.upstreams,
        Tally {
            unchanged: 1,
            ..NONE
        }
    );
    assert_eq!(
        settled.routes,
        Tally {
            unchanged: 2,
            ..NONE
        }
    );
}

/// One reconcile run over the registry write path `init` built.
async fn reconcile(
    gear: &OutboundApiGatewayGear,
    upstreams: &[ProvisionedUpstream],
    routes: &[ProvisionedRoute],
    root: Uuid,
) -> anyhow::Result<ProvisioningCounts> {
    reconcile_registry(
        &*provisioner(gear),
        &ProvisioningMetrics::default(),
        &Unattended,
        upstreams,
        routes,
        root,
    )
    .await
}

/// The registry write path `init` built.
fn provisioner(gear: &OutboundApiGatewayGear) -> Arc<dyn RegistryProvisioner> {
    gear.registry_provisioner
        .get()
        .expect("init stores the provisioner")
        .clone()
}

/// Polls `cond` every 20 ms for up to 10 s.
async fn eventually<F, Fut>(what: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !cond().await {
        assert!(tokio::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// `stop` cancels only its own `start`'s task token: a second `start` gets
/// a live one, and its task reconciles again.
///
/// ADR-0018: PERS-LOOP-007.
#[tokio::test]
async fn a_second_start_after_stop_reconciles_again() {
    use crate::domain::repo::RegistryStateStore;
    use toolkit::HealthcheckStatus;

    let file = SqliteFile::new();
    let db = file.open().await;
    let root = Uuid::new_v4();
    let (upstream_id, route_id) = (Uuid::new_v4(), Uuid::new_v4());
    let hub = hub_over(
        MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
        one_upstream_registry(upstream_id, route_id),
    );
    let ctx = context(database_section(), Some(db.clone()), hub);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&system_context()).await.expect("post_init");
    let root_token = tokio_util::sync::CancellationToken::new();
    let cp = control_plane(&gear);
    let pctx = provisioning_ctx(root).unwrap();

    gear.start(root_token.clone()).await.expect("start");
    eventually("the first task reconciles", || async {
        health(&gear, &ctx).await == HealthcheckStatus::Healthy
    })
    .await;
    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");

    // Undo the first run: drop the route and record another hash.
    let provisioner = gear.registry_provisioner.get().unwrap().clone();
    provisioner
        .delete_registry_route(&pctx, route_id)
        .await
        .expect("delete the registry route");
    let state = crate::infra::storage::DbRegistryStateStore::new(DBProvider::new(db));
    let other = Uuid::new_v4();
    assert!(
        state
            .try_acquire(other, std::time::Duration::from_secs(60), None)
            .await
            .unwrap()
    );
    assert!(state.finish(other, Some("another-content")).await.unwrap());

    gear.start(root_token.clone()).await.expect("second start");
    eventually("the second task re-creates the route", || async {
        cp.get_route(&pctx, route_id).await.is_ok()
    })
    .await;
    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");
}

/// The task hangs off the token `start` receives: a runtime shutdown that
/// never reaches the stop phase still ends it.
///
/// ADR-0018: PERS-LOOP-007.
#[tokio::test]
async fn cancelling_the_start_token_ends_the_task() {
    let file = SqliteFile::new();
    let root = Uuid::new_v4();
    let hub = hub_over(
        MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
        one_upstream_registry(Uuid::new_v4(), Uuid::new_v4()),
    );
    let ctx = context(database_section(), Some(file.open().await), hub);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&system_context()).await.expect("post_init");
    let root_token = tokio_util::sync::CancellationToken::new();
    gear.start(root_token.clone()).await.expect("start");

    root_token.cancel();
    let (_, handle) = gear.registry_task.lock().take().expect("a running task");
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("the task ends with the runtime token")
        .expect("no panic");
}

/// Sets its flag when dropped: dropped inside a task, it shows the task
/// ended (aborted, for a task that never returns).
struct DropFlag(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A task blocked in a call that ignores cancel is aborted at the deadline:
/// `stop` returns instead of waiting on it.
///
/// ADR-0018: PERS-LOOP-009.
#[tokio::test]
async fn stop_aborts_a_task_that_ignores_its_token_at_the_deadline() {
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let guard = DropFlag(dropped.clone());
    let handle = tokio::spawn(async move {
        let _guard = guard;
        std::future::pending::<()>().await;
    });
    let deadline = tokio_util::sync::CancellationToken::new();
    deadline.cancel();

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stop_registry_task(tokio_util::sync::CancellationToken::new(), handle, deadline),
    )
    .await
    .expect("stop returns at the deadline");
    assert!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        "the task was aborted"
    );
}

/// A panicked task is logged at `stop`.
#[tokio::test]
#[traced_test]
async fn stop_logs_a_panicked_task() {
    let handle = tokio::spawn(async { panic!("registry run blew up") });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stop_registry_task(
            tokio_util::sync::CancellationToken::new(),
            handle,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await
    .expect("stop returns once the task ends");
    assert!(logs_contain("Registry task panicked"));
}
