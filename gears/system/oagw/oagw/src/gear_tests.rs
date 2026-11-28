//! Gear wiring: storage selection (ADR-0018 *Backend Selection*), restart
//! survival on a database, and registry provisioning across boots of a gear on
//! a database. The reconcile itself is tested in `registry_reconcile_tests.rs`.
//!
//! Each test runs `Gear::init` over a real `GearCtx`. The `ClientHub` holds
//! the four clients init resolves; the database, when there is one, is a
//! migrated `SQLite` file, as the runtime's DB phase would leave it.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use credstore_sdk::CredStoreClientV1;
use oagw_sdk::api::ServiceGatewayClientV1;
use serde_json::{Value, json};
use tenant_resolver_sdk::{TenantId, TenantResolverClient};
use toolkit::{ClientHub, ConfigProvider, GearCtx};
use toolkit_db::{DBProvider, Db};
use toolkit_security::SecurityContext;
use tracing_test::traced_test;
use types_registry_sdk::testing_platform::MockTypesRegistry;
use uuid::Uuid;

use super::*;
use crate::domain::error::DomainError;
use crate::domain::model::ManagedBy;
use crate::domain::repo::Tags;
#[cfg(feature = "cluster")]
use crate::domain::services::registry_reconcile::Tally;
use crate::domain::services::registry_reconcile::provisioning_ctx;
#[cfg(feature = "cluster")]
use crate::domain::services::registry_reconcile::tests::{
    CREATED_ALL, NONE, UNCHANGED_ALL, registry_instances, set_path, sibling_route, upstream_at,
};
#[cfg(feature = "cluster")]
use crate::domain::services::registry_reconcile::{
    ProvisioningCounts, Unattended, reconcile_registry,
};
#[cfg(feature = "cluster")]
use crate::domain::test_support::ProvisioningMetrics;
use crate::domain::test_support::{
    CapturingAuthZResolverClient, MockCredStoreClient, MockTenantResolverClient,
};
#[cfg(feature = "cluster")]
use crate::domain::type_provisioning::{ProvisionedRoute, ProvisionedUpstream};
#[cfg(feature = "cluster")]
use crate::infra::proxy_cache::testing::{ClusterFixture, ScriptedCache, facade, read_log, until};
use crate::infra::storage::testing::SqliteFile;
use toolkit::contracts::RunnableCapability;
use toolkit::gts_publication::GtsPublicationCapability;

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

/// The registry instance identifiers of [`one_upstream_registry`].
const REGISTRY_UPSTREAM: &str =
    toolkit_gts::gts_id!("cf.core.oagw.upstream.v1~cf.test.oagw.api.v1");
const REGISTRY_ROUTE: &str = toolkit_gts::gts_id!("cf.core.oagw.route.v1~cf.test.oagw.api.v1");

/// The row ids OAGW provisions [`REGISTRY_UPSTREAM`] and [`REGISTRY_ROUTE`]
/// into: their registry UUIDs.
fn registry_row_ids() -> (Uuid, Uuid) {
    let uuid = |id| gts::GtsId::try_new(id).unwrap().to_uuid();
    (uuid(REGISTRY_UPSTREAM), uuid(REGISTRY_ROUTE))
}

/// A fresh hub holding the clients init resolves.
fn wired_hub() -> Arc<ClientHub> {
    hub_over(
        MockTenantResolverClient::single_tenant(),
        MockTypesRegistry::new(),
    )
}

/// A fresh hub holding the clients init resolves, with these tenants and
/// types-registry content.
fn hub_over(tenants: MockTenantResolverClient, registry: MockTypesRegistry) -> Arc<ClientHub> {
    hub_with(Arc::new(tenants), registry)
}

/// [`hub_over`] with any tenant resolver.
fn hub_with(tenants: Arc<dyn TenantResolverClient>, registry: MockTypesRegistry) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TenantResolverClient>(tenants);
    hub.register::<dyn CredStoreClientV1>(Arc::new(MockCredStoreClient::empty()));
    hub.register::<dyn AuthZResolverApi>(Arc::new(CapturingAuthZResolverClient::new()));
    Arc::new(registry).install(&hub);
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
    context_under(section, db, hub, Default::default())
}

/// [`context`] with `root` as the runtime's root token.
fn context_under(
    section: Value,
    db: Option<Db>,
    hub: Arc<ClientHub>,
    root: tokio_util::sync::CancellationToken,
) -> GearCtx {
    let manifest = Arc::new(manifest(&section));
    unpublished_context(section, db, hub, root).with_gts_publication_manifest_for_test(manifest)
}

/// The publication manifest the runtime builds for `section`.
fn manifest(section: &Value) -> toolkit::gts_publication::GtsManifest {
    OutboundApiGatewayGear::default()
        .descriptor()
        .build_gts_manifest(&FixedConfig(section.clone()))
        .expect("the OAGW manifest builds")
}

/// As [`context_under`], without the admitted manifest the runtime attaches.
fn unpublished_context(
    section: Value,
    db: Option<Db>,
    hub: Arc<ClientHub>,
    root: tokio_util::sync::CancellationToken,
) -> GearCtx {
    crate::domain::test_support::ensure_crypto_provider();
    let ctx = GearCtx::new(
        OutboundApiGatewayGear::MODULE_NAME,
        Uuid::new_v4(),
        Arc::new(FixedConfig(section)),
        hub,
        root,
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
// GTS publication
// ---------------------------------------------------------------------------

/// The runtime publishes the OAGW catalog before `init`: the manifest is the
/// catalog, under the gear's name and version.
#[test]
fn the_manifest_is_the_oagw_catalog() {
    let manifest = manifest(&json!({ "config": {} }));

    let expected: Vec<(gts::GtsId, Value)> = crate::gts_declarations()
        .into_iter()
        .map(|(id, text)| {
            (
                gts::GtsId::try_new(&id).unwrap(),
                serde_json::from_str(&text).unwrap(),
            )
        })
        .collect();
    assert_eq!(manifest.documents(), expected.as_slice());
    assert_eq!(expected.len(), 23, "7 Type Schemas and 16 Instances");
    assert_eq!(manifest.publisher_name(), "oagw");
    assert_eq!(manifest.publisher_version(), env!("CARGO_PKG_VERSION"));
}

/// Outside the runtime — without an admitted manifest — `init` fails before
/// serving anything.
#[tokio::test]
async fn init_without_an_admitted_manifest_fails() {
    let hub = wired_hub();
    let ctx = unpublished_context(
        json!({ "config": {} }),
        None,
        Arc::clone(&hub),
        Default::default(),
    );

    let err = init(&ctx).await.err().expect("init refuses");

    assert!(
        format!("{err:#}").contains("admitted GTS manifest"),
        "{err:#}"
    );
    assert!(hub.get::<dyn ServiceGatewayClientV1>().is_err());
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
    Arc::new(MockTypesRegistry::new()).install(&hub);
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
    let (upstream_id, route_id) = registry_row_ids();
    let registry = one_upstream_registry();
    let hub = hub_over(
        MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
        registry,
    );
    let gear = init(&context(json!({ "config": {} }), None, hub))
        .await
        .expect("init");
    gear.post_init(&post_init_ctx()).await.expect("post_init");

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

/// A registry of one upstream with one route, named [`REGISTRY_UPSTREAM`]
/// and [`REGISTRY_ROUTE`]; the route names the upstream by its identifier.
fn one_upstream_registry() -> MockTypesRegistry {
    let registry = MockTypesRegistry::new();
    registry.seed(
        REGISTRY_UPSTREAM,
        json!({
            "id": REGISTRY_UPSTREAM,
            "server": {
                "endpoints": [{"host": "api.example.com", "port": 443, "scheme": "https"}]
            },
            "protocol": oagw_sdk::HTTP_PROTOCOL_ID,
            "enabled": true,
            "tags": [],
        }),
    );
    registry.seed(
        REGISTRY_ROUTE,
        json!({
            "id": REGISTRY_ROUTE,
            "upstream_id": REGISTRY_UPSTREAM,
            "match": { "http": { "methods": ["GET"], "path": "/v1" } },
            "enabled": true,
            "tags": [],
            "priority": 0,
        }),
    );
    registry
}

/// The platform registry API is a consumed contract, wired after `init`:
/// `init` does not need it, and `post_init` fails without it rather than
/// provisioning from nothing.
#[tokio::test]
async fn post_init_needs_the_consumed_platform_registry_client() {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn TenantResolverClient>(Arc::new(MockTenantResolverClient::single_tenant()));
    hub.register::<dyn CredStoreClientV1>(Arc::new(MockCredStoreClient::empty()));
    let gear = init(&context(json!({ "config": {} }), None, hub))
        .await
        .expect("init without the registry client");

    let err = gear.post_init(&post_init_ctx()).await.unwrap_err();

    assert!(
        format!("{err:#}").contains("types-registry platform client unavailable"),
        "{err:#}"
    );
}

/// The context of `post_init`, which reads nothing from it.
fn post_init_ctx() -> GearCtx {
    context(json!({ "config": {} }), None, Arc::new(ClientHub::new()))
}

async fn health(gear: &OutboundApiGatewayGear, ctx: &GearCtx) -> toolkit::HealthcheckStatus {
    health_result(gear, ctx).await.status
}

/// The stable code of the registry provisioning healthcheck, if any.
#[cfg(feature = "cluster")]
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
#[cfg(feature = "cluster")]
#[tokio::test]
async fn on_a_database_start_reconciles_in_the_background() {
    use toolkit::HealthcheckStatus;

    let file = SqliteFile::new();
    let root = Uuid::new_v4();
    let (upstream_id, route_id) = registry_row_ids();
    let hub = hub_over(
        MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
        one_upstream_registry(),
    );
    let cluster = ClusterFixture::standalone(&hub);
    let ctx = context(database_section(), Some(file.open().await), hub);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&post_init_ctx()).await.expect("post_init");

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
    assert!(gear.change_feed.lock().is_none());
    cluster.stop().await;
}

/// A registry lease another pod holds never delays startup: `post_init`
/// and `start` return, the pod reports Degraded, and `stop` ends the
/// waiting task.
///
/// ADR-0018: PERS-LOOP-008.
#[cfg(feature = "cluster")]
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
    let (upstream_id, _) = registry_row_ids();
    let hub = hub_over(
        MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
        one_upstream_registry(),
    );
    let cluster = ClusterFixture::standalone(&hub);
    let ctx = context(database_section(), Some(db), hub);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&post_init_ctx()).await.expect("post_init");
    gear.start(tokio_util::sync::CancellationToken::new())
        .await
        .expect("start");
    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    // The provisioning part alone: the cluster part reads the cluster SDK's
    // process-global readiness, which other tests in the process share.
    let provisioning = ProvisioningHealth(gear.provisioning.clone()).check().await;
    assert_eq!(provisioning.status, HealthcheckStatus::Degraded);
    assert_eq!(
        provisioning.code.as_deref(),
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
    cluster.stop().await;
}

/// `start` after an in-memory `post_init` run that did not fail: the task
/// waits for the re-check interval (±20%) before its first run, runs once
/// it has passed, and `stop` ends it.
async fn assert_in_memory_start_rechecks_after_the_interval(
    registry: MockTypesRegistry,
    after_post_init: toolkit::HealthcheckStatus,
) {
    const RECHECK: std::time::Duration = std::time::Duration::from_mins(1);
    let tenants = MockTenantResolverClient::with_siblings(TenantId(Uuid::new_v4()), vec![]);
    let calls = tenants.calls();
    let calls = || calls.load(std::sync::atomic::Ordering::SeqCst);
    let hub = hub_over(tenants, registry);
    let ctx = context(
        json!({ "config": { "registry_provisioning": { "recheck_interval": "1m" } } }),
        None,
        hub,
    );
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&post_init_ctx()).await.expect("post_init");
    assert_eq!(health(&gear, &ctx).await, after_post_init);
    let post_init_calls = calls();
    assert!(
        post_init_calls > 0,
        "post_init's run asks for the root tenant"
    );

    gear.start(tokio_util::sync::CancellationToken::new())
        .await
        .expect("start");
    assert!(gear.registry_task.lock().is_some(), "the task runs");
    tokio::time::sleep(RECHECK.mul_f64(0.79)).await;
    assert_eq!(calls(), post_init_calls, "no run before the re-check");
    tokio::time::sleep(RECHECK.mul_f64(0.42)).await;
    assert!(calls() > post_init_calls, "a run after the re-check");
    assert_eq!(health(&gear, &ctx).await, after_post_init);

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        gear.stop(tokio_util::sync::CancellationToken::new()),
    )
    .await
    .expect("stop ends the task")
    .expect("stop");
    assert!(gear.registry_task.lock().is_none());
}

/// In memory `post_init` reconciles before the pod serves, and the
/// healthcheck is Healthy at once; `start` spawns the re-check task, which
/// waits for the re-check interval first, and `stop` ends it.
///
/// ADR-0018: PERS-BOOT-001, PERS-LOOP-011.
#[tokio::test(start_paused = true)]
async fn in_memory_post_init_reconciles_and_start_rechecks_in_the_background() {
    assert_in_memory_start_rechecks_after_the_interval(
        one_upstream_registry(),
        toolkit::HealthcheckStatus::Healthy,
    )
    .await;
}

/// A `post_init` run that skipped an instance (a route on an unknown
/// upstream) did not fail either: the first run also waits for the re-check.
///
/// ADR-0018: PERS-LOOP-011.
#[tokio::test(start_paused = true)]
async fn in_memory_start_after_a_not_clean_run_waits_for_the_recheck() {
    let registry = MockTypesRegistry::new();
    registry.seed(
        REGISTRY_ROUTE,
        json!({
            "id": REGISTRY_ROUTE,
            "upstream_id": REGISTRY_UPSTREAM,
            "match": { "http": { "methods": ["GET"], "path": "/v1" } },
            "enabled": true,
            "tags": [],
            "priority": 0,
        }),
    );
    assert_in_memory_start_rechecks_after_the_interval(
        registry,
        toolkit::HealthcheckStatus::Degraded,
    )
    .await;
}

/// A tenant resolver whose root tenant does not exist until `create_root`,
/// as before account-management's bootstrap; every other call answers.
struct LateRoot {
    inner: MockTenantResolverClient,
    root: TenantId,
    created: std::sync::atomic::AtomicBool,
    root_calls: std::sync::atomic::AtomicUsize,
}

impl LateRoot {
    fn new(root: Uuid) -> Self {
        Self {
            inner: MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
            root: TenantId(root),
            created: std::sync::atomic::AtomicBool::new(false),
            root_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn create_root(&self) {
        self.created
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn root_calls(&self) -> usize {
        self.root_calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl TenantResolverClient for LateRoot {
    async fn get_tenant(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<tenant_resolver_sdk::TenantInfo, tenant_resolver_sdk::TenantResolverError> {
        self.inner.get_tenant(ctx, id).await
    }
    async fn get_root_tenant(
        &self,
        ctx: &SecurityContext,
    ) -> Result<tenant_resolver_sdk::TenantInfo, tenant_resolver_sdk::TenantResolverError> {
        self.root_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if !self.created.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(tenant_resolver_sdk::TenantResolverError::TenantNotFound {
                tenant_id: self.root,
            });
        }
        self.inner.get_root_tenant(ctx).await
    }
    async fn get_tenants(
        &self,
        ctx: &SecurityContext,
        ids: &[TenantId],
        options: &tenant_resolver_sdk::GetTenantsOptions,
    ) -> Result<Vec<tenant_resolver_sdk::TenantInfo>, tenant_resolver_sdk::TenantResolverError>
    {
        self.inner.get_tenants(ctx, ids, options).await
    }
    async fn get_ancestors(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
        options: &tenant_resolver_sdk::GetAncestorsOptions,
    ) -> Result<tenant_resolver_sdk::GetAncestorsResponse, tenant_resolver_sdk::TenantResolverError>
    {
        self.inner.get_ancestors(ctx, id, options).await
    }
    async fn get_descendants(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
        options: &tenant_resolver_sdk::GetDescendantsOptions,
    ) -> Result<tenant_resolver_sdk::GetDescendantsResponse, tenant_resolver_sdk::TenantResolverError>
    {
        self.inner.get_descendants(ctx, id, options).await
    }
    async fn is_ancestor(
        &self,
        ctx: &SecurityContext,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        options: &tenant_resolver_sdk::IsAncestorOptions,
    ) -> Result<bool, tenant_resolver_sdk::TenantResolverError> {
        self.inner
            .is_ancestor(ctx, ancestor_id, descendant_id, options)
            .await
    }
}

/// In memory, a root tenant that does not exist yet (the tr-authz binary
/// before account-management's bootstrap, #4568) does not fail startup:
/// `post_init` succeeds with the healthcheck Degraded `failed`, `start`
/// retries with backoff, and once the root exists the registry rows appear
/// and the healthcheck turns Healthy. `stop` ends the task.
///
/// ADR-0018: PERS-LOOP-011.
#[tokio::test(start_paused = true)]
#[traced_test]
async fn in_memory_startup_survives_a_missing_root_tenant_and_provisions_later() {
    use toolkit::HealthcheckStatus;

    let root = Uuid::new_v4();
    let (upstream_id, route_id) = registry_row_ids();
    let tenants = Arc::new(LateRoot::new(root));
    let hub = hub_with(tenants.clone(), one_upstream_registry());
    let ctx = context(json!({ "config": {} }), None, hub);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&post_init_ctx())
        .await
        .expect("a missing root tenant does not fail startup");
    assert!(logs_contain(
        "Registry provisioning failed at startup; retrying in the background"
    ));
    let result = health_result(&gear, &ctx).await;
    assert_eq!(
        (result.status, result.code.as_deref()),
        (
            HealthcheckStatus::Degraded,
            Some("registry_provisioning_failed")
        )
    );
    let cp = control_plane(&gear);
    let pctx = provisioning_ctx(root).unwrap();
    assert!(cp.get_upstream(&pctx, upstream_id).await.is_err());

    gear.start(tokio_util::sync::CancellationToken::new())
        .await
        .expect("start");
    // The task runs at once, then backs off 1 s, 2 s (±20%).
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    assert!(
        tenants.root_calls() >= 4,
        "post_init's run and at least three retries, got {}",
        tenants.root_calls()
    );
    assert_eq!(gear.provisioning.get(), ProvisioningStatus::Failed);
    // Retries below the backoff cap are expected at startup: warnings only.
    logs_assert(|lines: &[&str]| {
        let retries: Vec<_> = lines
            .iter()
            .filter(|l| l.contains("Registry provisioning run failed; retrying"))
            .collect();
        match retries.iter().find(|l| !l.contains(" WARN ")) {
            _ if retries.is_empty() => Err("no retry was logged".to_owned()),
            Some(l) => Err(format!("a retry below the cap logged above WARN: {l}")),
            None => Ok(()),
        }
    });

    tenants.create_root();
    // The next backoff is at most 8 s (±20%).
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    assert_eq!(health(&gear, &ctx).await, HealthcheckStatus::Healthy);
    let upstream = cp
        .get_upstream(&pctx, upstream_id)
        .await
        .expect("the upstream is provisioned once the root exists");
    assert_eq!(upstream.managed_by, ManagedBy::Registry);
    cp.get_route(&pctx, route_id)
        .await
        .expect("the route is provisioned too");

    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");
    assert!(gear.registry_task.lock().is_none(), "stop ends the task");
    let calls = tenants.root_calls();
    tokio::time::sleep(std::time::Duration::from_mins(10)).await;
    assert_eq!(tenants.root_calls(), calls, "no run after stop");
}

/// An empty registry needs no root tenant: startup never asks the tenant
/// resolver, and the healthcheck is Healthy at once.
///
/// ADR-0018: PERS-PROV-019.
#[tokio::test]
async fn an_empty_registry_never_calls_the_tenant_resolver() {
    use toolkit::HealthcheckStatus;

    let tenants = Arc::new(LateRoot::new(Uuid::new_v4()));
    let hub = hub_with(tenants.clone(), MockTypesRegistry::new());
    let ctx = context(json!({ "config": {} }), None, hub);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&post_init_ctx()).await.expect("post_init");
    assert_eq!(health(&gear, &ctx).await, HealthcheckStatus::Healthy);
    assert_eq!(tenants.root_calls(), 0);
}

/// An SSE stream on a silent upstream, watching the gear's stream signal.
fn open_stream(gear: &OutboundApiGatewayGear) -> oagw_sdk::body::BodyStream {
    let streams = gear.stream_shutdown.get().expect("init sets the signal");
    crate::infra::proxy::streaming::tests::sse(
        Box::pin(futures_util::stream::pending::<
            Result<bytes::Bytes, oagw_sdk::body::BoxError>,
        >()),
        std::time::Duration::from_secs(300),
        streams.subscribe(),
    )
}

/// The stream ends with the shutdown error within a second, then ends.
async fn assert_cut(mut stream: oagw_sdk::body::BodyStream, case: &str) {
    use futures_util::StreamExt as _;
    let next = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
        .await
        .unwrap_or_else(|_| panic!("{case}: the stream is still open"));
    crate::infra::proxy::streaming::tests::assert_shutdown_error(next);
    assert!(stream.next().await.is_none(), "{case}: the stream goes on");
}

/// A gear after `init` and `post_init`, on a context whose root token is
/// `root`; not started.
async fn initialized_gear(root: tokio_util::sync::CancellationToken) -> OutboundApiGatewayGear {
    let hub = hub_over(
        MockTenantResolverClient::with_siblings(TenantId(Uuid::new_v4()), vec![]),
        one_upstream_registry(),
    );
    let ctx = context_under(json!({ "config": {} }), None, hub, root);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&post_init_ctx()).await.expect("post_init");
    gear
}

/// [`initialized_gear`], started with `root` as the runtime does.
async fn started_gear(root: tokio_util::sync::CancellationToken) -> OutboundApiGatewayGear {
    let gear = initialized_gear(root.clone()).await;
    gear.start(root).await.expect("start");
    gear
}

/// `stop` ends the open SSE streams (and WebSocket relays, which watch the
/// same signal), also one waiting on its upstream; a later `start` lets new
/// streams run (#4987).
#[tokio::test]
async fn stop_ends_open_streams_and_a_later_start_lets_new_ones_run() {
    use crate::infra::proxy::streaming::wake_flag::WakeFlag;
    use futures_util::StreamExt as _;
    let gear = started_gear(tokio_util::sync::CancellationToken::new()).await;

    let mut waiting = open_stream(&gear);
    let flag = Arc::new(WakeFlag::default());
    flag.park(std::pin::pin!(waiting.next()));
    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");
    assert!(flag.woken(), "stop wakes a waiting stream");
    assert_cut(waiting, "waiting when the stop came").await;

    // Opened while stopped, polled only after the restart.
    let unpolled = open_stream(&gear);
    gear.start(tokio_util::sync::CancellationToken::new())
        .await
        .expect("start again");
    let restarted = open_stream(&gear);
    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");
    gear.start(tokio_util::sync::CancellationToken::new())
        .await
        .expect("start again");
    assert_cut(unpolled, "opened while stopped, polled after a restart").await;
    assert_cut(restarted, "polled only after a stop and a start").await;

    let mut fresh = open_stream(&gear);
    let still_open = tokio::time::timeout(std::time::Duration::from_millis(50), fresh.next()).await;
    assert!(still_open.is_err(), "a stream opened after start runs");
}

/// After a stop, the earlier start's shutdown watcher is gone: cancelling
/// that start's token does not cut the streams of the next start.
#[tokio::test]
async fn an_earlier_starts_token_does_not_cut_later_streams() {
    use futures_util::StreamExt as _;
    let gear = initialized_gear(tokio_util::sync::CancellationToken::new()).await;
    let first = tokio_util::sync::CancellationToken::new();
    gear.start(first.clone()).await.expect("start");
    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");
    let second = tokio_util::sync::CancellationToken::new();
    gear.start(second.clone()).await.expect("start again");
    let mut open = open_stream(&gear);
    first.cancel();
    let still_open = tokio::time::timeout(std::time::Duration::from_millis(100), open.next()).await;
    assert!(still_open.is_err(), "the stream must stay open");
    // Its own start's token does reach it.
    second.cancel();
    assert_cut(open, "the current start's cancellation").await;
}

/// A start after a start without a stop (an anomaly the gear warns about)
/// stops what the earlier start began, open streams included, on every
/// storage backend.
#[tokio::test]
async fn a_start_without_a_stop_ends_the_open_streams() {
    let gear = started_gear(tokio_util::sync::CancellationToken::new()).await;
    let open = open_stream(&gear);
    gear.start(tokio_util::sync::CancellationToken::new())
        .await
        .expect("start again");
    assert_cut(open, "a repeated start").await;
}

/// The runtime drains HTTP before it calls `stop`: the open streams end as
/// soon as its cancellation fires, not at the drain deadline.
#[tokio::test]
async fn runtime_shutdown_ends_open_streams_before_stop() {
    let root = tokio_util::sync::CancellationToken::new();
    let gear = started_gear(root.clone()).await;
    let open = open_stream(&gear);
    root.cancel();
    assert_cut(open, "runtime cancellation").await;
}

/// The runtime serves the REST surface before it starts the app gears, so a
/// stream can open before the first `start`. Runtime shutdown ends it too.
#[tokio::test]
async fn runtime_shutdown_ends_a_stream_opened_before_the_first_start() {
    let root = tokio_util::sync::CancellationToken::new();
    let gear = initialized_gear(root.clone()).await;
    let open = open_stream(&gear);
    gear.start(root.clone()).await.expect("start");
    root.cancel();
    assert_cut(open, "runtime cancellation").await;
}

/// A stream opened before the first `start` is ended by the next `stop`,
/// though the start gave later streams another token.
#[tokio::test]
async fn stop_ends_a_stream_opened_before_the_first_start() {
    let root = tokio_util::sync::CancellationToken::new();
    let gear = initialized_gear(root.clone()).await;
    let open = open_stream(&gear);
    gear.start(root).await.expect("start");
    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");
    assert_cut(open, "gear stop").await;
}

// ---------------------------------------------------------------------------
// Database storage across a restart
// ---------------------------------------------------------------------------

/// ADR-0018: PERS-BOOT-004.
#[cfg(feature = "cluster")]
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
#[cfg(feature = "cluster")]
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
#[cfg(feature = "cluster")]
#[tokio::test]
#[traced_test]
async fn booting_twice_through_the_lifecycle_leaves_the_rows_unchanged() {
    use crate::domain::model::ListQuery;

    let file = SqliteFile::new();
    let root = Uuid::new_v4();
    let (upstream_id, route_id) = registry_row_ids();
    let pctx = provisioning_ctx(root).unwrap();
    let all = ListQuery { top: 100, skip: 0 };
    let boot = async || {
        let hub = hub_over(
            MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
            one_upstream_registry(),
        );
        let cluster = ClusterFixture::standalone(&hub);
        let ctx = context(database_section(), Some(file.open().await), hub);
        let gear = init(&ctx).await.expect("init");
        gear.post_init(&post_init_ctx()).await.expect("post_init");
        gear.start(tokio_util::sync::CancellationToken::new())
            .await
            .expect("start");
        eventually("the first run settles", || async {
            gear.provisioning.get() == ProvisioningStatus::Settled
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
        cluster.stop().await;
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
#[cfg(feature = "cluster")]
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
#[cfg(feature = "cluster")]
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
        Some(root),
    )
    .await
}

/// The registry write path `init` built.
#[cfg(feature = "cluster")]
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
#[cfg(feature = "cluster")]
#[tokio::test]
async fn a_second_start_after_stop_reconciles_again() {
    use crate::domain::repo::RegistryStateStore;

    let file = SqliteFile::new();
    let db = file.open().await;
    let root = Uuid::new_v4();
    let (_, route_id) = registry_row_ids();
    let hub = hub_over(
        MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
        one_upstream_registry(),
    );
    let cluster = ClusterFixture::standalone(&hub);
    let ctx = context(database_section(), Some(db.clone()), hub);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&post_init_ctx()).await.expect("post_init");
    let root_token = tokio_util::sync::CancellationToken::new();
    let cp = control_plane(&gear);
    let pctx = provisioning_ctx(root).unwrap();

    gear.start(root_token.clone()).await.expect("start");
    eventually("the first task reconciles", || async {
        gear.provisioning.get() == ProvisioningStatus::Settled
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
    cluster.stop().await;
}

/// The task hangs off the token `start` receives: a runtime shutdown that
/// never reaches the stop phase still ends it.
///
/// ADR-0018: PERS-LOOP-007.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn cancelling_the_start_token_ends_the_task() {
    let file = SqliteFile::new();
    let root = Uuid::new_v4();
    let hub = hub_over(
        MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
        one_upstream_registry(),
    );
    let cluster = ClusterFixture::standalone(&hub);
    let ctx = context(database_section(), Some(file.open().await), hub);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&post_init_ctx()).await.expect("post_init");
    let root_token = tokio_util::sync::CancellationToken::new();
    gear.start(root_token.clone()).await.expect("start");

    root_token.cancel();
    let (_, handle) = gear.registry_task.lock().take().expect("a running task");
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("the task ends with the runtime token")
        .expect("no panic");
    let feed = gear.change_feed.lock().take().expect("a running feed");
    until("the change feed ends with the runtime token", || {
        feed.is_finished()
    })
    .await;
    cluster.stop().await;
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

// ---------------------------------------------------------------------------
// The proxy configuration cache (ADR-0020)
// ---------------------------------------------------------------------------

/// An upstream `api.example.com` with one `POST /v1/things` route, created
/// through the SDK in `ctx`'s tenant, and the server it was created with.
async fn served_upstream(
    gw: &dyn ServiceGatewayClientV1,
    ctx: SecurityContext,
) -> (oagw_sdk::Upstream, oagw_sdk::Route, oagw_sdk::Server) {
    let server = oagw_sdk::Server {
        endpoints: vec![oagw_sdk::Endpoint {
            scheme: oagw_sdk::Scheme::Https,
            host: "api.example.com".into(),
            port: 443,
        }],
    };
    let upstream = gw
        .create_upstream(
            ctx.clone(),
            oagw_sdk::CreateUpstreamRequest::builder(server.clone(), oagw_sdk::HTTP_PROTOCOL_ID)
                .build(),
        )
        .await
        .expect("create upstream");
    let route = gw
        .create_route(
            ctx,
            oagw_sdk::CreateRouteRequest::builder(
                upstream.id,
                oagw_sdk::MatchRules {
                    http: Some(oagw_sdk::HttpMatch {
                        methods: vec![oagw_sdk::HttpMethod::Post],
                        path: "/v1/things".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: oagw_sdk::PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .expect("create route");
    (upstream, route, server)
}

/// What `cp` resolves `api.example.com` to for `ctx`, without tags.
async fn resolve_served(
    cp: &dyn ControlPlaneService,
    ctx: &SecurityContext,
) -> Result<(Uuid, Uuid), DomainError> {
    cp.resolve_proxy_target(
        &crate::domain::services::ProxyGrant::for_tests(ctx.clone()),
        "api.example.com",
        "POST",
        "/v1/things",
        Tags::Skip,
    )
    .await
    .map(|target| (target.upstream.id, target.route.id))
}

/// Disable `tenant`'s upstream `id` in `db`, behind the gear's back.
#[cfg(feature = "cluster")]
async fn disable_behind(db: Db, tenant: Uuid, id: Uuid) {
    let behind = DbUpstreamRepo::new(DBProvider::<RepositoryError>::new(db));
    let mut row = behind.get_by_id(tenant, id).await.unwrap();
    row.enabled = false;
    behind.update(row).await.unwrap();
}

/// On a database the gear resolves proxy targets through the cache, and
/// the writes of the SDK and of the registry provisioner evict it: a change
/// made in the database behind the gear stays unseen until one of them
/// writes. (The Management API's handlers share the SDK's Control Plane.)
///
/// ADR-0020: CACHE-FRESH-002.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn on_a_database_proxy_reads_are_cached_and_writes_evict() {
    let file = SqliteFile::new();
    let db = file.open().await;
    let hub = wired_hub();
    let gear = init(&context(
        database_section(),
        Some(db.clone()),
        Arc::clone(&hub),
    ))
    .await
    .expect("init");
    let tenant = Uuid::new_v4();
    let ctx = || provisioning_ctx(tenant).expect("security context");
    let gw = hub
        .get::<dyn ServiceGatewayClientV1>()
        .expect("init registers the client");
    let cp = control_plane(&gear);
    let resolve = async |alias: &str| {
        cp.resolve_proxy_target(
            &crate::domain::services::ProxyGrant::for_tests(ctx().clone()),
            alias,
            "POST",
            "/v1/things",
            Tags::Skip,
        )
        .await
        .map(|target| (target.upstream.id, target.route.id))
    };

    let (upstream, route, server) = served_upstream(&*gw, ctx()).await;
    let served = (upstream.id, route.id);
    assert_eq!(resolve("api.example.com").await.expect("served"), served);

    // Disabled behind the gear: the cached answer stands.
    disable_behind(db, tenant, upstream.id).await;
    assert_eq!(
        resolve("api.example.com").await.expect("cached"),
        served,
        "cached"
    );

    // An SDK write keeping what it read evicts, and the change shows.
    let stored = gw.get_upstream(ctx(), upstream.id).await.unwrap();
    assert!(!stored.enabled);
    gw.update_upstream(
        ctx(),
        upstream.id,
        oagw_sdk::UpdateUpstreamRequest::builder(server, oagw_sdk::HTTP_PROTOCOL_ID)
            .enabled(stored.enabled)
            .build(),
    )
    .await
    .expect("update upstream");
    let disabled = resolve("api.example.com").await.unwrap_err();
    assert!(
        matches!(disabled, DomainError::UpstreamDisabled { .. }),
        "{disabled:?}"
    );

    // The registry provisioner's writes, including its own delete.
    let provisioner = gear.registry_provisioner.get().unwrap().clone();
    let registry = provisioner
        .create_registry_upstream(&ctx(), upstream_at(tenant, "registry.example.com").request)
        .await
        .expect("registry upstream");
    let unrouted = resolve(&registry.alias).await.unwrap_err();
    assert!(
        matches!(
            unrouted,
            DomainError::NotFound {
                entity: "route",
                ..
            }
        ),
        "{unrouted:?}"
    );
    provisioner
        .delete_registry_upstream_if_unused(&ctx(), registry.id)
        .await
        .expect("registry delete");
    let gone = resolve(&registry.alias).await.unwrap_err();
    assert!(
        matches!(
            gone,
            DomainError::NotFound {
                entity: "upstream",
                ..
            }
        ),
        "{gone:?}"
    );
}

/// On a database, `start` connects the cache to the cluster change log:
/// this replica's writes are logged as its own, and another replica's
/// logged change evicts here. After `stop`, writes are no longer logged.
///
/// ADR-0020: CACHE-FRESH-003.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn on_a_database_start_connects_the_cache_to_the_cluster_change_log() {
    let file = SqliteFile::new();
    let db = file.open().await;
    let hub = wired_hub();
    let cluster = ClusterFixture::standalone(&hub);
    let ctx = context(database_section(), Some(db.clone()), Arc::clone(&hub));
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&post_init_ctx()).await.expect("post_init");
    gear.start(tokio_util::sync::CancellationToken::new())
        .await
        .expect("start");
    let log_store = facade(&hub).await;
    let tenant = Uuid::new_v4();
    let sctx = || provisioning_ctx(tenant).expect("security context");
    let gw = hub.get::<dyn ServiceGatewayClientV1>().unwrap();
    let cp = control_plane(&gear);

    let (upstream, route, _) = served_upstream(&*gw, sctx()).await;
    let log = read_log(&log_store).await.expect("the writes are logged");
    assert_eq!(log.seq(), 2);
    assert!(log.changes().all(|logged| logged.origin == gear.owner));

    // Cached here; another replica disables the upstream and logs it.
    let served = (upstream.id, route.id);
    assert_eq!(resolve_served(&*cp, &sctx()).await.expect("served"), served);
    disable_behind(db, tenant, upstream.id).await;
    assert_eq!(
        resolve_served(&*cp, &sctx()).await.expect("cached"),
        served,
        "cached"
    );
    let (other, stop_other) =
        crate::infra::proxy_cache::testing::publisher(&hub, Uuid::new_v4()).await;
    other
        .submit(
            crate::infra::proxy_cache::Change::Upstream {
                tenant_id: tenant,
                id: upstream.id,
                alias: upstream.alias.clone(),
            },
            None,
        )
        .expect("the other replica's publisher runs");
    eventually("the other replica's change evicts here", || async {
        matches!(
            resolve_served(&*cp, &sctx()).await,
            Err(DomainError::UpstreamDisabled { .. })
        )
    })
    .await;
    stop_other.cancel();

    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");
    assert!(gear.change_feed.lock().is_none());
    gw.delete_route(sctx(), route.id)
        .await
        .expect("delete route");
    assert_eq!(read_log(&log_store).await.unwrap().seq(), 3, "not logged");
    cluster.stop().await;
}

/// A profile the gear never resolves.
#[cfg(feature = "cluster")]
#[derive(Debug, Clone, Copy)]
struct OtherProfile;

#[cfg(feature = "cluster")]
impl cluster_sdk::ClusterProfile for OtherProfile {
    const NAME: &'static str = "other";
}

/// Boot a gear on a database with the cluster `wire` puts in its hub, and
/// return why `start` fails; nothing runs after it.
#[cfg(feature = "cluster")]
async fn start_error(wire: impl FnOnce(&Arc<ClientHub>) -> ClusterFixture) -> anyhow::Error {
    let file = SqliteFile::new();
    let hub = wired_hub();
    let cluster = wire(&hub);
    let ctx = context(database_section(), Some(file.open().await), hub);
    let gear = init(&ctx).await.expect("init");
    gear.post_init(&post_init_ctx()).await.expect("post_init");
    let err = gear
        .start(tokio_util::sync::CancellationToken::new())
        .await
        .expect_err("start fails");
    assert!(gear.change_feed.lock().is_none());
    assert!(gear.registry_task.lock().is_none(), "no reconcile either");
    cluster.stop().await;
    err
}

/// On a database, `start` fails when the cluster binds no `oagw` profile,
/// or binds one whose cache cannot watch.
///
/// ADR-0020: CACHE-CONF-001.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn on_a_database_start_needs_a_watchable_oagw_cluster_profile() {
    let unbound = start_error(|hub| ClusterFixture::with(hub, OtherProfile, |cache| cache)).await;
    let message = format!("{unbound:#}");
    assert!(
        message.contains("no backend bound for profile `oagw`"),
        "{message}"
    );
    assert!(
        unbound
            .downcast_ref::<cluster_sdk::ClusterError>()
            .is_some(),
        "the cluster error is kept as the source: {unbound:?}"
    );

    let unwatchable =
        start_error(|hub| ClusterFixture::scripted(hub, ScriptedCache::without_watch).0).await;
    let message = format!("{unwatchable:#}");
    assert!(message.contains("required `Watch`"), "{message}");
}

/// The `gears.oagw` section with a database and this proxy cache config.
fn database_section_with_cache(proxy_cache: Value) -> Value {
    json!({
        "config": { "proxy_cache": proxy_cache },
        "database": { "server": "sqlite_users", "file": "oagw.db" },
    })
}

/// A hub whose registry reconcile settles: root tenant `root`, an empty
/// registry.
fn settling_hub(root: Uuid) -> Arc<ClientHub> {
    hub_over(
        MockTenantResolverClient::with_siblings(TenantId(root), vec![]),
        MockTypesRegistry::new(),
    )
}

/// `post_init` and `start` (returning at once) on `gear`.
async fn boot(gear: &OutboundApiGatewayGear) {
    gear.post_init(&post_init_ctx()).await.expect("post_init");
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        gear.start(tokio_util::sync::CancellationToken::new()),
    )
    .await
    .expect("start returns at once")
    .expect("start");
}

/// With a database and `proxy_cache.enabled: false` there is no cache and
/// no change log: OAGW needs no cluster (none is in the hub, in either
/// build), every lookup reads the database, and the healthcheck has no
/// cache part.
///
/// ADR-0020: CACHE-CONF-002.
#[tokio::test]
async fn with_the_cache_disabled_every_lookup_reads_and_no_cluster_is_needed() {
    use toolkit_db::test_support::QueryKind;

    let (db, recorder) = crate::infra::storage::testing::recorded_sqlite().await;
    let tenant = Uuid::new_v4();
    let hub = settling_hub(tenant);
    let ctx = context(
        database_section_with_cache(json!({ "enabled": false })),
        Some(db),
        Arc::clone(&hub),
    );
    let gear = init(&ctx).await.expect("init");
    #[cfg(feature = "cluster")]
    assert!(matches!(gear.cache_sync.get(), Some(None)), "no cache");
    boot(&gear).await;
    #[cfg(feature = "cluster")]
    assert!(gear.change_feed.lock().is_none(), "no change feed");
    let sctx = || provisioning_ctx(tenant).expect("security context");
    let gw = hub.get::<dyn ServiceGatewayClientV1>().unwrap();
    let cp = control_plane(&gear);

    let (upstream, route, _) = served_upstream(&*gw, sctx()).await;
    let served = (upstream.id, route.id);
    assert_eq!(resolve_served(&*cp, &sctx()).await.expect("served"), served);
    recorder.clear();
    assert_eq!(resolve_served(&*cp, &sctx()).await.expect("served"), served);
    let selects = recorder
        .events()
        .iter()
        .filter(|e| e.kind == QueryKind::Select)
        .count();
    assert!(selects > 0, "a repeated lookup reads the database");

    eventually("healthy without a cluster", || async {
        health(&gear, &ctx).await == toolkit::HealthcheckStatus::Healthy
    })
    .await;
    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");
}

/// Without a database there is no cache: a lookup after a write reads what
/// the write stored, with nothing to evict.
///
/// ADR-0020: CACHE-CONF-002.
#[tokio::test]
async fn without_a_database_every_lookup_reads_the_store() {
    let hub = wired_hub();
    let ctx = context(json!({ "config": {} }), None, Arc::clone(&hub));
    let gear = init(&ctx).await.expect("init");
    #[cfg(feature = "cluster")]
    assert!(matches!(gear.cache_sync.get(), Some(None)), "no cache");
    let tenant = Uuid::new_v4();
    let sctx = || provisioning_ctx(tenant).expect("security context");
    let gw = hub.get::<dyn ServiceGatewayClientV1>().unwrap();
    let cp = control_plane(&gear);

    let (upstream, route, server) = served_upstream(&*gw, sctx()).await;
    assert_eq!(
        resolve_served(&*cp, &sctx()).await.expect("served"),
        (upstream.id, route.id)
    );
    gw.update_upstream(
        sctx(),
        upstream.id,
        oagw_sdk::UpdateUpstreamRequest::builder(server, oagw_sdk::HTTP_PROTOCOL_ID)
            .enabled(false)
            .build(),
    )
    .await
    .expect("update upstream");
    let disabled = resolve_served(&*cp, &sctx()).await.unwrap_err();
    assert!(
        matches!(disabled, DomainError::UpstreamDisabled { .. }),
        "{disabled:?}"
    );
}

/// A remote cluster unreachable at startup: `start` returns at once, the
/// gear reports the watch connecting (Degraded) while the watch is retried,
/// is healthy once it succeeds, and reports the watch down once stopped.
///
/// ADR-0020: CACHE-CONF-004.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn an_unreachable_cluster_at_startup_leaves_the_gear_degraded_until_the_watch_succeeds() {
    use crate::infra::proxy_cache::WatchState;
    use toolkit::HealthcheckStatus;

    let file = SqliteFile::new();
    let hub = settling_hub(Uuid::new_v4());
    let (cluster, scripted) = ClusterFixture::scripted(&hub, |cache| {
        cache.fail_watches(usize::MAX);
        cache
    });
    let ctx = context(database_section(), Some(file.open().await), hub);
    let gear = init(&ctx).await.expect("init");
    boot(&gear).await;

    eventually(
        "provisioning settles, and only the watch is connecting",
        || async {
            health_code(&gear, &ctx).await.as_deref() == Some("proxy_cache_watch_connecting")
        },
    )
    .await;
    assert_eq!(health(&gear, &ctx).await, HealthcheckStatus::Degraded);
    assert!(scripted.watch_calls() >= 1);

    // The registry task now sleeps until its recheck, so no statement runs
    // while the clock is paused through the watch's backoff.
    tokio::time::pause();
    scripted.fail_watches(0);
    eventually("the watch succeeds", || async {
        gear.cache_sync
            .get()
            .and_then(Option::as_ref)
            .is_some_and(|sync| sync.watch.get() == WatchState::Up)
    })
    .await;
    tokio::time::resume();

    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");
    assert_eq!(
        health_code(&gear, &ctx).await.as_deref(),
        Some("proxy_cache_watch_down")
    );
    cluster.stop().await;
}

/// Without a database the gear initializes, starts and is ready with no
/// cluster client and no `oagw` profile in the hub, and the process's
/// cluster readiness is no part of its health.
///
/// ADR-0020: CACHE-CONF-005.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn without_a_database_the_gear_starts_and_is_ready_without_a_cluster() {
    use toolkit::HealthcheckStatus;

    let hub = settling_hub(Uuid::new_v4());
    let ctx = context(json!({ "config": {} }), None, Arc::clone(&hub));
    let gear = init(&ctx).await.expect("init");
    boot(&gear).await;
    assert!(gear.change_feed.lock().is_none());
    assert_eq!(health(&gear, &ctx).await, HealthcheckStatus::Healthy);

    // A resolve with no client makes this process's cluster readiness
    // unready; OAGW in memory does not report it.
    cluster_sdk::ClusterCacheV1::resolver(&ClientHub::new())
        .profile(OtherProfile)
        .resolve()
        .await
        .expect("an unbound facade");
    let readiness = cluster_sdk::cluster_readiness(Arc::clone(&hub))
        .check()
        .await;
    assert_eq!(readiness.status, HealthcheckStatus::Unhealthy);
    assert_eq!(health(&gear, &ctx).await, HealthcheckStatus::Healthy);

    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");
}

/// Without the `cluster` feature a database with the cache enabled (the
/// default) is refused at `init`: the proxy configuration cache on a
/// database needs the cluster change log. The error names the feature,
/// `proxy_cache.enabled: false` and ADR-0020, and the gear registers no
/// client.
///
/// ADR-0020: CACHE-CONF-005.
#[cfg(not(feature = "cluster"))]
#[tokio::test]
async fn without_the_cluster_feature_a_database_is_refused_at_init() {
    let file = SqliteFile::new();
    let hub = wired_hub();
    let ctx = context(
        database_section(),
        Some(file.open().await),
        Arc::clone(&hub),
    );
    let gear = OutboundApiGatewayGear::default();
    let err = gear.init(&ctx).await.expect_err("a database is refused");
    let message = err.to_string();
    assert!(message.contains("no `cluster` feature"), "{message}");
    assert!(message.contains("proxy_cache.enabled: false"), "{message}");
    assert!(message.contains("ADR-0020"), "{message}");
    assert!(gear.state.load().is_none(), "no state");
    assert!(
        hub.get::<dyn ServiceGatewayClientV1>().is_err(),
        "no client registered"
    );
}

/// Without the `cluster` feature OAGW runs in memory: it initializes,
/// starts, is ready and stops, with no cluster in the process.
///
/// ADR-0020: CACHE-CONF-005.
#[cfg(not(feature = "cluster"))]
#[tokio::test]
async fn without_the_cluster_feature_the_gear_runs_in_memory() {
    use toolkit::HealthcheckStatus;

    let hub = settling_hub(Uuid::new_v4());
    let ctx = context(json!({ "config": {} }), None, Arc::clone(&hub));
    let gear = init(&ctx).await.expect("init");
    boot(&gear).await;
    let result = health_result(&gear, &ctx).await;
    assert_eq!(result.status, HealthcheckStatus::Healthy, "{result:?}");
    assert!(hub.get::<dyn ServiceGatewayClientV1>().is_ok());
    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");
}

/// A healthcheck part that answers `result`.
struct FixedHealth(toolkit::HealthcheckResult);

#[async_trait]
impl toolkit::Healthcheck for FixedHealth {
    fn name(&self) -> &'static str {
        "fixed"
    }

    async fn check(&self) -> toolkit::HealthcheckResult {
        self.0.clone()
    }
}

/// What `ClusterHealth` makes of the cluster requirements' `result`.
#[cfg(feature = "cluster")]
async fn cluster_part(result: toolkit::HealthcheckResult) -> toolkit::HealthcheckResult {
    ClusterHealth::new(Arc::new(FixedHealth(result)))
        .check()
        .await
}

/// A cluster still starting, or a profile whose backend is degraded, leaves
/// OAGW Degraded with the SDK's message and code: the TTL bounds staleness
/// and proxying continues. A misconfiguration stays Unhealthy.
///
/// ADR-0020: CACHE-CONF-004.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn the_cluster_requirements_degrade_oagw_unless_misconfigured() {
    use cluster_sdk::requirements::{CODE_MISCONFIGURED, CODE_PROFILE_DEGRADED, CODE_STARTING};
    use toolkit::{HealthcheckResult, HealthcheckStatus};

    for code in [CODE_STARTING, CODE_PROFILE_DEGRADED] {
        let result = cluster_part(HealthcheckResult::unhealthy("cause").with_code(code)).await;
        assert_eq!(result.status, HealthcheckStatus::Degraded, "{code}");
        assert_eq!(result.code.as_deref(), Some(code));
        assert_eq!(result.message.as_deref(), Some("cause"));
    }
    let misconfigured =
        cluster_part(HealthcheckResult::unhealthy("cause").with_code(CODE_MISCONFIGURED)).await;
    assert_eq!(misconfigured.status, HealthcheckStatus::Unhealthy);
    assert_eq!(misconfigured.code.as_deref(), Some(CODE_MISCONFIGURED));
    let ready = cluster_part(HealthcheckResult::healthy()).await;
    assert_eq!(ready.status, HealthcheckStatus::Healthy);
}

/// A cluster readiness check that answers `result` after `delay`, or
/// panics, counting its runs.
#[cfg(feature = "cluster")]
struct ScriptedHealth {
    delay: parking_lot::Mutex<std::time::Duration>,
    result: toolkit::HealthcheckResult,
    panics: std::sync::atomic::AtomicBool,
    runs: std::sync::atomic::AtomicUsize,
}

#[cfg(feature = "cluster")]
impl ScriptedHealth {
    fn new(delay: std::time::Duration, result: toolkit::HealthcheckResult) -> Arc<Self> {
        Arc::new(Self {
            delay: parking_lot::Mutex::new(delay),
            result,
            panics: std::sync::atomic::AtomicBool::new(false),
            runs: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn runs(&self) -> usize {
        self.runs.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(feature = "cluster")]
#[async_trait]
impl toolkit::Healthcheck for ScriptedHealth {
    fn name(&self) -> &'static str {
        "scripted"
    }

    async fn check(&self) -> toolkit::HealthcheckResult {
        self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let delay = *self.delay.lock();
        tokio::time::sleep(delay).await;
        assert!(
            !self.panics.load(std::sync::atomic::Ordering::SeqCst),
            "scripted panic"
        );
        self.result.clone()
    }
}

/// A cluster readiness check slower than OAGW's budget does not hold up
/// OAGW's healthcheck: it answers within 250 ms, Degraded
/// `cluster_readiness_slow`, while the check runs on; once it ends, the
/// next call answers with its result, mapped.
///
/// ADR-0020: CACHE-CONF-004.
#[cfg(feature = "cluster")]
#[tokio::test(start_paused = true)]
async fn a_slow_cluster_check_does_not_hold_up_the_gear_health() {
    use cluster_sdk::requirements::CODE_STARTING;
    use toolkit::{HealthcheckResult, HealthcheckStatus};

    let sdk = ScriptedHealth::new(
        std::time::Duration::from_secs(2),
        HealthcheckResult::unhealthy("starting").with_code(CODE_STARTING),
    );
    let health = OagwHealth(vec![Arc::new(ClusterHealth::new(sdk.clone()))]);

    let started = tokio::time::Instant::now();
    let slow = health.check().await;
    assert!(
        started.elapsed() <= CLUSTER_CHECK_BUDGET,
        "{:?}",
        started.elapsed()
    );
    assert_eq!(slow.status, HealthcheckStatus::Degraded);
    assert_eq!(slow.code.as_deref(), Some(CODE_CLUSTER_READINESS_SLOW));

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let ended = health.check().await;
    assert_eq!(ended.status, HealthcheckStatus::Degraded);
    assert_eq!(ended.code.as_deref(), Some(CODE_STARTING));
    assert_eq!(sdk.runs(), 1, "the same check, waited for again");
}

/// While a check runs past the budget, OAGW answers with the last result,
/// for up to 10 s after the check began; then `cluster_readiness_slow`.
///
/// ADR-0020: CACHE-CONF-004.
#[cfg(feature = "cluster")]
#[tokio::test(start_paused = true)]
async fn a_slow_cluster_check_reuses_the_last_result_for_ten_seconds() {
    use toolkit::{HealthcheckResult, HealthcheckStatus};

    let sdk = ScriptedHealth::new(std::time::Duration::ZERO, HealthcheckResult::healthy());
    let health = ClusterHealth::new(sdk.clone());
    assert_eq!(health.check().await.status, HealthcheckStatus::Healthy);

    *sdk.delay.lock() = std::time::Duration::from_secs(60);
    assert_eq!(health.check().await.status, HealthcheckStatus::Healthy);
    tokio::time::sleep(CLUSTER_CHECK_STALE - std::time::Duration::from_secs(1)).await;
    assert_eq!(health.check().await.status, HealthcheckStatus::Healthy);

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let slow = health.check().await;
    assert_eq!(slow.status, HealthcheckStatus::Degraded);
    assert_eq!(slow.code.as_deref(), Some(CODE_CLUSTER_READINESS_SLOW));
    assert_eq!(sdk.runs(), 2, "one check runs at a time");
}

/// A cluster readiness check that panics leaves OAGW Degraded with the
/// panic in the message, and the next call runs a new check.
///
/// ADR-0020: CACHE-CONF-004.
#[cfg(feature = "cluster")]
#[tokio::test(start_paused = true)]
async fn a_panicking_cluster_check_is_degraded_and_run_again() {
    use toolkit::{HealthcheckResult, HealthcheckStatus};

    let sdk = ScriptedHealth::new(std::time::Duration::ZERO, HealthcheckResult::healthy());
    sdk.panics.store(true, std::sync::atomic::Ordering::SeqCst);
    let health = ClusterHealth::new(sdk.clone());
    let panicked = health.check().await;
    assert_eq!(panicked.status, HealthcheckStatus::Degraded);
    assert_eq!(panicked.code.as_deref(), Some(CODE_CLUSTER_READINESS_SLOW));
    let message = panicked.message.unwrap_or_default();
    assert!(message.contains("panic"), "{message}");

    sdk.panics.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(health.check().await.status, HealthcheckStatus::Healthy);
    assert_eq!(sdk.runs(), 2);
}

/// On a tie the cache's parts come before provisioning: with provisioning
/// not clean and the change log watch down, both Degraded, the code is the
/// watch's, and the message names both.
///
/// ADR-0020: CACHE-CONF-004.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn on_a_tie_the_watch_code_wins_over_provisioning() {
    use crate::infra::proxy_cache::WatchState;
    use toolkit::HealthcheckStatus;

    let file = SqliteFile::new();
    let ctx = context(database_section(), Some(file.open().await), wired_hub());
    let gear = init(&ctx).await.expect("init");
    gear.provisioning.set(ProvisioningStatus::NotClean);
    let Some(Some(sync)) = gear.cache_sync.get() else {
        panic!("a database connects the cache to the cluster");
    };
    sync.watch.set(WatchState::Down);

    let result = health_result(&gear, &ctx).await;
    assert_eq!(result.status, HealthcheckStatus::Degraded);
    assert_eq!(result.code.as_deref(), Some("proxy_cache_watch_down"));
    let message = result.message.unwrap_or_default();
    assert!(message.contains("watch is down"), "{message}");
    assert!(message.contains("registry provisioning"), "{message}");
}

/// The worst part decides the status and the code (the earlier on a tie);
/// the message names every part that is not healthy, in order.
#[tokio::test]
async fn the_gear_health_names_every_part_that_is_not_healthy() {
    use toolkit::{HealthcheckResult, HealthcheckStatus};

    let parts = |results: Vec<HealthcheckResult>| {
        OagwHealth(
            results
                .into_iter()
                .map(|r| Arc::new(FixedHealth(r)) as Arc<dyn Healthcheck>)
                .collect(),
        )
    };
    let two_degraded = parts(vec![
        HealthcheckResult::degraded("first").with_code("a"),
        HealthcheckResult::healthy(),
        HealthcheckResult::degraded("second").with_code("b"),
    ])
    .check()
    .await;
    assert_eq!(two_degraded.status, HealthcheckStatus::Degraded);
    assert_eq!(two_degraded.code.as_deref(), Some("a"));
    assert_eq!(two_degraded.message.as_deref(), Some("first; second"));

    let worst_later = parts(vec![
        HealthcheckResult::degraded("first").with_code("a"),
        HealthcheckResult::unhealthy("second").with_code("b"),
    ])
    .check()
    .await;
    assert_eq!(worst_later.status, HealthcheckStatus::Unhealthy);
    assert_eq!(worst_later.code.as_deref(), Some("b"));
    assert_eq!(worst_later.message.as_deref(), Some("first; second"));

    let one = parts(vec![
        HealthcheckResult::healthy(),
        HealthcheckResult::degraded("only").with_code("a"),
    ])
    .check()
    .await;
    assert_eq!(one.message.as_deref(), Some("only"));
}

/// A second `start` without a `stop` ends the change feed and the registry
/// task the first began before it starts new ones.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn a_second_start_stops_what_the_first_began() {
    let file = SqliteFile::new();
    let hub = settling_hub(Uuid::new_v4());
    let cluster = ClusterFixture::standalone(&hub);
    let ctx = context(database_section(), Some(file.open().await), hub);
    let gear = init(&ctx).await.expect("init");
    boot(&gear).await;
    let first_feed = gear
        .change_feed
        .lock()
        .as_ref()
        .expect("a feed")
        .cancel_token();
    let first_task = gear
        .registry_task
        .lock()
        .as_ref()
        .expect("a registry task")
        .0
        .clone();

    gear.start(tokio_util::sync::CancellationToken::new())
        .await
        .expect("start again");
    assert!(first_feed.is_cancelled(), "the first feed is stopped");
    assert!(
        first_task.is_cancelled(),
        "the first registry task is stopped"
    );
    let feed = gear
        .change_feed
        .lock()
        .as_ref()
        .expect("a feed")
        .cancel_token();
    assert!(!feed.is_cancelled(), "a new feed runs");
    let task = gear
        .registry_task
        .lock()
        .as_ref()
        .expect("a task")
        .0
        .clone();
    assert!(!task.is_cancelled(), "a new registry task runs");

    gear.stop(tokio_util::sync::CancellationToken::new())
        .await
        .expect("stop");
    cluster.stop().await;
}

/// The gear builds the cache from `proxy_cache`: with `max_entries: 1` a
/// second alias evicts the first, and with `ttl: 1s` an entry is read again
/// after a second.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn the_cache_takes_its_ttl_and_size_from_the_config() {
    use toolkit_db::test_support::QueryKind;

    let (db, recorder) = crate::infra::storage::testing::recorded_sqlite().await;
    let tenant = Uuid::new_v4();
    let hub = settling_hub(tenant);
    let ctx = context(
        database_section_with_cache(json!({ "ttl": "1s", "max_entries": 1 })),
        Some(db),
        Arc::clone(&hub),
    );
    let gear = init(&ctx).await.expect("init");
    let sctx = || provisioning_ctx(tenant).expect("security context");
    let gw = hub.get::<dyn ServiceGatewayClientV1>().unwrap();
    let cp = control_plane(&gear);
    let (upstream, route, _) = served_upstream(&*gw, sctx()).await;
    let served = (upstream.id, route.id);
    let selects_of = async |alias: &str| {
        recorder.clear();
        let _ = cp
            .resolve_proxy_target(
                &crate::domain::services::ProxyGrant::for_tests(sctx().clone()),
                alias,
                "POST",
                "/v1/things",
                Tags::Skip,
            )
            .await;
        recorder
            .events()
            .iter()
            .filter(|e| e.kind == QueryKind::Select)
            .count()
    };

    assert_eq!(resolve_served(&*cp, &sctx()).await.expect("served"), served);
    assert_eq!(selects_of("api.example.com").await, 0, "cached");
    assert!(selects_of("other.example.com").await > 0);
    assert_eq!(selects_of("other.example.com").await, 0, "cached");
    assert!(
        selects_of("api.example.com").await > 0,
        "evicted: one entry per section"
    );

    assert_eq!(selects_of("api.example.com").await, 0, "cached");
    // No statement runs while the clock is paused.
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    tokio::time::resume();
    assert!(selects_of("api.example.com").await > 0, "expired");
}
