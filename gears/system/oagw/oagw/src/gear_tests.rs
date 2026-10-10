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
    ProvisioningCounts, Tally, provisioning_ctx, reconcile_registry,
};
use crate::domain::test_support::{
    CapturingAuthZResolverClient, MockCredStoreClient, MockTenantResolverClient,
};
use crate::domain::type_provisioning::{ProvisionedRoute, ProvisionedUpstream};
use crate::infra::storage::testing::SqliteFile;

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
/// handle the runtime's DB phase would attach, if any.
fn context(section: Value, db: Option<Db>, hub: Arc<ClientHub>) -> GearCtx {
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

#[tokio::test]
#[traced_test]
async fn without_database_config_init_keeps_memory_storage_and_warns() {
    let ctx = context(json!({ "config": {} }), None, wired_hub());
    init(&ctx).await.expect("init without a database");
    logs_assert(assert_warned_once_about_memory);
}

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

// ---------------------------------------------------------------------------
// Database storage across a restart
// ---------------------------------------------------------------------------

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
        reconcile_registry(&*provisioner(&gear), &upstreams, &routes, root)
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
    let second = reconcile_registry(&*provisioner(&gear), &upstreams, &routes, root)
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

/// The reconcile's write order on a database, one gear per boot: two routes
/// swap paths, then their upstream is replaced by one with the same alias.
#[tokio::test]
async fn provisioning_swaps_and_moves_routes_on_a_database() {
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
            reconcile_registry(&*provisioner(&gear), &upstreams, &routes, root).await
        }
    };
    boot(upstreams.clone(), routes.clone()).await.unwrap();

    set_path(&mut routes[0], "/v2");
    set_path(&mut routes[1], "/v1");
    let swapped = boot(upstreams.clone(), routes.clone()).await.unwrap();
    assert_eq!(swapped.routes, Tally { updated: 2, ..NONE });

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

/// The registry write path `init` built.
fn provisioner(gear: &OutboundApiGatewayGear) -> Arc<dyn RegistryProvisioner> {
    gear.registry_provisioner
        .get()
        .expect("init stores the provisioner")
        .clone()
}
