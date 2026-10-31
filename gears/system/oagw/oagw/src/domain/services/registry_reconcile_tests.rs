//! The registry reconcile against a Control Plane: what each run creates,
//! updates, moves, removes, keeps and skips, the passes, and the error rule.
//! Cases that hold on every store run on memory and `SQLite`, and on
//! `PostgreSQL` and `MySQL` behind the `integration` feature, each on a fresh
//! store (the reconcile reads every tenant's registry rows). Runs of a real
//! gear are in `gear_tests.rs`, which shares the fixtures here.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::CredStoreClientV1;
use tenant_resolver_sdk::{TenantId, TenantResolverClient};
use tokio_util::sync::CancellationToken;
use toolkit_db::test_support::{QueryKind, QueryRecorder};
use toolkit_db::{DBProvider, Db};
use tracing_test::traced_test;
use uuid::Uuid;

use super::*;
use crate::domain::model::{
    AuthConfig, Endpoint, GrpcMatch, HttpMatch, HttpMethod, MatchRules, PathSuffixMode, Scheme,
    Server, SharingMode,
};
use crate::domain::ports::metric_labels::provisioning_reason as why;
use crate::domain::repo::{
    RegistryRowRepository, RegistryState, RegistryStateStore, RepositoryError, RouteRepository,
    Tags, UpstreamRepository,
};
use crate::domain::services::registry_state::{
    Gate, LEASE_POLL, LEASE_TTL, RunOutcome, content_hash, gated_reconcile,
};
use crate::domain::services::{
    ControlPlaneService, ControlPlaneServiceImpl, RegistryProvisionerImpl,
};
use crate::domain::ssrf::SsrfGuard;
use crate::domain::test_support::{
    ErroringCredStore, MockCredStoreClient, MockTenantResolverClient, ProvisioningMetrics,
    UnauthorizedTenantResolver, UnavailableStorage, allow_all_enforcer,
};
use crate::infra::proxy_cache::testing::CacheMetrics;
use crate::infra::proxy_cache::{
    CachedProxyConfigReader, FlushingRegistryRowRepo, FlushingRouteRepo, FlushingUpstreamRepo,
    ProxyConfigCache, WriteHook,
};
use crate::infra::storage::registry_state_in_memory::InMemoryRegistryStateStore;
use crate::infra::storage::testing::recorded_sqlite;
use crate::infra::storage::{
    DbRegistryRowRepo, DbRegistryStateStore, DbRouteRepo, DbUpstreamRepo, InMemoryRegistryRowRepo,
    InMemoryRouteRepo, InMemoryUpstreamRepo,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// An upstream instance at `host` (alias derived from it), new ID.
pub(crate) fn upstream_at(tenant_id: Uuid, host: &str) -> ProvisionedUpstream {
    ProvisionedUpstream {
        tenant_id: Some(tenant_id),
        request: CreateUpstreamRequest {
            id: Some(Uuid::new_v4()),
            server: Server {
                endpoints: vec![Endpoint {
                    scheme: Scheme::Https,
                    host: host.into(),
                    port: 443,
                }],
            },
            protocol: oagw_sdk::HTTP_PROTOCOL_ID.into(),
            alias: None,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            enabled: true,
        },
    }
}

/// A route instance on `upstream` at `path` (POST), new ID.
pub(crate) fn route_on(upstream: &ProvisionedUpstream, path: &str) -> ProvisionedRoute {
    ProvisionedRoute {
        tenant_id: upstream.tenant_id,
        request: CreateRouteRequest {
            id: Some(Uuid::new_v4()),
            upstream_id: upstream.request.id.unwrap(),
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    methods: vec![HttpMethod::Post],
                    path: path.into(),
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::Append,
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
        },
    }
}

/// One upstream at `api.example.com` with one route at `/v1`.
pub(crate) fn registry_instances(
    tenant_id: Uuid,
) -> (Vec<ProvisionedUpstream>, Vec<ProvisionedRoute>) {
    let upstream = upstream_at(tenant_id, "api.example.com");
    let route = route_on(&upstream, "/v1");
    (vec![upstream], vec![route])
}

pub(crate) const NONE: Tally = Tally {
    created: 0,
    updated: 0,
    unchanged: 0,
    removed: 0,
    kept: 0,
    failed: 0,
};

pub(crate) const CREATED_ALL: ProvisioningCounts = ProvisioningCounts {
    upstreams: Tally { created: 1, ..NONE },
    routes: Tally { created: 1, ..NONE },
    passes: 2,
    stopped: false,
};

pub(crate) const UNCHANGED_ALL: ProvisioningCounts = ProvisioningCounts {
    upstreams: Tally {
        unchanged: 1,
        ..NONE
    },
    routes: Tally {
        unchanged: 1,
        ..NONE
    },
    passes: 2,
    stopped: false,
};

/// A second registry route on the same upstream as `of`.
pub(crate) fn sibling_route(of: &ProvisionedRoute, path: &str) -> ProvisionedRoute {
    let mut route = of.clone();
    route.request.id = Some(Uuid::new_v4());
    set_path(&mut route, path);
    route
}

pub(crate) fn set_path(route: &mut ProvisionedRoute, path: &str) {
    route.request.match_rules.http.as_mut().unwrap().path = path.into();
}

fn set_host(upstream: &mut ProvisionedUpstream, host: &str) {
    upstream.request.server.endpoints[0].host = host.into();
}

fn id(upstream: &ProvisionedUpstream) -> Uuid {
    upstream.request.id.unwrap()
}

fn rid(route: &ProvisionedRoute) -> Uuid {
    route.request.id.unwrap()
}

/// The store a case runs on.
pub(crate) enum Backend {
    Memory,
    Db(Db),
}

/// The repositories of one store, and the registry rows over them.
#[derive(Clone)]
pub(crate) struct Stores {
    pub(crate) upstreams: Arc<dyn UpstreamRepository>,
    pub(crate) routes: Arc<dyn RouteRepository>,
    pub(crate) rows: Arc<dyn RegistryRowRepository>,
}

impl Stores {
    pub(crate) fn new(backend: Backend) -> Self {
        match backend {
            Backend::Memory => Self::in_memory(
                Arc::new(InMemoryUpstreamRepo::new()),
                Arc::new(InMemoryRouteRepo::new()),
            ),
            Backend::Db(db) => {
                let db = DBProvider::<RepositoryError>::new(db);
                Self {
                    upstreams: Arc::new(DbUpstreamRepo::new(db.clone())),
                    routes: Arc::new(DbRouteRepo::new(db.clone())),
                    rows: Arc::new(DbRegistryRowRepo::new(db)),
                }
            }
        }
    }

    fn in_memory(upstreams: Arc<InMemoryUpstreamRepo>, routes: Arc<InMemoryRouteRepo>) -> Self {
        Self {
            rows: Arc::new(InMemoryRegistryRowRepo::new(
                upstreams.clone(),
                routes.clone(),
            )),
            upstreams,
            routes,
        }
    }
}

/// A Control Plane and its registry write path over one store.
pub(crate) fn planes_over(
    stores: Stores,
    tenants: Arc<dyn TenantResolverClient>,
    credstore: Arc<dyn CredStoreClientV1>,
) -> (Arc<ControlPlaneServiceImpl>, Arc<RegistryProvisionerImpl>) {
    planes_guarded(stores, tenants, credstore, SsrfGuard::disabled())
}

/// [`planes_over`] with the SSRF policy `ssrf`.
fn planes_guarded(
    stores: Stores,
    tenants: Arc<dyn TenantResolverClient>,
    credstore: Arc<dyn CredStoreClientV1>,
    ssrf: SsrfGuard,
) -> (Arc<ControlPlaneServiceImpl>, Arc<RegistryProvisionerImpl>) {
    let cp = Arc::new(ControlPlaneServiceImpl::new(
        stores.upstreams,
        stores.routes,
        tenants,
        allow_all_enforcer(),
        credstore,
        Arc::new(ssrf),
        None,
    ));
    let provisioner = Arc::new(RegistryProvisionerImpl::new(cp.clone(), stores.rows));
    (cp, provisioner)
}

/// One proxy lookup: the tenant, the alias, and the method and path to
/// match, if any.
pub(crate) type Lookup<'a> = (Uuid, &'a str, Option<(&'a str, &'a str)>);

/// What a lookup resolves to, as a comparable value: the effective upstream
/// and route, or the error.
pub(crate) type Resolved = Result<(Upstream, Option<Route>), Failed>;

/// A lookup's error, kept typed so a test can match its variant.
/// `DomainError` has no `PartialEq`; two are equal when their debug forms,
/// which spell out every field, are.
#[derive(Debug, Clone)]
pub(crate) struct Failed(pub(crate) crate::domain::error::DomainError);

impl PartialEq for Failed {
    fn eq(&self, other: &Self) -> bool {
        format!("{:?}", self.0) == format!("{:?}", other.0)
    }
}

/// Whether `answer` is "not found": no upstream with the alias, or no route.
pub(crate) fn is_not_found(answer: &Resolved) -> bool {
    matches!(
        answer,
        Err(Failed(crate::domain::error::DomainError::NotFound { .. }))
    )
}

/// Whether `answer` is "disabled": the visible upstreams are all disabled.
pub(crate) fn is_disabled(answer: &Resolved) -> bool {
    matches!(
        answer,
        Err(Failed(
            crate::domain::error::DomainError::UpstreamDisabled { .. }
        ))
    )
}

/// A Control Plane and its registry write path wired as the gear wires them
/// on a database (ADR-0020): proxy resolution reads through a proxy
/// configuration cache, and every write evicts from it. `uncached` reads
/// the same store directly; the store's statements are recorded.
pub(crate) struct CachedPlanes {
    pub(crate) cp: Arc<ControlPlaneServiceImpl>,
    pub(crate) provisioner: Arc<RegistryProvisionerImpl>,
    pub(crate) uncached: Arc<ControlPlaneServiceImpl>,
    /// The hook its writes go through (holding the cache), to connect them
    /// to a cluster change log.
    #[cfg(feature = "cluster")]
    pub(crate) hook: WriteHook,
    /// What its cache records.
    #[cfg(feature = "cluster")]
    pub(crate) metrics: Arc<CacheMetrics>,
    recorder: QueryRecorder,
}

impl CachedPlanes {
    /// On a fresh recorded `SQLite` database, over the tenants `tenants`.
    pub(crate) async fn new(tenants: Arc<dyn TenantResolverClient>) -> Self {
        let (db, recorder) = recorded_sqlite().await;
        Self::over(Stores::new(Backend::Db(db)), recorder, tenants)
    }

    /// Over `stores`, whose statements `recorder` records.
    pub(crate) fn over(
        stores: Stores,
        recorder: QueryRecorder,
        tenants: Arc<dyn TenantResolverClient>,
    ) -> Self {
        Self::with_ttl(stores, recorder, tenants, Duration::from_secs(60))
    }

    /// [`over`](Self::over), with cache entries living for `ttl`.
    pub(crate) fn with_ttl(
        stores: Stores,
        recorder: QueryRecorder,
        tenants: Arc<dyn TenantResolverClient>,
        ttl: Duration,
    ) -> Self {
        let credstore: Arc<dyn CredStoreClientV1> = Arc::new(MockCredStoreClient::empty());
        let metrics = Arc::new(CacheMetrics::default());
        let cache = Arc::new(ProxyConfigCache::new(ttl, 100, metrics.clone()));
        let reader = CachedProxyConfigReader::new(
            Arc::clone(&cache),
            stores.upstreams.clone(),
            stores.routes.clone(),
        );
        let hook = WriteHook::new(cache);
        let cp = Arc::new(ControlPlaneServiceImpl::new(
            Arc::new(FlushingUpstreamRepo::new(
                stores.upstreams.clone(),
                hook.clone(),
            )),
            Arc::new(FlushingRouteRepo::new(stores.routes.clone(), hook.clone())),
            tenants.clone(),
            allow_all_enforcer(),
            credstore.clone(),
            Arc::new(SsrfGuard::disabled()),
            Some(Arc::new(reader)),
        ));
        let provisioner = Arc::new(RegistryProvisionerImpl::new(
            cp.clone(),
            Arc::new(FlushingRegistryRowRepo::new(
                stores.rows.clone(),
                hook.clone(),
            )),
        ));
        let (uncached, _) = planes_over(stores, tenants, credstore);
        Self {
            cp,
            provisioner,
            uncached,
            #[cfg(feature = "cluster")]
            hook,
            #[cfg(feature = "cluster")]
            metrics,
            recorder,
        }
    }

    /// `SELECT`s the store has run so far.
    pub(crate) fn selects(&self) -> usize {
        self.recorder
            .events()
            .iter()
            .filter(|e| e.kind == QueryKind::Select)
            .count()
    }

    /// Resolve `lookup` through the cache, and check that the answer is
    /// cached: a second lookup answers the same and runs no `SELECT`.
    pub(crate) async fn cached(&self, lookup: Lookup<'_>) -> Resolved {
        let first = resolve(&self.cp, lookup).await;
        let selects = self.selects();
        let second = resolve(&self.cp, lookup).await;
        assert_eq!(self.selects(), selects, "{lookup:?} is cached");
        assert_eq!(second, first, "{lookup:?}");
        second
    }

    /// Run `write` and check that the next lookups through the cache see
    /// it: before `write` each lookup's answer is cached; after it, each
    /// answers as without the cache, and differently from before.
    pub(crate) async fn assert_sees<T>(
        &self,
        name: &str,
        lookups: &[Lookup<'_>],
        write: impl AsyncFnOnce() -> T,
    ) -> T {
        let mut before = Vec::with_capacity(lookups.len());
        for &lookup in lookups {
            before.push(self.cached(lookup).await);
        }
        let written = write().await;
        for (&lookup, before) in lookups.iter().zip(before) {
            let after = resolve(&self.cp, lookup).await;
            assert_eq!(
                after,
                resolve(&self.uncached, lookup).await,
                "{name}: {lookup:?} through the cache"
            );
            assert_ne!(after, before, "{name} changes {lookup:?}");
        }
        written
    }
}

/// What `cp` resolves `lookup` to, without tags.
pub(crate) async fn resolve(
    cp: &ControlPlaneServiceImpl,
    (tenant, alias, target): Lookup<'_>,
) -> Resolved {
    let ctx = provisioning_ctx(tenant).expect("security context");
    let chain = cp.build_tenant_chain(&ctx).await.map_err(Failed)?;
    cp.resolve_alias(&ctx, &chain, alias, target, Tags::Skip)
        .await
        .map_err(Failed)
}

/// A Control Plane and its registry write path in memory, one tenant.
pub(crate) fn in_memory_planes() -> (Arc<ControlPlaneServiceImpl>, Arc<RegistryProvisionerImpl>) {
    planes_over(
        Stores::new(Backend::Memory),
        Arc::new(MockTenantResolverClient::single_tenant()),
        Arc::new(MockCredStoreClient::empty()),
    )
}

/// A Control Plane and its registry write path over one store, and the
/// metrics the reconcile records.
struct Fixture {
    cp: Arc<ControlPlaneServiceImpl>,
    provisioner: Arc<RegistryProvisionerImpl>,
    metrics: ProvisioningMetrics,
}

impl Fixture {
    fn new(backend: Backend) -> Self {
        Self::with(
            backend,
            Arc::new(MockTenantResolverClient::single_tenant()),
            Arc::new(MockCredStoreClient::empty()),
        )
    }

    fn with(
        backend: Backend,
        tenants: Arc<dyn TenantResolverClient>,
        credstore: Arc<dyn CredStoreClientV1>,
    ) -> Self {
        Self::over(Stores::new(backend), tenants, credstore)
    }

    fn over(
        stores: Stores,
        tenants: Arc<dyn TenantResolverClient>,
        credstore: Arc<dyn CredStoreClientV1>,
    ) -> Self {
        let (cp, provisioner) = planes_over(stores, tenants, credstore);
        Self {
            cp,
            provisioner,
            metrics: ProvisioningMetrics::default(),
        }
    }

    /// One tenant, under the SSRF policy `ssrf`.
    fn guarded(backend: Backend, ssrf: SsrfGuard) -> Self {
        let (cp, provisioner) = planes_guarded(
            Stores::new(backend),
            Arc::new(MockTenantResolverClient::single_tenant()),
            Arc::new(MockCredStoreClient::empty()),
            ssrf,
        );
        Self {
            cp,
            provisioner,
            metrics: ProvisioningMetrics::default(),
        }
    }

    async fn run(
        &self,
        upstreams: &[ProvisionedUpstream],
        routes: &[ProvisionedRoute],
        root: Uuid,
    ) -> anyhow::Result<ProvisioningCounts> {
        reconcile_registry(
            &*self.provisioner,
            &self.metrics,
            &Unattended,
            upstreams,
            routes,
            root,
        )
        .await
    }

    async fn ok(
        &self,
        upstreams: &[ProvisionedUpstream],
        routes: &[ProvisionedRoute],
        root: Uuid,
    ) -> ProvisioningCounts {
        self.run(upstreams, routes, root)
            .await
            .expect("only an infrastructure error fails a run")
    }

    async fn upstream(&self, tenant: Uuid, id: Uuid) -> Option<Upstream> {
        self.cp
            .get_upstream(&provisioning_ctx(tenant).unwrap(), id)
            .await
            .ok()
    }

    async fn route(&self, tenant: Uuid, id: Uuid) -> Option<Route> {
        self.cp
            .get_route(&provisioning_ctx(tenant).unwrap(), id)
            .await
            .ok()
    }

    async fn alias(&self, tenant: Uuid, id: Uuid) -> String {
        self.upstream(tenant, id).await.expect("upstream").alias
    }

    async fn path(&self, tenant: Uuid, id: Uuid) -> String {
        self.route(tenant, id)
            .await
            .expect("route")
            .match_rules
            .http
            .expect("HTTP route")
            .path
    }

    /// A Management API route on `upstream` at `path`, with `id` if given.
    async fn api_route(&self, tenant: Uuid, upstream: Uuid, path: &str, id: Option<Uuid>) -> Route {
        let mut req = route_on(&upstream_at(tenant, "x.example.com"), path).request;
        req.id = id;
        req.upstream_id = upstream;
        self.cp
            .create_route(&provisioning_ctx(tenant).unwrap(), req)
            .await
            .expect("API route")
    }

    /// A Management API upstream at `host`, with `id` if given.
    async fn api_upstream(&self, tenant: Uuid, host: &str, id: Option<Uuid>) -> Upstream {
        let mut req = upstream_at(tenant, host).request;
        req.id = id;
        self.cp
            .create_upstream(&provisioning_ctx(tenant).unwrap(), req)
            .await
            .expect("API upstream")
    }

    fn failures(&self) -> Vec<(String, String)> {
        self.metrics.failures()
    }
}

fn failure(kind: &str, reason: &str) -> (String, String) {
    (kind.to_owned(), reason.to_owned())
}

// ---------------------------------------------------------------------------
// Cases on every store
// ---------------------------------------------------------------------------

/// Instantiate every case on memory, `SQLite`, and (one test per server,
/// a fresh database per case) `PostgreSQL` and `MySQL`.
macro_rules! reconcile_cases {
    ($($case:ident),+ $(,)?) => {
        mod in_memory {
            $(
                #[tokio::test]
                async fn $case() {
                    super::$case(super::Backend::Memory).await;
                }
            )+
        }

        mod sqlite {
            $(
                #[tokio::test]
                async fn $case() {
                    let db = crate::infra::storage::testing::shared_sqlite().await;
                    super::$case(super::Backend::Db(db)).await;
                }
            )+
        }

        #[cfg(feature = "integration")]
        mod postgres {
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn all_cases() {
                let server = crate::infra::storage::testing::postgres_server().await;
                $(super::$case(super::Backend::Db(server.fresh().await)).await;)+
            }
        }

        #[cfg(feature = "integration")]
        mod mysql {
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn all_cases() {
                let server = crate::infra::storage::testing::mysql_server().await;
                $(super::$case(super::Backend::Db(server.fresh().await)).await;)+
            }
        }
    };
}

reconcile_cases!(
    provisioning_twice_leaves_existing_instances_unchanged,
    provisioning_applies_changed_registry_content,
    provisioning_changes_the_alias_of_a_registry_upstream,
    an_alias_chain_resolves_in_one_run_in_any_listing_order,
    a_chain_listed_in_reverse_takes_a_pass_per_link,
    an_alias_swap_is_skipped_as_a_cycle,
    an_alias_held_by_an_api_upstream_skips_the_instance,
    an_api_row_holding_an_upstream_id_skips_the_instance,
    an_api_row_holding_a_route_id_skips_the_instance,
    a_hierarchy_rejection_skips_and_the_old_config_serves,
    the_content_check_skips_bad_instances_and_keeps_their_rows,
    every_kind_of_bad_content_is_skipped_and_keeps_its_row,
    a_path_hand_over_resolves_in_one_run_in_any_listing_order,
    a_path_swap_is_skipped_as_a_cycle,
    a_route_moves_to_another_upstream_in_place,
    a_move_to_a_skipped_upstream_keeps_the_route_serving,
    the_stale_row_of_a_skipped_route_blocks_its_path,
    a_route_on_an_api_upstream_is_skipped,
    registry_and_api_routes_still_conflict,
    provisioning_removes_instances_the_registry_no_longer_has,
    a_dropped_upstream_that_api_routes_use_is_kept_until_they_go,
    a_route_follows_its_upstream_to_a_replacement_with_the_same_alias,
    a_route_leaving_a_kept_upstream_serves_there_until_its_move_succeeds,
    a_route_moving_tenant_off_a_kept_upstream_serves_there_until_it_can_move,
    provisioning_moves_an_instance_to_its_new_tenant,
    provisioning_after_a_partial_run_creates_only_what_is_missing,
    a_credstore_outage_aborts_and_a_refusal_skips,
    a_tenant_resolver_refusing_oagw_aborts,
    a_stop_inside_the_upstream_pass_leaves_the_rest_to_the_next_run,
    a_stop_between_upstreams_and_routes_leaves_the_routes_to_the_next_run,
    a_stop_inside_the_route_pass_leaves_the_rest_to_the_next_run,
    a_skipped_route_keeps_its_dropped_upstream_until_its_content_is_fixed,
    a_route_moving_to_a_skipped_upstream_keeps_its_dropped_upstream,
    a_stop_after_a_moving_routes_delete_leaves_the_rest_to_the_next_run,
    a_route_naming_its_dropped_upstream_keeps_it_serving,
    api_writes_to_registry_rows_are_rejected,
    a_lease_lost_inside_the_upstream_pass_leaves_the_rest_to_the_next_run,
    a_lease_lost_after_the_upstream_pass_leaves_the_routes_to_the_next_run,
    a_shutdown_inside_the_route_pass_leaves_the_rest_to_the_next_run,
);

async fn provisioning_twice_leaves_existing_instances_unchanged(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    assert_eq!(f.ok(&upstreams, &routes, root).await, CREATED_ALL);
    assert_eq!(f.ok(&upstreams, &routes, root).await, UNCHANGED_ALL);
    let upstream = f.upstream(root, id(&upstreams[0])).await.unwrap();
    let route = f.route(root, rid(&routes[0])).await.unwrap();
    assert_eq!(
        (upstream.managed_by, route.managed_by),
        (ManagedBy::Registry, ManagedBy::Registry)
    );
    assert!(f.failures().is_empty());
}

/// Registry content changed since the stored rows were written: the
/// registry wins.
///
/// ADR-0018: PERS-PROV-001.
async fn provisioning_applies_changed_registry_content(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (mut upstreams, mut routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;

    upstreams[0].request.tags = vec!["new".into()];
    routes[0].request.priority = 7;
    assert_eq!(
        f.ok(&upstreams, &routes, root).await,
        ProvisioningCounts {
            upstreams: Tally { updated: 1, ..NONE },
            routes: Tally { updated: 1, ..NONE },
            passes: 2,
            stopped: false,
        }
    );
    assert_eq!(
        f.upstream(root, id(&upstreams[0])).await.unwrap().tags,
        vec!["new".to_owned()]
    );
    assert_eq!(f.route(root, rid(&routes[0])).await.unwrap().priority, 7);
}

/// A registry upstream whose endpoints now derive another alias takes it in
/// place, by the create rules: its ID and its routes, API routes included,
/// stay. The Management API keeps rejecting an alias change.
async fn provisioning_changes_the_alias_of_a_registry_upstream(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (mut upstreams, routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;
    let upstream_id = id(&upstreams[0]);
    let api_route = f.api_route(root, upstream_id, "/user", None).await;

    set_host(&mut upstreams[0], "api.other.com");
    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(counts.upstreams, Tally { updated: 1, ..NONE });
    assert_eq!(f.alias(root, upstream_id).await, "api.other.com");
    assert_eq!(
        f.route(root, api_route.id).await.unwrap().upstream_id,
        upstream_id
    );
    assert_eq!(f.ok(&upstreams, &routes, root).await, UNCHANGED_ALL);

    // hostname → IP, which needs the explicit alias create would need.
    set_host(&mut upstreams[0], "10.0.0.1");
    upstreams[0].request.alias = Some("internal-api".into());
    f.ok(&upstreams, &routes, root).await;
    assert_eq!(f.alias(root, upstream_id).await, "internal-api");

    // An API upstream still keeps its alias.
    let api_upstream = f.api_upstream(root, "api-owned.example.com", None).await;
    let mut renamed = upstream_update(&upstream_at(root, "api-owned.example.com").request);
    renamed.alias = Some("api-renamed".into());
    let err =
        f.cp.update_upstream(&provisioning_ctx(root).unwrap(), api_upstream.id, renamed)
            .await
            .expect_err("the Management API cannot change an alias");
    assert!(err.to_string().contains("alias cannot be changed"), "{err}");
}

/// U1 gives `a` up for `b`, U2 gives `b` up for `c`, a new U3 takes `a`: all
/// applied in one run, whichever way the registry lists them.
///
/// ADR-0018: PERS-PROV-004.
async fn an_alias_chain_resolves_in_one_run_in_any_listing_order(b: Backend) {
    let f = Fixture::new(b);
    for reversed in [false, true] {
        let root = Uuid::new_v4();
        let mut upstreams = vec![
            upstream_at(root, "a.example.com"),
            upstream_at(root, "b.example.com"),
        ];
        f.ok(&upstreams, &[], root).await;

        set_host(&mut upstreams[0], "b.example.com");
        set_host(&mut upstreams[1], "c.example.com");
        upstreams.push(upstream_at(root, "a.example.com"));
        let mut listed = upstreams.clone();
        if reversed {
            listed.reverse();
        }
        let counts = f.ok(&listed, &[], root).await;
        assert_eq!(
            counts.upstreams,
            Tally {
                created: 1,
                updated: 2,
                ..NONE
            },
            "reversed={reversed}"
        );
        for (u, alias) in upstreams.iter().zip(["b", "c", "a"]) {
            assert_eq!(
                f.alias(root, id(u)).await,
                format!("{alias}.example.com"),
                "reversed={reversed}"
            );
        }
    }
    assert!(f.failures().is_empty());
}

/// Each link of a chain listed in the wrong order frees the next alias one
/// pass later: three links, three passes.
///
/// ADR-0018: PERS-PROV-005.
async fn a_chain_listed_in_reverse_takes_a_pass_per_link(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let mut upstreams = vec![
        upstream_at(root, "a.example.com"),
        upstream_at(root, "b.example.com"),
        upstream_at(root, "c.example.com"),
    ];
    f.ok(&upstreams, &[], root).await;

    for (u, host) in upstreams.iter_mut().zip(["b", "c", "d"]) {
        set_host(u, &format!("{host}.example.com"));
    }
    let counts = f.ok(&upstreams, &[], root).await;
    assert_eq!(counts.upstreams, Tally { updated: 3, ..NONE });
    assert_eq!(counts.passes, 3);
}

/// Two upstreams swap aliases: no order works, both are skipped and keep
/// serving their old aliases. A pass that changes nothing ends the loop.
///
/// ADR-0018: PERS-PROV-006.
async fn an_alias_swap_is_skipped_as_a_cycle(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let mut upstreams = vec![
        upstream_at(root, "a.example.com"),
        upstream_at(root, "b.example.com"),
    ];
    f.ok(&upstreams, &[], root).await;

    set_host(&mut upstreams[0], "b.example.com");
    set_host(&mut upstreams[1], "a.example.com");
    let counts = f.ok(&upstreams, &[], root).await;
    assert_eq!(counts.upstreams, Tally { failed: 2, ..NONE });
    assert_eq!(counts.passes, 1);
    assert!(!counts.is_clean());
    assert_eq!(
        f.failures(),
        vec![
            failure("upstream", why::ALIAS_CYCLE),
            failure("upstream", why::ALIAS_CYCLE)
        ]
    );
    assert_eq!(f.alias(root, id(&upstreams[0])).await, "a.example.com");
    assert_eq!(f.alias(root, id(&upstreams[1])).await, "b.example.com");
}

/// A Management API upstream holds the alias: the instance is skipped (its
/// route too, on its own write), nothing fails, and once the API row is
/// gone the next run applies it. A stored instance that wants such an alias
/// keeps serving its old one.
///
/// ADR-0018: PERS-PROV-003.
async fn an_alias_held_by_an_api_upstream_skips_the_instance(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let api = f.api_upstream(root, "api.example.com", None).await;
    let (mut upstreams, routes) = registry_instances(root);

    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(counts.upstreams, Tally { failed: 1, ..NONE });
    assert_eq!(counts.routes, Tally { failed: 1, ..NONE });
    assert_eq!(counts.passes, 2, "a pass without progress ends each loop");
    assert_eq!(
        f.failures(),
        vec![
            failure("route", why::VALIDATION),
            failure("upstream", why::CONFLICT)
        ]
    );
    assert!(f.upstream(root, id(&upstreams[0])).await.is_none());

    f.cp.delete_upstream(&ctx, api.id).await.unwrap();
    assert_eq!(f.ok(&upstreams, &routes, root).await, CREATED_ALL);

    // A stored instance that now wants an API upstream's alias.
    f.api_upstream(root, "taken.example.com", None).await;
    set_host(&mut upstreams[0], "taken.example.com");
    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(counts.upstreams, Tally { failed: 1, ..NONE });
    assert_eq!(
        counts.routes,
        Tally {
            unchanged: 1,
            ..NONE
        }
    );
    assert_eq!(f.alias(root, id(&upstreams[0])).await, "api.example.com");
}

/// A Management API upstream with an instance's ID is never overwritten:
/// the instance is skipped, and so is its route.
///
/// ADR-0018: PERS-PROV-003.
async fn an_api_row_holding_an_upstream_id_skips_the_instance(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    f.api_upstream(root, "elsewhere.example.com", Some(id(&upstreams[0])))
        .await;

    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(counts.upstreams, Tally { failed: 1, ..NONE });
    assert_eq!(counts.routes, Tally { failed: 1, ..NONE });
    assert_eq!(
        f.failures(),
        vec![
            failure("route", why::VALIDATION),
            failure("upstream", why::ID_HELD_BY_API)
        ]
    );
    let row = f.upstream(root, id(&upstreams[0])).await.unwrap();
    assert_eq!(
        (row.managed_by, row.alias.as_str()),
        (ManagedBy::Api, "elsewhere.example.com")
    );
}

/// The same for a route ID a Management API route holds.
async fn an_api_row_holding_a_route_id_skips_the_instance(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    f.ok(&upstreams, &[], root).await;
    f.api_route(root, id(&upstreams[0]), "/api", Some(rid(&routes[0])))
        .await;

    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(counts.routes, Tally { failed: 1, ..NONE });
    assert_eq!(f.failures(), vec![failure("route", why::ID_HELD_BY_API)]);
    assert_eq!(f.path(root, rid(&routes[0])).await, "/api");
}

/// A change the hierarchy refuses (an auth override of an ancestor that
/// enforces its auth) is skipped; the stored row keeps its old config.
///
/// ADR-0018: PERS-PROV-003.
async fn a_hierarchy_rejection_skips_and_the_old_config_serves(b: Backend) {
    let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
    let f = Fixture::with(
        b,
        Arc::new(MockTenantResolverClient::with_hierarchy(vec![
            TenantId(root),
            TenantId(child),
        ])),
        Arc::new(MockCredStoreClient::empty()),
    );
    let mut ancestor = upstream_at(root, "api.example.com").request;
    ancestor.auth = Some(AuthConfig {
        plugin_type: "apikey".into(),
        sharing: SharingMode::Enforce,
        config: None,
    });
    f.cp.create_upstream(&provisioning_ctx(root).unwrap(), ancestor)
        .await
        .unwrap();
    let mut upstreams = vec![upstream_at(child, "api.example.com")];
    assert_eq!(
        f.ok(&upstreams, &[], root).await.upstreams,
        Tally { created: 1, ..NONE }
    );

    upstreams[0].request.auth = Some(AuthConfig {
        plugin_type: "apikey".into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let counts = f.ok(&upstreams, &[], root).await;
    assert_eq!(counts.upstreams, Tally { failed: 1, ..NONE });
    assert_eq!(f.failures(), vec![failure("upstream", why::VALIDATION)]);
    assert!(
        f.upstream(child, id(&upstreams[0]))
            .await
            .unwrap()
            .auth
            .is_none()
    );
}

/// Every content check skips with its reason, before any write; a skipped
/// instance's stored row is neither changed nor removed.
///
/// ADR-0018: PERS-PROV-002.
async fn the_content_check_skips_bad_instances_and_keeps_their_rows(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let good = upstream_at(root, "good.example.com");
    let mut invalid = upstream_at(root, "invalid.example.com");
    let routes = vec![route_on(&good, "/ok"), route_on(&invalid, "/on-invalid")];
    f.ok(&[good.clone(), invalid.clone()], &routes, root).await;
    assert!(f.failures().is_empty());

    // An IP endpoint without the explicit alias it needs.
    set_host(&mut invalid, "10.0.0.1");
    let mut no_id = upstream_at(root, "noid.example.com");
    no_id.request.id = None;
    let mut bad_route = routes[0].clone();
    bad_route.request.match_rules.http = None;
    let mut unknown = route_on(&good, "/unknown");
    unknown.request.upstream_id = Uuid::new_v4();
    let counts = f
        .ok(
            &[
                good.clone(),
                invalid.clone(),
                upstream_at(root, "twin.example.com"),
                upstream_at(root, "twin.example.com"),
                no_id,
            ],
            &[
                bad_route,
                routes[1].clone(),
                unknown,
                route_on(&good, "/twice"),
                route_on(&good, "/twice"),
            ],
            root,
        )
        .await;
    assert_eq!(
        counts.upstreams,
        Tally {
            unchanged: 1,
            failed: 4,
            ..NONE
        }
    );
    assert_eq!(
        counts.routes,
        Tally {
            unchanged: 1,
            failed: 4,
            ..NONE
        }
    );
    assert_eq!(
        f.failures(),
        vec![
            failure("route", why::INVALID_REQUEST),
            failure("route", why::OVERLAPPING_REGISTRY_ROUTES),
            failure("route", why::OVERLAPPING_REGISTRY_ROUTES),
            failure("route", why::UNKNOWN_UPSTREAM),
            failure("upstream", why::DUPLICATE_ALIAS),
            failure("upstream", why::DUPLICATE_ALIAS),
            failure("upstream", why::INVALID_INSTANCE),
            failure("upstream", why::INVALID_REQUEST),
        ]
    );
    // The skipped instances' rows are untouched and still serve.
    assert_eq!(f.alias(root, id(&invalid)).await, "invalid.example.com");
    assert_eq!(f.path(root, rid(&routes[0])).await, "/ok");
    assert_eq!(f.path(root, rid(&routes[1])).await, "/on-invalid");
}

/// Each kind of bad content the content check catches, under an enabled
/// SSRF policy: an upstream on a denied host, with an invalid CORS, budget or
/// `secret_ref`, a route with an invalid CORS, and a route without an ID.
/// Each is skipped with its reason, and the rows the instances had stored
/// stay as they were.
///
/// ADR-0018: PERS-PROV-002.
async fn every_kind_of_bad_content_is_skipped_and_keeps_its_row(b: Backend) {
    use crate::domain::model::{
        BudgetConfig, BudgetMode, CorsConfig, RateLimitAlgorithm, RateLimitConfig, RateLimitScope,
        RateLimitStrategy, SustainedRate, Window,
    };
    use crate::domain::ssrf::SsrfPolicy;

    let ssrf = SsrfGuard::from_config(&SsrfPolicy {
        extra_deny_hostnames: vec!["denied.example.com".into()],
        ..SsrfPolicy::default()
    })
    .unwrap();
    let f = Fixture::guarded(b, ssrf);
    let root = Uuid::new_v4();
    let good = upstream_at(root, "good.example.com");
    let hosts = ["ssrf", "cors", "budget", "secret"].map(|h| format!("{h}.example.com"));
    let mut bad = hosts.map(|host| upstream_at(root, &host));
    let mut bad_cors_route = route_on(&good, "/cors");
    let mut upstreams = vec![good.clone()];
    upstreams.extend(bad.iter().cloned());
    let routes = vec![route_on(&good, "/ok"), bad_cors_route.clone()];
    f.ok(&upstreams, &routes, root).await;
    assert!(f.failures().is_empty());
    let mut stored = Vec::new();
    for u in &bad {
        stored.push(f.upstream(root, id(u)).await.expect("stored upstream"));
    }
    let stored_route = f.route(root, rid(&bad_cors_route)).await.expect("route");

    let wildcard_with_credentials = CorsConfig {
        sharing: SharingMode::Private,
        enabled: true,
        allowed_origins: vec!["*".into()],
        allowed_methods: vec![],
        expose_headers: vec![],
        allow_credentials: true,
    };
    let [ssrf, cors, budget, secret] = &mut bad;
    set_host(ssrf, "denied.example.com");
    cors.request.cors = Some(wildcard_with_credentials.clone());
    budget.request.rate_limit = Some(RateLimitConfig {
        sharing: SharingMode::Private,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 10,
            window: Window::Second,
        },
        burst: None,
        budget: Some(BudgetConfig {
            mode: BudgetMode::Allocated,
            total: None,
            overcommit_ratio: None,
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: false,
        pool_owner_id: None,
    });
    secret.request.auth = Some(AuthConfig {
        plugin_type: "apikey".into(),
        sharing: SharingMode::Private,
        config: Some(
            [
                ("header".into(), "authorization".into()),
                ("secret_ref".into(), "cred://not a ref".into()),
            ]
            .into(),
        ),
    });
    bad_cors_route.request.cors = Some(wildcard_with_credentials);
    let mut no_id = route_on(&good, "/no-id");
    no_id.request.id = None;

    // Each fails the content check for its own fault.
    for (u, fault) in bad.iter().zip([
        "blocked by SSRF protection",
        "allow_credentials",
        "budget.total is required",
        "invalid secret_ref",
    ]) {
        let e = f
            .provisioner
            .check_registry_upstream(&u.request)
            .unwrap_err();
        assert!(e.to_string().contains(fault), "{fault}: {e}");
    }
    let e = validate_route_request(&bad_cors_route.request).unwrap_err();
    assert!(e.to_string().contains("allow_credentials"), "{e}");

    let mut upstreams = vec![good.clone()];
    upstreams.extend(bad.iter().cloned());
    let routes = [routes[0].clone(), bad_cors_route.clone(), no_id];
    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(
        (counts.upstreams, counts.routes),
        (
            Tally {
                unchanged: 1,
                failed: 4,
                ..NONE
            },
            Tally {
                unchanged: 1,
                failed: 2,
                ..NONE
            }
        )
    );
    assert_eq!(
        f.failures(),
        vec![
            failure("route", why::INVALID_INSTANCE),
            failure("route", why::INVALID_REQUEST),
            failure("upstream", why::INVALID_REQUEST),
            failure("upstream", why::INVALID_REQUEST),
            failure("upstream", why::INVALID_REQUEST),
            failure("upstream", why::INVALID_REQUEST),
        ]
    );
    for (u, before) in bad.iter().zip(&stored) {
        assert_eq!(f.upstream(root, id(u)).await.as_ref(), Some(before));
    }
    assert_eq!(
        f.route(root, rid(&bad_cors_route)).await,
        Some(stored_route)
    );
}

/// R1 leaves `/v1` for `/v3` and R2 takes `/v1`, in either listing order.
///
/// ADR-0018: PERS-PROV-004.
async fn a_path_hand_over_resolves_in_one_run_in_any_listing_order(b: Backend) {
    let f = Fixture::new(b);
    for r2_first in [false, true] {
        let root = Uuid::new_v4();
        let (upstreams, mut routes) = registry_instances(root);
        routes.push(sibling_route(&routes[0], "/v2"));
        f.ok(&upstreams, &routes, root).await;

        set_path(&mut routes[0], "/v3");
        set_path(&mut routes[1], "/v1");
        let mut listed = routes.clone();
        if r2_first {
            listed.reverse();
        }
        let counts = f.ok(&upstreams, &listed, root).await;
        assert_eq!(
            counts.routes,
            Tally { updated: 2, ..NONE },
            "r2_first={r2_first}"
        );
        assert_eq!(f.path(root, rid(&routes[0])).await, "/v3");
        assert_eq!(f.path(root, rid(&routes[1])).await, "/v1");
    }
    assert!(f.failures().is_empty());
}

/// Two registry routes swap paths: both skipped, both keep their old paths.
///
/// ADR-0018: PERS-PROV-006.
async fn a_path_swap_is_skipped_as_a_cycle(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, mut routes) = registry_instances(root);
    routes.push(sibling_route(&routes[0], "/v2"));
    f.ok(&upstreams, &routes, root).await;

    set_path(&mut routes[0], "/v2");
    set_path(&mut routes[1], "/v1");
    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(counts.routes, Tally { failed: 2, ..NONE });
    assert_eq!(
        f.failures(),
        vec![
            failure("route", why::PATH_CYCLE),
            failure("route", why::PATH_CYCLE)
        ]
    );
    assert_eq!(f.path(root, rid(&routes[0])).await, "/v1");
    assert_eq!(f.path(root, rid(&routes[1])).await, "/v2");
}

/// A route the registry moves to another upstream moves in place: same ID,
/// registry-managed, counted as updated.
///
/// ADR-0018: PERS-PROV-007.
async fn a_route_moves_to_another_upstream_in_place(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (mut upstreams, mut routes) = registry_instances(root);
    let other = upstream_at(root, "other.example.com");
    upstreams.push(other.clone());
    f.ok(&upstreams, &routes, root).await;

    routes[0].request.upstream_id = id(&other);
    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(counts.routes, Tally { updated: 1, ..NONE });
    let route = f.route(root, rid(&routes[0])).await.unwrap();
    assert_eq!(
        (route.upstream_id, route.managed_by),
        (id(&other), ManagedBy::Registry)
    );
    assert_eq!(f.ok(&upstreams, &routes, root).await.routes.unchanged, 1);
}

/// The target upstream is skipped and has no row: the move is rejected and
/// the route keeps serving on its old upstream.
async fn a_move_to_a_skipped_upstream_keeps_the_route_serving(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (mut upstreams, mut routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;
    let old = id(&upstreams[0]);

    // Invalid: an IP endpoint needs an explicit alias.
    let target = upstream_at(root, "10.0.0.2");
    upstreams.push(target.clone());
    routes[0].request.upstream_id = id(&target);
    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(counts.routes, Tally { failed: 1, ..NONE });
    assert_eq!(
        f.failures(),
        vec![
            failure("route", why::VALIDATION),
            failure("upstream", why::INVALID_REQUEST)
        ]
    );
    assert_eq!(
        f.route(root, rid(&routes[0])).await.unwrap().upstream_id,
        old
    );
}

/// A skipped route's stored row keeps its path, so a new registry route at
/// that path is rejected too: registry routes never overlap at match time.
async fn the_stale_row_of_a_skipped_route_blocks_its_path(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, mut routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;

    // R1 now has invalid content (both match types) and is skipped; a new
    // R2 wants R1's path.
    let newcomer = sibling_route(&routes[0], "/v1");
    routes[0].request.match_rules.grpc = Some(GrpcMatch {
        service: "svc".into(),
        method: "M".into(),
    });
    let counts = f
        .ok(&upstreams, &[routes[0].clone(), newcomer.clone()], root)
        .await;
    assert_eq!(counts.routes, Tally { failed: 2, ..NONE });
    assert_eq!(
        f.failures(),
        vec![
            failure("route", why::CONFLICT),
            failure("route", why::INVALID_REQUEST)
        ]
    );
    assert!(f.route(root, rid(&newcomer)).await.is_none());
    assert_eq!(f.path(root, rid(&routes[0])).await, "/v1");
}

/// A registry route naming an upstream that is a Management API row in the
/// store (its instance is skipped for that) is skipped too.
///
/// ADR-0018: PERS-PROV-003.
async fn a_route_on_an_api_upstream_is_skipped(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (mut upstreams, mut routes) = registry_instances(root);
    let api = f.api_upstream(root, "api-owned.example.com", None).await;
    let mut twin = upstream_at(root, "api-owned.example.com");
    twin.request.id = Some(api.id);
    upstreams.push(twin);
    routes[0].request.upstream_id = api.id;

    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(counts.routes, Tally { failed: 1, ..NONE });
    assert_eq!(
        f.failures(),
        vec![
            failure("route", why::VALIDATION),
            failure("upstream", why::ID_HELD_BY_API)
        ]
    );
    assert!(f.route(root, rid(&routes[0])).await.is_none());
}

/// Registry routes still conflict with API routes, in both directions: the
/// registry route is skipped; the API create is refused.
async fn registry_and_api_routes_still_conflict(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (upstreams, routes) = registry_instances(root);
    f.ok(&upstreams, &[], root).await;
    let api_route = f.api_route(root, id(&upstreams[0]), "/v1", None).await;

    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(counts.routes, Tally { failed: 1, ..NONE });
    assert_eq!(f.failures(), vec![failure("route", why::CONFLICT)]);

    f.cp.delete_route(&ctx, api_route.id).await.unwrap();
    assert_eq!(f.ok(&upstreams, &routes, root).await.routes.created, 1);
    let mut api = routes[0].request.clone();
    api.id = None;
    assert!(matches!(
        f.cp.create_route(&ctx, api).await,
        Err(DomainError::Conflict { .. })
    ));
}

/// Instances gone from the registry are removed, routes and upstreams.
///
/// ADR-0018: PERS-PROV-008.
async fn provisioning_removes_instances_the_registry_no_longer_has(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;

    let counts = f.ok(&[], &[], root).await;
    assert_eq!(
        counts,
        ProvisioningCounts {
            upstreams: Tally { removed: 1, ..NONE },
            routes: Tally { removed: 1, ..NONE },
            passes: 0,
            stopped: false,
        }
    );
    assert!(counts.is_clean());
    assert!(f.upstream(root, id(&upstreams[0])).await.is_none());
    assert!(f.route(root, rid(&routes[0])).await.is_none());
}

/// A dropped upstream that a Management API route uses is kept, never
/// cascaded: the API route survives, the run is not clean, and the first
/// run after the API route is deleted removes the upstream.
///
/// ADR-0018: PERS-GATE-008, PERS-OWN-002, PERS-PROV-009.
async fn a_dropped_upstream_that_api_routes_use_is_kept_until_they_go(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;
    let api_route = f.api_route(root, id(&upstreams[0]), "/user", None).await;

    let counts = f.ok(&[], &[], root).await;
    assert_eq!(counts.upstreams, Tally { kept: 1, ..NONE });
    assert_eq!(counts.routes, Tally { removed: 1, ..NONE });
    assert!(!counts.is_clean());
    assert_eq!(f.failures(), vec![failure("upstream", why::KEPT_IN_USE)]);
    assert_eq!(
        f.upstream(root, id(&upstreams[0]))
            .await
            .unwrap()
            .managed_by,
        ManagedBy::Registry
    );
    assert_eq!(
        f.route(root, api_route.id).await.unwrap().managed_by,
        ManagedBy::Api
    );

    f.cp.delete_route(&provisioning_ctx(root).unwrap(), api_route.id)
        .await
        .unwrap();
    let counts = f.ok(&[], &[], root).await;
    assert_eq!(counts.upstreams, Tally { removed: 1, ..NONE });
    assert!(counts.is_clean());
}

/// The upstream gets a new ID and keeps its alias, and the route follows
/// it: the old upstream loses its registry route and goes, the new one takes
/// the alias, and the route is re-created on it under the same ID.
///
/// ADR-0018: PERS-PROV-012.
async fn a_route_follows_its_upstream_to_a_replacement_with_the_same_alias(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (mut upstreams, mut routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;
    let old_id = id(&upstreams[0]);
    let new_id = Uuid::new_v4();
    upstreams[0].request.id = Some(new_id);
    routes[0].request.upstream_id = new_id;

    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(
        counts,
        ProvisioningCounts {
            upstreams: Tally {
                created: 1,
                removed: 1,
                ..NONE
            },
            routes: Tally { updated: 1, ..NONE },
            passes: 2,
            stopped: false,
        }
    );
    assert_eq!(
        f.route(root, rid(&routes[0])).await.unwrap().upstream_id,
        new_id
    );
    assert!(f.upstream(root, old_id).await.is_none());
    assert_eq!(f.ok(&upstreams, &routes, root).await, UNCHANGED_ALL);
}

/// The replacement of a dropped upstream wants its alias, but a Management
/// API route keeps the old upstream (and its alias): the replacement is
/// skipped, and the registry route moving to it stays on the old upstream
/// instead of being deleted. Once the API route goes, the move completes.
///
/// ADR-0018: PERS-PROV-011.
async fn a_route_leaving_a_kept_upstream_serves_there_until_its_move_succeeds(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (mut upstreams, mut routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;
    let old_id = id(&upstreams[0]);
    let api_route = f.api_route(root, old_id, "/user", None).await;
    let new_id = Uuid::new_v4();
    upstreams[0].request.id = Some(new_id);
    routes[0].request.upstream_id = new_id;

    let counts = f.ok(&upstreams, &routes, root).await;
    assert!(!counts.is_clean());
    assert_eq!(counts.upstreams.kept, 1);
    assert_eq!(counts.routes.removed, 0);
    assert!(f.upstream(root, new_id).await.is_none());
    assert_eq!(
        f.route(root, rid(&routes[0])).await.unwrap().upstream_id,
        old_id,
        "the registry route still serves on the kept upstream"
    );

    f.cp.delete_route(&provisioning_ctx(root).unwrap(), api_route.id)
        .await
        .unwrap();
    let counts = f.ok(&upstreams, &routes, root).await;
    assert!(counts.is_clean(), "{counts:?}");
    assert!(f.upstream(root, old_id).await.is_none());
    assert_eq!(
        f.route(root, rid(&routes[0])).await.unwrap().upstream_id,
        new_id
    );
}

/// An instance moving to another tenant while a Management API route keeps
/// its old upstream: its route stays in the old tenant instead of being
/// deleted, and moves once the API route goes.
async fn a_route_moving_tenant_off_a_kept_upstream_serves_there_until_it_can_move(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let elsewhere = Uuid::new_v4();
    let (mut upstreams, mut routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;
    let api_route = f.api_route(root, id(&upstreams[0]), "/user", None).await;

    upstreams[0].tenant_id = Some(elsewhere);
    routes[0].tenant_id = Some(elsewhere);
    let counts = f.ok(&upstreams, &routes, root).await;
    assert!(!counts.is_clean());
    assert_eq!(counts.routes.removed, 0);
    assert!(
        f.route(root, rid(&routes[0])).await.is_some(),
        "still serving"
    );
    assert!(f.route(elsewhere, rid(&routes[0])).await.is_none());

    f.cp.delete_route(&provisioning_ctx(root).unwrap(), api_route.id)
        .await
        .unwrap();
    let counts = f.ok(&upstreams, &routes, root).await;
    assert!(counts.is_clean(), "{counts:?}");
    assert!(f.route(root, rid(&routes[0])).await.is_none());
    assert!(f.route(elsewhere, rid(&routes[0])).await.is_some());
    assert!(f.upstream(elsewhere, id(&upstreams[0])).await.is_some());
}

/// An instance that moves to another tenant is re-created there.
///
/// ADR-0018: PERS-PROV-013.
async fn provisioning_moves_an_instance_to_its_new_tenant(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let elsewhere = Uuid::new_v4();
    let (mut upstreams, mut routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;

    upstreams[0].tenant_id = Some(elsewhere);
    routes[0].tenant_id = Some(elsewhere);
    let counts = f.ok(&upstreams, &routes, root).await;
    let moved = Tally {
        created: 1,
        removed: 1,
        ..NONE
    };
    assert_eq!((counts.upstreams, counts.routes), (moved, moved));
    assert!(f.upstream(elsewhere, id(&upstreams[0])).await.is_some());
    assert!(f.upstream(root, id(&upstreams[0])).await.is_none());
    let route = f
        .route(elsewhere, rid(&routes[0]))
        .await
        .expect("moved route");
    assert_eq!(
        (route.tenant_id, route.upstream_id, route.managed_by),
        (elsewhere, id(&upstreams[0]), ManagedBy::Registry)
    );
    assert_eq!(f.path(elsewhere, rid(&routes[0])).await, "/v1");
    assert!(f.route(root, rid(&routes[0])).await.is_none());
}

/// A run that stopped after the upstreams creates only the routes.
async fn provisioning_after_a_partial_run_creates_only_what_is_missing(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    f.ok(&upstreams, &[], root).await;
    let counts = f.ok(&upstreams, &routes, root).await;
    assert_eq!(
        (counts.upstreams, counts.routes),
        (
            Tally {
                unchanged: 1,
                ..NONE
            },
            Tally { created: 1, ..NONE }
        )
    );
}

/// An upstream instance whose auth names a secret.
fn with_secret(root: Uuid) -> ProvisionedUpstream {
    let mut u = upstream_at(root, "api.example.com");
    let mut config = std::collections::HashMap::new();
    config.insert("header".into(), "authorization".into());
    config.insert("secret_ref".into(), "cred://key".into());
    u.request.auth = Some(AuthConfig {
        plugin_type: "apikey".into(),
        sharing: SharingMode::Private,
        config: Some(config),
    });
    u
}

/// A credential-store outage is fatal: the run aborts. The store refusing
/// one secret skips that upstream only.
///
/// ADR-0018: PERS-PROV-015.
async fn a_credstore_outage_aborts_and_a_refusal_skips(b: Backend) {
    let stores = Stores::new(b);
    let tenants: Arc<dyn TenantResolverClient> =
        Arc::new(MockTenantResolverClient::single_tenant());
    let outage = Fixture::over(
        stores.clone(),
        tenants.clone(),
        Arc::new(ErroringCredStore(|| {
            credstore_sdk::CredStoreError::NoPluginAvailable
        })),
    );
    let root = Uuid::new_v4();
    let err = outage
        .run(&[with_secret(root)], &[], root)
        .await
        .expect_err("a credential-store outage aborts the run")
        .to_string();
    assert!(err.contains("credential validation unavailable"), "{err}");

    let refusal = Fixture::over(
        stores,
        tenants,
        Arc::new(ErroringCredStore(|| {
            credstore_sdk::CredStoreError::AccessDenied
        })),
    );
    let counts = refusal
        .ok(
            &[with_secret(root), upstream_at(root, "plain.example.com")],
            &[],
            root,
        )
        .await;
    assert_eq!(
        counts.upstreams,
        Tally {
            created: 1,
            failed: 1,
            ..NONE
        }
    );
    assert_eq!(
        refusal.failures(),
        vec![failure("upstream", why::VALIDATION)]
    );
}

/// The tenant resolver refusing OAGW itself would fail every write the same
/// way: fatal, not a skip per instance.
///
/// ADR-0018: PERS-PROV-015.
async fn a_tenant_resolver_refusing_oagw_aborts(b: Backend) {
    let f = Fixture::with(
        b,
        Arc::new(UnauthorizedTenantResolver),
        Arc::new(MockCredStoreClient::empty()),
    );
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let err = f
        .run(&upstreams, &routes, root)
        .await
        .expect_err("an unauthorized tenant resolver aborts the run")
        .to_string();
    assert!(
        err.contains(&format!(
            "Failed to provision upstream {}",
            id(&upstreams[0])
        )),
        "{err}"
    );
    assert!(f.failures().is_empty());
}

// ---------------------------------------------------------------------------
// In memory only: logs, scripted stores, run control
// ---------------------------------------------------------------------------

/// The log names what changed, and the kept upstream's API routes.
#[tokio::test]
#[traced_test]
async fn the_log_names_the_drift_and_the_routes_that_keep_an_upstream() {
    let f = Fixture::new(Backend::Memory);
    let root = Uuid::new_v4();
    let (mut upstreams, mut routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;
    upstreams[0].request.tags = vec!["new".into()];
    routes[0].request.priority = 7;
    f.ok(&upstreams, &routes, root).await;
    logs_assert(|lines| {
        for (message, drift) in [
            ("Updated upstream from types-registry", r#"drift=["tags"]"#),
            ("Updated route from types-registry", r#"drift=["priority"]"#),
        ] {
            if !lines
                .iter()
                .any(|l| l.contains(message) && l.contains(drift))
            {
                return Err(format!("no line has both {message:?} and {drift}"));
            }
        }
        Ok(())
    });

    let api_route = f.api_route(root, id(&upstreams[0]), "/user", None).await;
    f.ok(&[], &[], root).await;
    let route_id = api_route.id.to_string();
    logs_assert(|lines| {
        lines
            .iter()
            .any(|l| {
                l.contains("Kept upstream no longer in types-registry") && l.contains(&route_id)
            })
            .then_some(())
            .ok_or_else(|| "no kept line names the API route".to_owned())
    });
}

/// The kept line names every registry route still on the upstream, even
/// past the first page of its routes, and the route deleted with a dropped
/// upstream whose write is then rejected is logged as not re-created.
#[tokio::test]
#[traced_test]
async fn the_log_names_what_keeps_an_upstream_and_a_route_not_re_created() {
    let f = Fixture::new(Backend::Memory);
    let root = Uuid::new_v4();
    let (upstreams, mut routes) = registry_instances(root);
    // The registry route sorts after the API routes.
    routes[0].request.id = Some(Uuid::max());
    f.ok(&upstreams, &routes, root).await;
    let old = id(&upstreams[0]);
    for i in 0..12 {
        f.api_route(root, old, &format!("/api{i}"), Some(Uuid::from_u128(i + 1)))
            .await;
    }
    let mut broken = routes[0].clone();
    broken.request.match_rules.http = None;
    f.ok(&[], &[broken], root).await;
    logs_assert(|lines| {
        lines
            .iter()
            .any(|l| {
                l.contains("Kept upstream no longer in types-registry")
                    && l.contains(&format!("registry_routes={}", Uuid::max()))
            })
            .then_some(())
            .ok_or_else(|| "no kept line names the registry route".to_owned())
    });

    // A route moving to an upstream whose ID an API row holds: the route is
    // deleted with its dropped upstream, then its write is rejected.
    let f = Fixture::new(Backend::Memory);
    let (upstreams, routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;
    let replacement = upstream_at(root, "new.example.com");
    f.api_upstream(root, "held.example.com", Some(id(&replacement)))
        .await;
    let mut moved = routes[0].clone();
    moved.request.upstream_id = id(&replacement);
    let counts = f
        .ok(std::slice::from_ref(&replacement), &[moved], root)
        .await;
    assert_eq!(counts.routes, Tally { failed: 1, ..NONE }, "{counts:?}");
    assert!(f.route(root, rid(&routes[0])).await.is_none());
    let route_id = rid(&routes[0]).to_string();
    logs_assert(|lines| {
        lines
            .iter()
            .any(|l| l.contains("was not re-created") && l.contains(&route_id))
            .then_some(())
            .ok_or_else(|| "no line says the route was not re-created".to_owned())
    });
}

/// A skipped instance is logged at error with its reason and whether its
/// stored row keeps serving.
#[tokio::test]
#[traced_test]
async fn a_skip_is_logged_at_error_with_its_reason() {
    let f = Fixture::new(Backend::Memory);
    let root = Uuid::new_v4();
    let mut upstreams = vec![
        upstream_at(root, "a.example.com"),
        upstream_at(root, "b.example.com"),
    ];
    f.ok(&upstreams, &[], root).await;
    set_host(&mut upstreams[0], "b.example.com");
    set_host(&mut upstreams[1], "a.example.com");
    f.ok(&upstreams, &[], root).await;
    logs_assert(|lines| {
        lines
            .iter()
            .any(|l| {
                l.contains("ERROR")
                    && l.contains("Skipped upstream from types-registry")
                    && l.contains("alias_cycle")
                    && l.contains("stored_row_serves=true")
                    && l.contains("split the change across two releases")
            })
            .then_some(())
            .ok_or_else(|| "no error line for the alias cycle".to_owned())
    });
}

/// Hides every registry row from the bulk load, as if another replica wrote
/// it just after this run loaded.
struct HiddenFromLoad(Arc<dyn RegistryRowRepository>);

#[async_trait]
impl RegistryRowRepository for HiddenFromLoad {
    async fn list_upstreams(&self) -> Result<Vec<Upstream>, RepositoryError> {
        Ok(vec![])
    }
    async fn list_routes(&self) -> Result<Vec<Route>, RepositoryError> {
        Ok(vec![])
    }
    async fn delete_upstream_if_unused(
        &self,
        tenant_id: Uuid,
        id: Uuid,
    ) -> Result<DeleteOutcome, RepositoryError> {
        self.0.delete_upstream_if_unused(tenant_id, id).await
    }
}

/// Replicas reconciling together (a lost lease): this run did not load the
/// rows another wrote meanwhile; its creates conflict, find our own rows,
/// and go on as for stored rows.
#[tokio::test]
async fn provisioning_accepts_rows_another_replica_just_created() {
    let root = Uuid::new_v4();
    let (upstreams, mut routes) = registry_instances(root);
    let stores = Stores::new(Backend::Memory);
    let tenants: Arc<dyn TenantResolverClient> =
        Arc::new(MockTenantResolverClient::single_tenant());
    let racer = Fixture::over(
        stores.clone(),
        tenants.clone(),
        Arc::new(MockCredStoreClient::empty()),
    );
    racer.ok(&upstreams, &routes, root).await;

    let f = Fixture::over(
        Stores {
            rows: Arc::new(HiddenFromLoad(stores.rows.clone())),
            ..stores
        },
        tenants,
        Arc::new(MockCredStoreClient::empty()),
    );
    assert_eq!(f.ok(&upstreams, &routes, root).await, UNCHANGED_ALL);
    routes[0].request.priority = 3;
    assert_eq!(f.ok(&upstreams, &routes, root).await.routes.updated, 1);
}

/// Storage that fails is an infrastructure error: the run aborts before any
/// write.
#[tokio::test]
async fn provisioning_fails_when_storage_fails() {
    let f = Fixture::over(
        Stores {
            upstreams: Arc::new(UnavailableStorage),
            routes: Arc::new(UnavailableStorage),
            rows: Arc::new(UnavailableStorage),
        },
        Arc::new(MockTenantResolverClient::single_tenant()),
        Arc::new(MockCredStoreClient::empty()),
    );
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let err = f
        .run(&upstreams, &routes, root)
        .await
        .expect_err("a storage outage fails the run")
        .to_string();
    assert!(
        err.contains("Failed to list registry-managed upstreams"),
        "{err}"
    );
}

/// Registry rows change only through the reconcile: Management API writes
/// to them are rejected and leave the stored rows as they were.
///
/// ADR-0018: PERS-OWN-001.
async fn api_writes_to_registry_rows_are_rejected(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (upstreams, routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;
    let (uid, route_id) = (id(&upstreams[0]), rid(&routes[0]));
    let stored = (f.upstream(root, uid).await, f.route(root, route_id).await);
    assert!(stored.0.is_some() && stored.1.is_some());
    let unchanged = async || {
        assert_eq!(
            (f.upstream(root, uid).await, f.route(root, route_id).await),
            stored
        );
    };

    let mut upstream = upstreams[0].clone();
    set_host(&mut upstream, "changed.example.com");
    let mut route = routes[0].clone();
    set_path(&mut route, "/changed");
    let registry_managed = |e: DomainError| matches!(e, DomainError::RegistryManaged { .. });
    assert!(registry_managed(
        f.cp.update_upstream(&ctx, uid, upstream_update(&upstream.request))
            .await
            .unwrap_err()
    ));
    unchanged().await;
    assert!(registry_managed(
        f.cp.update_route(&ctx, route_id, route_update(&route.request))
            .await
            .unwrap_err()
    ));
    unchanged().await;
    assert!(registry_managed(
        f.cp.delete_route(&ctx, route_id).await.unwrap_err()
    ));
    unchanged().await;
    assert!(registry_managed(
        f.cp.delete_upstream(&ctx, uid).await.unwrap_err()
    ));
    unchanged().await;
}

/// Every `DomainError` variant is classified: outages abort the run (no
/// reason), every refusal about the instance skips it with its reason.
///
/// ADR-0018: PERS-PROV-017.
#[test]
fn every_domain_error_is_fatal_or_has_a_reason() {
    let d = || "detail".to_owned();
    let i = || "/instance".to_owned();
    let cases: Vec<(DomainError, Option<&str>)> = vec![
        (DomainError::internal("db down"), None),
        (
            DomainError::DownstreamError {
                detail: d(),
                instance: i(),
            },
            None,
        ),
        (
            DomainError::LinkUnavailable {
                detail: d(),
                instance: i(),
            },
            None,
        ),
        (
            DomainError::CircuitBreakerOpen {
                detail: d(),
                instance: i(),
            },
            None,
        ),
        (
            DomainError::ConnectionTimeout {
                detail: d(),
                instance: i(),
            },
            None,
        ),
        (
            DomainError::RequestTimeout {
                detail: d(),
                instance: i(),
            },
            None,
        ),
        (
            DomainError::IdleTimeout {
                detail: d(),
                instance: i(),
            },
            None,
        ),
        (
            DomainError::StreamAborted {
                detail: d(),
                instance: i(),
            },
            None,
        ),
        (
            DomainError::ProtocolError {
                detail: d(),
                instance: i(),
            },
            None,
        ),
        (
            DomainError::Forbidden {
                reason: oagw_sdk::reason::permission::TENANT_RESOLVER_UNAUTHORIZED.to_owned(),
                detail: d(),
            },
            None,
        ),
        (
            DomainError::Forbidden {
                reason: "policy".to_owned(),
                detail: d(),
            },
            Some(why::FORBIDDEN),
        ),
        (
            DomainError::Conflict {
                entity: "upstream",
                resource: "alias".into(),
                detail: d(),
            },
            Some(why::CONFLICT),
        ),
        (
            DomainError::NotFound {
                entity: "tenant",
                id: Uuid::nil(),
            },
            Some(why::NOT_FOUND),
        ),
        (DomainError::validation("bad"), Some(why::VALIDATION)),
        (
            DomainError::RegistryManaged {
                entity: "route",
                id: Uuid::nil(),
            },
            Some(why::REGISTRY_MANAGED),
        ),
        (
            DomainError::SecretRefNotAccessible {
                detail: d(),
                instance: i(),
            },
            Some(why::SECRET_NOT_ACCESSIBLE),
        ),
        (
            DomainError::SecretNotFound {
                detail: d(),
                instance: i(),
            },
            Some(why::SECRET_NOT_ACCESSIBLE),
        ),
        (
            DomainError::PluginNotFound {
                gts_id: "p".into(),
                detail: d(),
            },
            Some(why::PLUGIN),
        ),
        (
            DomainError::PluginInUse {
                gts_id: "p".into(),
                detail: d(),
            },
            Some(why::PLUGIN),
        ),
        (
            DomainError::UpstreamDisabled { alias: "a".into() },
            Some(why::UNEXPECTED),
        ),
        (
            DomainError::MissingTargetHost { instance: i() },
            Some(why::UNEXPECTED),
        ),
        (
            DomainError::InvalidTargetHost { instance: i() },
            Some(why::UNEXPECTED),
        ),
        (
            DomainError::UnknownTargetHost {
                detail: d(),
                instance: i(),
            },
            Some(why::UNEXPECTED),
        ),
        (
            DomainError::AuthenticationFailed {
                reason: "r",
                detail: d(),
                instance: i(),
            },
            Some(why::UNEXPECTED),
        ),
        (
            DomainError::PayloadTooLarge {
                detail: d(),
                instance: i(),
            },
            Some(why::UNEXPECTED),
        ),
        (
            DomainError::RateLimitExceeded {
                detail: d(),
                instance: i(),
                retry_after_secs: None,
                limit: None,
                remaining: None,
                reset_epoch: None,
            },
            Some(why::UNEXPECTED),
        ),
        (
            DomainError::GuardRejected {
                status: 403,
                error_code: "e".into(),
                detail: d(),
                instance: i(),
                resource_id: None,
            },
            Some(why::UNEXPECTED),
        ),
        (
            DomainError::CorsOriginNotAllowed {
                origin: "o".into(),
                instance: i(),
            },
            Some(why::UNEXPECTED),
        ),
        (
            DomainError::CorsMethodNotAllowed {
                method: "m".into(),
                instance: i(),
            },
            Some(why::UNEXPECTED),
        ),
    ];
    let key = key_of(Uuid::new_v4(), Uuid::new_v4());
    for (error, expected) in cases {
        let shown = format!("{error:?}");
        assert_eq!(reason_of(&error), expected, "{shown}");
        match (classify(Kind::Route, key, error), expected) {
            (Failure::Fatal(_), None) => {}
            (Failure::Rejected { reason, .. }, Some(expected)) => assert_eq!(reason, expected),
            _ => panic!("{shown}: classify disagrees with reason_of"),
        }
    }
}

/// An `unexpected` rejection logs the variant: its `Display` is the detail
/// alone.
///
/// ADR-0018: PERS-PROV-017.
#[test]
#[traced_test]
fn an_unexpected_rejection_logs_the_variant() {
    let error = DomainError::RateLimitExceeded {
        detail: "limit reached".into(),
        instance: "/i".into(),
        retry_after_secs: None,
        limit: None,
        remaining: None,
        reset_epoch: None,
    };
    assert_eq!(error.to_string(), "limit reached");
    let key = key_of(Uuid::new_v4(), Uuid::new_v4());
    assert!(matches!(
        classify(Kind::Route, key, error),
        Failure::Rejected {
            reason: why::UNEXPECTED,
            ..
        }
    ));
    assert!(logs_contain("RateLimitExceeded"));
}

/// Allows `writes` writes, then stops the run, as a shutdown or a lease
/// lost at a renewal due by time does.
struct StopAfter(std::sync::atomic::AtomicUsize);

impl StopAfter {
    fn writes(writes: usize) -> Self {
        Self(std::sync::atomic::AtomicUsize::new(writes))
    }
}

#[async_trait]
impl RunControl for StopAfter {
    async fn before_write(&self) -> anyhow::Result<bool> {
        use std::sync::atomic::Ordering;
        Ok(self
            .0
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok())
    }
}

/// How [`StopsAt`] stops a gated run at one renewal of its lease.
#[derive(Clone, Copy)]
enum Stop {
    /// Another pod takes the lease, so this renewal finds it lost.
    LeaseLost,
    /// The pod shuts down: the token is cancelled during this renewal.
    Shutdown,
}

/// A registry-state store that stops the run at its `at`-th lease renewal
/// (from 1). With a zero lease TTL every write renews, so `at` picks the
/// write the run stops before (a lost lease) or after (a shutdown).
struct StopsAt {
    inner: Arc<dyn RegistryStateStore>,
    at: usize,
    stop: Stop,
    renewals: std::sync::atomic::AtomicUsize,
    cancel: CancellationToken,
    /// The pod that takes the lease.
    thief: Uuid,
}

#[async_trait]
impl RegistryStateStore for StopsAt {
    async fn read(&self) -> Result<RegistryState, RepositoryError> {
        self.inner.read().await
    }
    async fn claim(&self, generation: u64) -> Result<bool, RepositoryError> {
        self.inner.claim(generation).await
    }
    async fn try_acquire(
        &self,
        owner: Uuid,
        ttl: Duration,
        generation: Option<u64>,
    ) -> Result<bool, RepositoryError> {
        self.inner.try_acquire(owner, ttl, generation).await
    }
    async fn renew(&self, owner: Uuid, ttl: Duration) -> Result<bool, RepositoryError> {
        use std::sync::atomic::Ordering;
        if self.renewals.fetch_add(1, Ordering::SeqCst) + 1 == self.at {
            match self.stop {
                Stop::LeaseLost => {
                    // As if this pod stalled past its TTL: the lease lapses
                    // and another pod takes it.
                    assert!(self.inner.finish(owner, None).await?);
                    assert!(self.inner.try_acquire(self.thief, LEASE_TTL, None).await?);
                }
                Stop::Shutdown => self.cancel.cancel(),
            }
        }
        self.inner.renew(owner, ttl).await
    }
    async fn finish(&self, owner: Uuid, hash: Option<&str>) -> Result<bool, RepositoryError> {
        self.inner.finish(owner, hash).await
    }
}

/// The registry-state store of `b`: on its database, or a fresh one in
/// memory.
fn state_of(b: &Backend) -> Arc<dyn RegistryStateStore> {
    match b {
        Backend::Memory => Arc::new(InMemoryRegistryStateStore::new()),
        Backend::Db(db) => Arc::new(DbRegistryStateStore::new(
            DBProvider::<RepositoryError>::new(db.clone()),
        )),
    }
}

/// A gated run stopped by `stop` at lease renewal `at`, through the real
/// lease checks: it is not clean, records no hash, holds no lease, and wrote
/// exactly `written`. The next run is clean and creates only what is
/// missing.
///
/// ADR-0018: PERS-PROV-014, PERS-LOOP-006.
async fn stopped_gated_run(b: Backend, stop: Stop, at: usize, written: (usize, usize)) {
    let inner = state_of(&b);
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, routes) = two_and_two(root);
    let cancel = CancellationToken::new();
    let store = StopsAt {
        inner: inner.clone(),
        at,
        stop,
        renewals: std::sync::atomic::AtomicUsize::new(0),
        cancel: cancel.clone(),
        thief: Uuid::new_v4(),
    };
    let thief = store.thief;
    let gate = |state: Arc<dyn RegistryStateStore>, lease_ttl| Gate {
        state,
        owner: Uuid::new_v4(),
        generation: None,
        lease_ttl,
        poll: LEASE_POLL,
        jitter: |d| d,
    };
    let run = |gate: Gate, cancel: CancellationToken| {
        let (f, upstreams, routes) = (&f, &upstreams, &routes);
        async move {
            gated_reconcile(
                &gate,
                &*f.provisioner,
                &f.metrics,
                &cancel,
                upstreams,
                routes,
                root,
            )
            .await
            .unwrap()
        }
    };

    let outcome = run(gate(Arc::new(store), Duration::ZERO), cancel).await;
    let RunOutcome::Reconciled(counts) = outcome else {
        panic!("{outcome:?}");
    };
    assert!(counts.stopped && !counts.is_clean(), "{counts:?}");
    assert_eq!(
        (counts.upstreams.created, counts.routes.created),
        written,
        "{counts:?}"
    );
    let mut stored = (0, 0);
    for u in &upstreams {
        stored.0 += usize::from(f.upstream(root, id(u)).await.is_some());
    }
    for r in &routes {
        stored.1 += usize::from(f.route(root, rid(r)).await.is_some());
    }
    assert_eq!(stored, written, "the stored rows are what the run wrote");
    assert_eq!(inner.read().await.unwrap().content_hash, None);
    if let Stop::LeaseLost = stop {
        // The other pod finishes without writing.
        assert!(inner.finish(thief, None).await.unwrap());
    }
    let next = Uuid::new_v4();
    assert!(
        inner.try_acquire(next, LEASE_TTL, None).await.unwrap(),
        "no lease is held"
    );
    assert!(inner.finish(next, None).await.unwrap());

    let outcome = run(gate(inner.clone(), LEASE_TTL), CancellationToken::new()).await;
    let RunOutcome::Reconciled(next) = outcome else {
        panic!("{outcome:?}");
    };
    assert!(next.is_clean(), "{next:?}");
    assert_eq!(
        (next.upstreams.created, next.routes.created),
        (2 - written.0, 2 - written.1)
    );
    assert_eq!(
        inner.read().await.unwrap().content_hash,
        Some(content_hash(&upstreams, &routes, root))
    );
}

// Renewals of `two_and_two` on an empty store: 1 after the (no) route
// removals, 2 and 3 before the upstream writes, 4 after the upstream pass,
// 5 and 6 before the route writes, 7 after the route pass.

async fn a_lease_lost_inside_the_upstream_pass_leaves_the_rest_to_the_next_run(b: Backend) {
    stopped_gated_run(b, Stop::LeaseLost, 3, (1, 0)).await;
}

async fn a_lease_lost_after_the_upstream_pass_leaves_the_routes_to_the_next_run(b: Backend) {
    stopped_gated_run(b, Stop::LeaseLost, 4, (2, 0)).await;
}

async fn a_shutdown_inside_the_route_pass_leaves_the_rest_to_the_next_run(b: Backend) {
    stopped_gated_run(b, Stop::Shutdown, 5, (2, 1)).await;
}

/// Two upstreams with one route each.
fn two_and_two(root: Uuid) -> (Vec<ProvisionedUpstream>, Vec<ProvisionedRoute>) {
    let a = upstream_at(root, "a.example.com");
    let b = upstream_at(root, "b.example.com");
    let routes = vec![route_on(&a, "/a"), route_on(&b, "/b")];
    (vec![a, b], routes)
}

/// A run stopped after `writes` writes: it is not clean, it wrote exactly
/// `written`, and the next run is clean and creates only what is missing.
///
/// ADR-0018: PERS-PROV-014.
async fn stop_after(b: Backend, writes: usize, written: (usize, usize)) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, routes) = two_and_two(root);
    let counts = reconcile_registry(
        &*f.provisioner,
        &f.metrics,
        &StopAfter::writes(writes),
        &upstreams,
        &routes,
        root,
    )
    .await
    .unwrap();
    assert!(counts.stopped && !counts.is_clean(), "{counts:?}");
    assert_eq!(
        (counts.upstreams.created, counts.routes.created),
        written,
        "{counts:?}"
    );
    let mut stored = (0, 0);
    for u in &upstreams {
        stored.0 += usize::from(f.upstream(root, id(u)).await.is_some());
    }
    for r in &routes {
        stored.1 += usize::from(f.route(root, rid(r)).await.is_some());
    }
    assert_eq!(stored, written, "the stored rows are what the run wrote");

    let next = f.ok(&upstreams, &routes, root).await;
    assert!(next.is_clean(), "{next:?}");
    assert_eq!(
        (next.upstreams.created, next.routes.created),
        (2 - written.0, 2 - written.1)
    );
}

async fn a_stop_inside_the_upstream_pass_leaves_the_rest_to_the_next_run(b: Backend) {
    stop_after(b, 1, (1, 0)).await;
}

async fn a_stop_between_upstreams_and_routes_leaves_the_routes_to_the_next_run(b: Backend) {
    stop_after(b, 2, (2, 0)).await;
}

async fn a_stop_inside_the_route_pass_leaves_the_rest_to_the_next_run(b: Backend) {
    stop_after(b, 3, (2, 1)).await;
}

/// A route the content check skips keeps serving on the upstream the
/// release drops: neither is deleted, the upstream is reported kept, and
/// once the route's content is fixed it moves to its new upstream and the
/// old one goes.
///
/// ADR-0018: PERS-PROV-010.
async fn a_skipped_route_keeps_its_dropped_upstream_until_its_content_is_fixed(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;
    let old = id(&upstreams[0]);

    // The release replaces the upstream, and the route moves to the new one
    // with content that fails the check.
    let replacement = upstream_at(root, "new.example.com");
    let mut moved = routes[0].clone();
    moved.request.upstream_id = id(&replacement);
    let mut broken = moved.clone();
    broken.request.match_rules.http = None;
    let counts = f
        .ok(std::slice::from_ref(&replacement), &[broken], root)
        .await;
    assert_eq!(
        counts.upstreams,
        Tally {
            created: 1,
            kept: 1,
            ..NONE
        }
    );
    assert_eq!(counts.routes, Tally { failed: 1, ..NONE });
    assert!(!counts.is_clean());
    let route = f
        .route(root, rid(&routes[0]))
        .await
        .expect("the route serves");
    assert_eq!(route.upstream_id, old, "on its old upstream");
    assert!(
        f.upstream(root, old).await.is_some(),
        "the old upstream is kept"
    );
    assert!(
        f.failures()
            .contains(&failure("upstream", why::KEPT_IN_USE))
    );

    let counts = f
        .ok(std::slice::from_ref(&replacement), &[moved], root)
        .await;
    assert!(counts.is_clean(), "{counts:?}");
    assert_eq!(
        (counts.upstreams.removed, counts.routes.updated),
        (1, 1),
        "{counts:?}"
    );
    assert_eq!(
        f.route(root, rid(&routes[0])).await.unwrap().upstream_id,
        id(&replacement)
    );
    assert!(f.upstream(root, old).await.is_none());
}

/// A route moving to an upstream the content check skips stays on the
/// upstream the release drops, and keeps it: deleting the route first would
/// leave it on no upstream. Once the new upstream is fixed, the route moves
/// and the old upstream goes.
///
/// ADR-0018: PERS-PROV-010.
async fn a_route_moving_to_a_skipped_upstream_keeps_its_dropped_upstream(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;
    let old = id(&upstreams[0]);

    let replacement = upstream_at(root, "new.example.com");
    // An IP endpoint without the explicit alias it needs.
    let mut invalid = replacement.clone();
    set_host(&mut invalid, "10.0.0.1");
    let mut moved = routes[0].clone();
    moved.request.upstream_id = id(&replacement);
    let counts = f
        .ok(
            std::slice::from_ref(&invalid),
            std::slice::from_ref(&moved),
            root,
        )
        .await;
    assert_eq!(
        counts.upstreams,
        Tally {
            kept: 1,
            failed: 1,
            ..NONE
        }
    );
    assert_eq!(
        counts.routes,
        Tally { failed: 1, ..NONE },
        "the move finds no upstream"
    );
    let route = f
        .route(root, rid(&routes[0]))
        .await
        .expect("the route serves");
    assert_eq!(route.upstream_id, old, "on its old upstream");
    assert!(
        f.upstream(root, old).await.is_some(),
        "the old upstream is kept"
    );

    let counts = f
        .ok(
            std::slice::from_ref(&replacement),
            std::slice::from_ref(&moved),
            root,
        )
        .await;
    assert!(counts.is_clean(), "{counts:?}");
    assert_eq!(
        f.route(root, rid(&routes[0])).await.unwrap().upstream_id,
        id(&replacement)
    );
    assert!(f.upstream(root, old).await.is_none());
}

/// A run stopped right after a route moving off a dropped upstream was
/// deleted, before the upstream's removal: the route is absent and the old
/// upstream stays; the next run creates the route on its new upstream and
/// removes the old one.
///
/// ADR-0018: PERS-PROV-014.
async fn a_stop_after_a_moving_routes_delete_leaves_the_rest_to_the_next_run(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;
    let old = id(&upstreams[0]);
    let replacement = upstream_at(root, "new.example.com");
    let mut moved = routes[0].clone();
    moved.request.upstream_id = id(&replacement);
    let (upstreams, routes) = (vec![replacement.clone()], vec![moved]);

    let counts = reconcile_registry(
        &*f.provisioner,
        &f.metrics,
        &StopAfter::writes(1),
        &upstreams,
        &routes,
        root,
    )
    .await
    .unwrap();
    assert!(counts.stopped && !counts.is_clean(), "{counts:?}");
    assert!(
        f.route(root, rid(&routes[0])).await.is_none(),
        "deleted with its upstream"
    );
    assert!(
        f.upstream(root, old).await.is_some(),
        "stopped before the upstream's removal"
    );
    assert!(f.upstream(root, id(&replacement)).await.is_none());

    let next = f.ok(&upstreams, &routes, root).await;
    assert!(next.is_clean(), "{next:?}");
    assert_eq!(
        (
            next.upstreams.created,
            next.upstreams.removed,
            next.routes.created
        ),
        (1, 1, 1),
        "{next:?}"
    );
    assert_eq!(
        f.route(root, rid(&routes[0])).await.unwrap().upstream_id,
        id(&replacement)
    );
    assert!(f.upstream(root, old).await.is_none());
}

/// A route whose instance still names the upstream the release drops is
/// skipped (unknown upstream), and keeps serving with that upstream.
///
/// ADR-0018: PERS-PROV-010.
async fn a_route_naming_its_dropped_upstream_keeps_it_serving(b: Backend) {
    let f = Fixture::new(b);
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    f.ok(&upstreams, &routes, root).await;

    let counts = f.ok(&[], &routes, root).await;
    assert_eq!(counts.upstreams, Tally { kept: 1, ..NONE });
    assert_eq!(counts.routes, Tally { failed: 1, ..NONE });
    assert!(f.route(root, rid(&routes[0])).await.is_some());
    assert!(f.upstream(root, id(&upstreams[0])).await.is_some());
    assert_eq!(
        f.failures(),
        vec![
            failure("route", why::UNKNOWN_UPSTREAM),
            failure("upstream", why::KEPT_IN_USE),
        ]
    );

    // Dropping the route too lets both go.
    let counts = f.ok(&[], &[], root).await;
    assert!(counts.is_clean(), "{counts:?}");
    assert!(f.upstream(root, id(&upstreams[0])).await.is_none());
}

/// How a write fails.
#[derive(Clone, Copy)]
enum WriteFailure {
    /// Storage is down: the run aborts.
    Outage,
    /// The write is refused: the row is skipped and keeps serving.
    Refusal,
    /// The write never answers: the call timeout aborts the run.
    Hang,
}

impl WriteFailure {
    async fn fail<T>(self) -> Result<T, RepositoryError> {
        match self {
            Self::Outage => Err(RepositoryError::Internal("storage unavailable".into())),
            Self::Refusal => Err(RepositoryError::Conflict {
                entity: "upstream",
                resource: "row".into(),
                detail: "refused".into(),
            }),
            Self::Hang => std::future::pending().await,
        }
    }
}

/// Registry rows whose upstream delete fails.
struct FailingUpstreamDelete(Arc<dyn RegistryRowRepository>, WriteFailure);

#[async_trait]
impl RegistryRowRepository for FailingUpstreamDelete {
    async fn list_upstreams(&self) -> Result<Vec<Upstream>, RepositoryError> {
        self.0.list_upstreams().await
    }
    async fn list_routes(&self) -> Result<Vec<Route>, RepositoryError> {
        self.0.list_routes().await
    }
    async fn delete_upstream_if_unused(
        &self,
        _: Uuid,
        _: Uuid,
    ) -> Result<DeleteOutcome, RepositoryError> {
        self.1.fail().await
    }
}

/// Which route write fails.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RouteWrite {
    Create,
    Delete,
}

/// Routes whose create or delete fails.
struct FailingRoutes(Arc<dyn RouteRepository>, RouteWrite, WriteFailure);

#[async_trait]
impl RouteRepository for FailingRoutes {
    async fn create(&self, route: Route) -> Result<Route, RepositoryError> {
        if self.1 == RouteWrite::Create {
            return self.2.fail().await;
        }
        self.0.create(route).await
    }
    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Route, RepositoryError> {
        self.0.get_by_id(tenant_id, id).await
    }
    async fn list(
        &self,
        tenant_id: Uuid,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, RepositoryError> {
        self.0.list(tenant_id, upstream_id, query).await
    }
    async fn find_matching_in_tenants(
        &self,
        tenant_chain: &[Uuid],
        upstream_ids: &[Uuid],
        method: &str,
        path: &str,
        tags: crate::domain::repo::Tags,
    ) -> Result<Route, RepositoryError> {
        self.0
            .find_matching_in_tenants(tenant_chain, upstream_ids, method, path, tags)
            .await
    }
    async fn list_proxy_routes(
        &self,
        upstreams: &[crate::domain::repo::RowKey],
    ) -> Result<Vec<(crate::domain::repo::RowKey, Vec<Route>)>, RepositoryError> {
        self.0.list_proxy_routes(upstreams).await
    }
    async fn update(&self, route: Route) -> Result<Route, RepositoryError> {
        self.0.update(route).await
    }
    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        if self.1 == RouteWrite::Delete {
            return self.2.fail().await;
        }
        self.0.delete(tenant_id, id).await
    }
    async fn delete_by_upstream(
        &self,
        tenant_id: Uuid,
        upstream_id: Uuid,
    ) -> Result<(), RepositoryError> {
        self.0.delete_by_upstream(tenant_id, upstream_id).await
    }
}

/// One upstream and its route stored, then a release that drops both, on
/// a store whose removal writes fail as given.
async fn drop_all_with(
    upstream_delete: Option<WriteFailure>,
    route_delete: Option<WriteFailure>,
) -> (
    Fixture,
    Uuid,
    Uuid,
    Uuid,
    anyhow::Result<ProvisioningCounts>,
) {
    let stores = Stores::new(Backend::Memory);
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let tenants: Arc<dyn TenantResolverClient> =
        Arc::new(MockTenantResolverClient::single_tenant());
    Fixture::over(
        stores.clone(),
        tenants.clone(),
        Arc::new(MockCredStoreClient::empty()),
    )
    .ok(&upstreams, &routes, root)
    .await;

    let f = Fixture::over(
        Stores {
            routes: match route_delete {
                Some(how) => Arc::new(FailingRoutes(
                    stores.routes.clone(),
                    RouteWrite::Delete,
                    how,
                )),
                None => stores.routes.clone(),
            },
            rows: match upstream_delete {
                Some(how) => Arc::new(FailingUpstreamDelete(stores.rows.clone(), how)),
                None => stores.rows.clone(),
            },
            ..stores
        },
        tenants,
        Arc::new(MockCredStoreClient::empty()),
    );
    let result = f.run(&[], &[], root).await;
    (f, root, id(&upstreams[0]), rid(&routes[0]), result)
}

/// Removals follow the reject-or-fatal rule: an outage aborts the run, a
/// refusal skips that row, which keeps serving, and the run goes on.
///
/// ADR-0018: PERS-PROV-015.
#[tokio::test]
async fn removal_failures_are_fatal_or_skip_the_row() {
    let (_, _, _, _, result) = drop_all_with(None, Some(WriteFailure::Outage)).await;
    assert!(result.is_err(), "a storage outage on a route delete aborts");
    let (_, _, _, _, result) = drop_all_with(Some(WriteFailure::Outage), None).await;
    assert!(
        result.is_err(),
        "a storage outage on an upstream delete aborts"
    );

    let (f, root, upstream, route, result) = drop_all_with(None, Some(WriteFailure::Refusal)).await;
    let counts = result.expect("a refusal does not abort");
    assert_eq!(counts.routes, Tally { failed: 1, ..NONE });
    assert_eq!(
        counts.upstreams,
        Tally { kept: 1, ..NONE },
        "its route keeps it"
    );
    assert!(f.route(root, route).await.is_some());
    assert!(f.upstream(root, upstream).await.is_some());

    let (f, root, upstream, route, result) = drop_all_with(Some(WriteFailure::Refusal), None).await;
    let counts = result.expect("a refusal does not abort");
    assert_eq!(counts.routes, Tally { removed: 1, ..NONE });
    assert_eq!(counts.upstreams, Tally { failed: 1, ..NONE });
    assert!(f.route(root, route).await.is_none());
    assert!(
        f.upstream(root, upstream).await.is_some(),
        "the refused row serves"
    );
    assert_eq!(f.failures(), vec![failure("upstream", why::CONFLICT)]);
}

/// A write that never answers aborts the run once the call timeout has
/// passed, as an infrastructure error: a phase-3 removal of a route or an
/// upstream, and a phase-5 route create.
///
/// ADR-0018: PERS-PROV-016.
#[tokio::test(start_paused = true)]
async fn a_hung_write_aborts_the_run_after_the_call_timeout() {
    use crate::domain::services::registry_reconcile::{CALL_TIMEOUT, CallTimedOut};

    let check =
        |what: &str, started: tokio::time::Instant, result: anyhow::Result<ProvisioningCounts>| {
            let e = result.expect_err(what);
            assert!(e.is::<CallTimedOut>(), "{what}: {e:#}");
            let waited = started.elapsed();
            assert!(
                waited >= CALL_TIMEOUT && waited < CALL_TIMEOUT * 2,
                "{what}: aborted after {waited:?}"
            );
        };

    let started = tokio::time::Instant::now();
    let (_, _, _, _, result) = drop_all_with(None, Some(WriteFailure::Hang)).await;
    check("a hung route removal", started, result);
    let started = tokio::time::Instant::now();
    let (_, _, _, _, result) = drop_all_with(Some(WriteFailure::Hang), None).await;
    check("a hung upstream removal", started, result);

    let stores = Stores::new(Backend::Memory);
    let f = Fixture::over(
        Stores {
            routes: Arc::new(FailingRoutes(
                stores.routes.clone(),
                RouteWrite::Create,
                WriteFailure::Hang,
            )),
            ..stores
        },
        Arc::new(MockTenantResolverClient::single_tenant()),
        Arc::new(MockCredStoreClient::empty()),
    );
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let started = tokio::time::Instant::now();
    let result = f.run(&upstreams, &routes, root).await;
    check("a hung route create", started, result);
    assert!(
        f.upstream(root, id(&upstreams[0])).await.is_some(),
        "phase 4 ran before the hang"
    );
}

// ---------------------------------------------------------------------------
// The proxy configuration cache (ADR-0020)
// ---------------------------------------------------------------------------

/// Every reconcile write is seen by the next lookup through the cache:
/// creates, upstream and route updates, a route move, and the removal,
/// whose upstreams go through `delete_upstream_if_unused` (a route delete
/// evicts no alias entry, so the alias lookups see only that).
///
/// ADR-0020: CACHE-FRESH-002.
#[tokio::test]
async fn reconcile_writes_are_seen_through_the_proxy_cache() {
    let root = Uuid::new_v4();
    let f = CachedPlanes::new(Arc::new(MockTenantResolverClient::single_tenant())).await;
    let metrics = ProvisioningMetrics::default();
    let run = async |upstreams: &[ProvisionedUpstream], routes: &[ProvisionedRoute]| {
        let counts = reconcile_registry(
            &*f.provisioner,
            &metrics,
            &Unattended,
            upstreams,
            routes,
            root,
        )
        .await
        .unwrap();
        assert!(counts.is_clean(), "{counts:?}");
    };
    let (mut upstreams, mut routes) = registry_instances(root);
    let other = upstream_at(root, "other.example.com");
    upstreams.push(other.clone());
    let (v1, v2) = (Some(("POST", "/v1")), Some(("POST", "/v2")));
    let api = |target| (root, "api.example.com", target);
    let moved_to = |target| (root, "other.example.com", target);

    f.assert_sees(
        "create",
        &[api(None), api(v1), moved_to(None)],
        async || run(&upstreams, &routes).await,
    )
    .await;

    upstreams[0].request.enabled = false;
    f.assert_sees("upstream update", &[api(None), api(v1)], async || {
        run(&upstreams, &routes).await;
    })
    .await;
    upstreams[0].request.enabled = true;
    run(&upstreams, &routes).await;

    set_path(&mut routes[0], "/v2");
    f.assert_sees("route update", &[api(v1), api(v2)], async || {
        run(&upstreams, &routes).await;
    })
    .await;

    routes[0].request.upstream_id = id(&other);
    f.assert_sees("route move", &[api(v2), moved_to(v2)], async || {
        run(&upstreams, &routes).await;
    })
    .await;

    f.assert_sees("removal", &[api(None), moved_to(None)], async || {
        run(&[], &[]).await;
    })
    .await;
}
