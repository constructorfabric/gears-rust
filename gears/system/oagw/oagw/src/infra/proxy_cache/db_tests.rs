//! The cache over a real database: what a lookup reads, what a write
//! evicts, and the cache across replicas, two services over one database and
//! one cluster (ADR-0020).

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(feature = "cluster")]
use std::time::Duration;

use async_trait::async_trait;

use tenant_resolver_sdk::{TenantId, TenantResolverClient};
#[cfg(feature = "cluster")]
use tokio_util::sync::CancellationToken;
#[cfg(feature = "cluster")]
use toolkit::client_hub::ClientHub;
use toolkit_db::Db;
use toolkit_db::test_support::QueryRecorder;
use uuid::Uuid;

#[cfg(feature = "cluster")]
use super::testing::{ClusterFixture, ScriptedCache, facade, read_log, resolver, until};
#[cfg(feature = "cluster")]
use super::write_hook::PUBLISH_TIMEOUT;
#[cfg(feature = "cluster")]
use super::{Change, ChangeFeed, WatchState, WatchStatus};
use crate::domain::error::DomainError;
use crate::domain::model::{
    AuthConfig, ListQuery, Route, SharingMode, UpdateUpstreamRequest, Upstream,
};
use crate::domain::repo::{
    DeleteOutcome, RepositoryError, RouteRepository, RowKey, Tags, UpstreamRepository,
};
use crate::domain::services::registry_reconcile::provisioning_ctx;
#[cfg(feature = "cluster")]
use crate::domain::services::registry_reconcile::tests::registry_instances;
use crate::domain::services::registry_reconcile::tests::{
    Backend, CachedPlanes, Failed, Lookup, Resolved, Stores, is_disabled, is_not_found, resolve,
    route_on, upstream_at,
};
#[cfg(feature = "cluster")]
use crate::domain::services::registry_reconcile::{Unattended, reconcile_registry};
use crate::domain::services::{ControlPlaneService, RegistryProvisioner};
#[cfg(feature = "cluster")]
use crate::domain::test_support::ProvisioningMetrics;
use crate::domain::test_support::{MockTenantResolverClient, UnavailableStorage};
#[cfg(feature = "cluster")]
use crate::domain::type_provisioning::ProvisionedUpstream;
use crate::infra::storage::testing::recorded_sqlite;

/// Connect `planes` to the change log of the cluster in `hub`, as a replica
/// of its own whose watch reports to `watch`.
#[cfg(feature = "cluster")]
async fn connect(planes: &CachedPlanes, hub: &ClientHub, watch: &WatchStatus) -> ChangeFeed {
    ChangeFeed::start(
        resolver(hub),
        &planes.hook,
        Uuid::new_v4(),
        watch,
        CancellationToken::new(),
    )
    .await
    .expect("the oagw profile")
}

/// The full replacement that keeps `upstream` as it is.
fn unchanged(upstream: &Upstream) -> UpdateUpstreamRequest {
    UpdateUpstreamRequest {
        server: upstream.server.clone(),
        protocol: upstream.protocol.clone(),
        alias: Some(upstream.alias.clone()),
        auth: upstream.auth.clone(),
        headers: upstream.headers.clone(),
        plugins: upstream.plugins.clone(),
        rate_limit: upstream.rate_limit.clone(),
        cors: upstream.cors.clone(),
        tags: upstream.tags.clone(),
        enabled: upstream.enabled,
    }
}

/// Auth that descendants inherit, which shares the upstream with them.
fn shared_auth() -> Option<AuthConfig> {
    Some(AuthConfig {
        plugin_type: "noop".into(),
        sharing: SharingMode::Inherit,
        config: None,
    })
}

/// A repeated lookup runs no `SELECT` and answers the same; the first, a
/// full miss, runs the proxy path's two (upstreams, then their routes).
///
/// ADR-0020: CACHE-HIT-001.
async fn a_second_lookup_runs_no_select(db: Db, recorder: QueryRecorder) {
    let tenant = Uuid::new_v4();
    let f = CachedPlanes::over(
        Stores::new(Backend::Db(db)),
        recorder,
        Arc::new(MockTenantResolverClient::single_tenant()),
    );
    let ctx = provisioning_ctx(tenant).unwrap();
    let upstream = upstream_at(tenant, "api.example.com");
    f.cp.create_upstream(&ctx, upstream.request.clone())
        .await
        .unwrap();
    f.cp.create_route(&ctx, route_on(&upstream, "/v1").request)
        .await
        .unwrap();
    let lookup = (tenant, "api.example.com", Some(("POST", "/v1")));

    let selects = f.selects();
    let first = resolve(&f.cp, lookup).await;
    assert_eq!(f.selects() - selects, 2, "a full miss");
    assert!(first.is_ok(), "{first:?}");
    assert_eq!(first, resolve(&f.uncached, lookup).await);

    let selects = f.selects();
    assert_eq!(resolve(&f.cp, lookup).await, first);
    assert_eq!(f.selects(), selects, "a hit");
}

#[tokio::test]
async fn a_second_lookup_runs_no_select_on_sqlite() {
    let (db, recorder) = recorded_sqlite().await;
    a_second_lookup_runs_no_select(db, recorder).await;
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn a_second_lookup_runs_no_select_on_postgres() {
    let (_container, db, recorder) = crate::infra::storage::testing::recorded_postgres().await;
    a_second_lookup_runs_no_select(db, recorder).await;
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn a_second_lookup_runs_no_select_on_mysql() {
    let (_container, db, recorder) = crate::infra::storage::testing::recorded_mysql().await;
    a_second_lookup_runs_no_select(db, recorder).await;
}

/// Error answers are cached like the others: an unknown alias, a disabled
/// upstream, and a target no route matches each answer the same error
/// again, as without the cache, with no `SELECT`.
///
/// ADR-0020: CACHE-HIT-003.
#[tokio::test]
async fn not_found_and_disabled_lookups_run_no_select_when_repeated() {
    let tenant = Uuid::new_v4();
    let f = CachedPlanes::new(Arc::new(MockTenantResolverClient::single_tenant())).await;
    let ctx = provisioning_ctx(tenant).unwrap();
    let mut off = upstream_at(tenant, "off.example.com");
    off.request.enabled = false;
    f.cp.create_upstream(&ctx, off.request).await.unwrap();
    let served = upstream_at(tenant, "api.example.com");
    f.cp.create_upstream(&ctx, served.request.clone())
        .await
        .unwrap();
    f.cp.create_route(&ctx, route_on(&served, "/v1").request)
        .await
        .unwrap();

    let not_found: fn(&Resolved) -> bool = is_not_found;
    for (lookup, expected) in [
        ((tenant, "missing.example.com", None), not_found),
        (
            (tenant, "missing.example.com", Some(("POST", "/v1"))),
            not_found,
        ),
        ((tenant, "off.example.com", None), is_disabled),
        (
            (tenant, "off.example.com", Some(("POST", "/v1"))),
            is_disabled,
        ),
        (
            (tenant, "api.example.com", Some(("POST", "/v2"))),
            not_found,
        ),
    ] {
        let answer = f.cached(lookup).await;
        assert!(expected(&answer), "{lookup:?}: {answer:?}");
        assert_eq!(answer, resolve(&f.uncached, lookup).await, "{lookup:?}");
    }
}

/// Run `write` on `f`: the `seen` lookups see it (as
/// [`CachedPlanes::assert_sees`] checks), while the `kept` ones, cached
/// before, stay cached: they answer as before and run no `SELECT`.
async fn leaves_cached<T>(
    f: &CachedPlanes,
    kept: &[Lookup<'_>],
    name: &str,
    seen: &[Lookup<'_>],
    write: impl AsyncFnOnce() -> T,
) {
    let mut before = Vec::with_capacity(kept.len());
    for &lookup in kept {
        before.push(f.cached(lookup).await);
    }
    f.assert_sees(name, seen, write).await;
    let selects = f.selects();
    for (&lookup, before) in kept.iter().zip(before) {
        assert_eq!(resolve(&f.cp, lookup).await, before, "{name}: {lookup:?}");
    }
    assert_eq!(
        f.selects(),
        selects,
        "{name} keeps the other tenants' entries"
    );
}

/// A write in one tenant evicts nothing another tenant's lookups read:
/// sibling tenants `a` and `b` share an alias; `a`'s lookups see each of
/// its upstream and route writes, while `b`'s and the root's entries (found
/// and not found) stay cached.
///
/// ADR-0020: CACHE-EVICT-005.
#[tokio::test]
async fn a_write_in_one_tenant_leaves_other_tenants_entries_cached() {
    let [root, a, b] = std::array::from_fn(|_| Uuid::new_v4());
    let f = CachedPlanes::new(Arc::new(MockTenantResolverClient::with_siblings(
        TenantId(root),
        vec![TenantId(a), TenantId(b)],
    )))
    .await;
    let mut served = Vec::new();
    for tenant in [a, b] {
        let ctx = provisioning_ctx(tenant).unwrap();
        let upstream = upstream_at(tenant, "api.example.com");
        f.cp.create_upstream(&ctx, upstream.request.clone())
            .await
            .unwrap();
        let route =
            f.cp.create_route(&ctx, route_on(&upstream, "/v1").request)
                .await
                .unwrap();
        served.push((upstream.request.id.unwrap(), route));
    }
    let ctx = provisioning_ctx(a).unwrap();
    let (upstream, route) = served.swap_remove(0);
    let (v1, v2) = (Some(("POST", "/v1")), Some(("POST", "/v2")));
    let kept: Vec<Lookup<'_>> = vec![
        (b, "api.example.com", None),
        (b, "api.example.com", v1),
        (b, "api.example.com", v2),
        (b, "new.example.com", None),
        (root, "api.example.com", None),
        (root, "new.example.com", None),
    ];
    let api = |target| (a, "api.example.com", target);
    let created = route_on(&upstream_at(a, "api.example.com"), "/v2").request;
    let created = crate::domain::model::CreateRouteRequest {
        upstream_id: upstream,
        ..created
    };
    let created_id = created.id.unwrap();
    leaves_cached(&f, &kept, "route create", &[api(v2)], async || {
        f.cp.create_route(&ctx, created).await.unwrap();
    })
    .await;
    leaves_cached(&f, &kept, "route update", &[api(v2)], async || {
        let route = f.cp.get_route(&ctx, created_id).await.unwrap();
        let mut req = crate::domain::model::UpdateRouteRequest {
            match_rules: route.match_rules,
            plugins: route.plugins,
            rate_limit: route.rate_limit,
            cors: route.cors,
            tags: route.tags,
            priority: route.priority,
            enabled: route.enabled,
        };
        req.match_rules.http.as_mut().unwrap().path = "/v3".into();
        f.cp.update_route(&ctx, created_id, req).await.unwrap();
    })
    .await;
    leaves_cached(&f, &kept, "route delete", &[api(v1)], async || {
        f.cp.delete_route(&ctx, route.id).await.unwrap();
    })
    .await;
    leaves_cached(
        &f,
        &kept,
        "upstream create",
        &[(a, "new.example.com", None)],
        async || {
            let created = upstream_at(a, "new.example.com");
            f.cp.create_upstream(&ctx, created.request).await.unwrap();
        },
    )
    .await;
    leaves_cached(
        &f,
        &kept,
        "upstream update",
        &[api(None), api(v2)],
        async || {
            let current = f.cp.get_upstream(&ctx, upstream).await.unwrap();
            let req = UpdateUpstreamRequest {
                enabled: false,
                ..unchanged(&current)
            };
            f.cp.update_upstream(&ctx, upstream, req).await.unwrap();
        },
    )
    .await;
    leaves_cached(
        &f,
        &kept,
        "upstream delete",
        &[api(None), api(v2)],
        async || {
            f.cp.delete_upstream(&ctx, upstream).await.unwrap();
        },
    )
    .await;
}

/// Deleting an upstream, through the cache: under a root that shares its
/// upstream for an alias, a child's own upstream for it shadows the root's.
/// Deleting the child's makes the alias fall back to the root's and the
/// child's routes match nothing; deleting the root's makes the alias not
/// found for both, and its routes match nothing. A registry upstream
/// deleted when unused is not found either.
///
/// ADR-0020: CACHE-FRESH-005.
#[tokio::test]
async fn deleting_an_upstream_makes_its_alias_not_found_and_its_routes_unmatched() {
    let [root, child] = std::array::from_fn(|_| Uuid::new_v4());
    let f = CachedPlanes::new(Arc::new(MockTenantResolverClient::with_hierarchy(vec![
        TenantId(root),
        TenantId(child),
    ])))
    .await;
    let mut ids = Vec::new();
    for (tenant, path) in [(root, "/v1"), (child, "/v2")] {
        let ctx = provisioning_ctx(tenant).unwrap();
        let mut upstream = upstream_at(tenant, "api.example.com");
        upstream.request.auth = shared_auth();
        f.cp.create_upstream(&ctx, upstream.request.clone())
            .await
            .unwrap();
        f.cp.create_route(&ctx, route_on(&upstream, path).request)
            .await
            .unwrap();
        ids.push(upstream.request.id.unwrap());
    }
    let (v1, v2) = (Some(("POST", "/v1")), Some(("POST", "/v2")));
    let api = |tenant, target| (tenant, "api.example.com", target);
    let not_found = async |lookups: &[Lookup<'_>]| {
        for &lookup in lookups {
            let answer = resolve(&f.cp, lookup).await;
            assert!(is_not_found(&answer), "{lookup:?}: {answer:?}");
        }
    };

    f.assert_sees(
        "the child's upstream deleted",
        &[api(child, None), api(child, v1), api(child, v2)],
        async || {
            let ctx = provisioning_ctx(child).unwrap();
            f.cp.delete_upstream(&ctx, ids[1]).await.unwrap();
        },
    )
    .await;
    not_found(&[api(child, v2)]).await;
    let fallback = resolve(&f.cp, api(child, None)).await.unwrap();
    assert_eq!(fallback.0.id, ids[0], "the root's upstream");

    let gone = [
        api(root, None),
        api(root, v1),
        api(child, None),
        api(child, v1),
    ];
    f.assert_sees("the root's upstream deleted", &gone, async || {
        let ctx = provisioning_ctx(root).unwrap();
        f.cp.delete_upstream(&ctx, ids[0]).await.unwrap();
    })
    .await;
    not_found(&gone).await;

    let ctx = provisioning_ctx(root).unwrap();
    let registry = upstream_at(root, "registry.example.com");
    f.provisioner
        .create_registry_upstream(&ctx, registry.request.clone())
        .await
        .unwrap();
    let lookup = (root, "registry.example.com", None);
    f.assert_sees(
        "an unused registry upstream deleted",
        &[lookup],
        async || {
            let outcome = f
                .provisioner
                .delete_registry_upstream_if_unused(&ctx, registry.request.id.unwrap())
                .await
                .unwrap();
            assert_eq!(outcome, DeleteOutcome::Deleted);
        },
    )
    .await;
    not_found(&[lookup]).await;
}

/// Repositories that pass every call on to `upstreams` and `routes` while
/// up, and fail it as a database outage does ([`UnavailableStorage`]) while
/// down. `failed` counts the calls that failed.
struct Outage {
    upstreams: Arc<dyn UpstreamRepository>,
    routes: Arc<dyn RouteRepository>,
    down: AtomicBool,
    failed: AtomicUsize,
}

impl Outage {
    fn over(stores: &Stores) -> Arc<Self> {
        Arc::new(Self {
            upstreams: stores.upstreams.clone(),
            routes: stores.routes.clone(),
            down: AtomicBool::new(false),
            failed: AtomicUsize::new(0),
        })
    }

    fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    fn failed(&self) -> usize {
        self.failed.load(Ordering::SeqCst)
    }

    /// Whether this call fails, counting it if so.
    fn fails(&self) -> bool {
        let down = self.down.load(Ordering::SeqCst);
        if down {
            self.failed.fetch_add(1, Ordering::SeqCst);
        }
        down
    }

    fn upstream_repo(&self) -> &dyn UpstreamRepository {
        if self.fails() {
            &UnavailableStorage
        } else {
            &*self.upstreams
        }
    }

    fn route_repo(&self) -> &dyn RouteRepository {
        if self.fails() {
            &UnavailableStorage
        } else {
            &*self.routes
        }
    }
}

#[async_trait]
impl UpstreamRepository for Outage {
    async fn create(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        self.upstream_repo().create(upstream).await
    }

    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Upstream, RepositoryError> {
        self.upstream_repo().get_by_id(tenant_id, id).await
    }

    async fn list(
        &self,
        tenant_id: Uuid,
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        self.upstream_repo().list(tenant_id, query).await
    }

    async fn update(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        self.upstream_repo().update(upstream).await
    }

    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        self.upstream_repo().delete(tenant_id, id).await
    }

    async fn list_by_alias_for_tenants(
        &self,
        alias: &str,
        tenant_ids: &HashSet<Uuid>,
        tags: Tags,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        self.upstream_repo()
            .list_by_alias_for_tenants(alias, tenant_ids, tags)
            .await
    }
}

#[async_trait]
impl RouteRepository for Outage {
    async fn create(&self, route: Route) -> Result<Route, RepositoryError> {
        self.route_repo().create(route).await
    }

    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Route, RepositoryError> {
        self.route_repo().get_by_id(tenant_id, id).await
    }

    async fn list(
        &self,
        tenant_id: Uuid,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, RepositoryError> {
        self.route_repo().list(tenant_id, upstream_id, query).await
    }

    async fn find_matching_in_tenants(
        &self,
        tenant_chain: &[Uuid],
        upstream_ids: &[Uuid],
        method: &str,
        path: &str,
        tags: Tags,
    ) -> Result<Route, RepositoryError> {
        self.route_repo()
            .find_matching_in_tenants(tenant_chain, upstream_ids, method, path, tags)
            .await
    }

    async fn list_proxy_routes(
        &self,
        upstreams: &[RowKey],
    ) -> Result<Vec<(RowKey, Vec<Route>)>, RepositoryError> {
        self.route_repo().list_proxy_routes(upstreams).await
    }

    async fn update(&self, route: Route) -> Result<Route, RepositoryError> {
        self.route_repo().update(route).await
    }

    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        self.route_repo().delete(tenant_id, id).await
    }

    async fn delete_by_upstream(
        &self,
        tenant_id: Uuid,
        upstream_id: Uuid,
    ) -> Result<(), RepositoryError> {
        self.route_repo()
            .delete_by_upstream(tenant_id, upstream_id)
            .await
    }
}

/// With the repositories failing as in a database outage, cached lookups
/// are still served without reading them; a miss in either section answers
/// the repository's error, which is not stored: every miss reads the
/// repository again, and once it is back the next miss loads and answers as
/// without the cache.
///
/// ADR-0020: CACHE-FAIL-001.
#[tokio::test]
async fn with_the_database_down_hits_are_served_and_misses_fail_until_it_is_back() {
    let tenant = Uuid::new_v4();
    let (db, recorder) = recorded_sqlite().await;
    let stores = Stores::new(Backend::Db(db));
    let outage = Outage::over(&stores);
    let f = CachedPlanes::over(
        Stores {
            upstreams: outage.clone(),
            routes: outage.clone(),
            ..stores
        },
        recorder,
        Arc::new(MockTenantResolverClient::single_tenant()),
    );
    let ctx = provisioning_ctx(tenant).unwrap();
    for host in ["api.example.com", "other.example.com", "third.example.com"] {
        let upstream = upstream_at(tenant, host);
        f.cp.create_upstream(&ctx, upstream.request.clone())
            .await
            .unwrap();
        f.cp.create_route(&ctx, route_on(&upstream, "/v1").request)
            .await
            .unwrap();
    }
    let v1 = Some(("POST", "/v1"));
    let hit = (tenant, "api.example.com", v1);
    let route_miss = (tenant, "other.example.com", v1);
    let alias_miss = (tenant, "third.example.com", None);
    let served = f.cached(hit).await;
    assert!(served.is_ok(), "{served:?}");
    f.cached((tenant, "other.example.com", None))
        .await
        .expect("other is served");

    outage.set_down(true);
    assert_eq!(resolve(&f.cp, hit).await, served, "a hit is served");
    assert_eq!(outage.failed(), 0, "a hit reads no repository");
    for miss in [route_miss, alias_miss] {
        for attempt in 1..=2 {
            let failed = outage.failed();
            let answer = resolve(&f.cp, miss).await;
            assert!(
                matches!(answer, Err(Failed(DomainError::Internal { .. }))),
                "{miss:?}, attempt {attempt}: {answer:?}"
            );
            assert_eq!(
                outage.failed(),
                failed + 1,
                "{miss:?}, attempt {attempt}: the miss reads the repository once"
            );
        }
    }

    outage.set_down(false);
    for miss in [route_miss, alias_miss] {
        let answer = resolve(&f.cp, miss).await;
        assert!(answer.is_ok(), "{miss:?}: {answer:?}");
        assert_eq!(answer, resolve(&f.uncached, miss).await, "{miss:?}");
    }
}

/// Wait until `planes`' watcher has applied `changes` changes of other
/// replicas in all, each on its own (not through a flush), so every change
/// logged before them has reached its cache. Within 10 s.
#[cfg(feature = "cluster")]
async fn applied(planes: &CachedPlanes, changes: u64) {
    until(&format!("{changes} remote changes are applied"), || {
        planes.metrics.count("changes remote") >= changes
    })
    .await;
    assert_eq!(planes.metrics.count("changes remote"), changes);
}

/// A write on replica A reaches replica B through the change log of the
/// cluster in `hub`: B's entry, cached before, answers as without the cache
/// once the change is delivered, and B's entries the write does not affect
/// stay cached.
///
/// Changes reach B on their own time (over Redis, a keyspace notification
/// per append), so the test waits for B to have applied them, counted. The
/// log exists before the watches start, so each watcher starts from its
/// baseline and applies every later change on its own: a log B first saw
/// part of would be a new log, flushed and followed from where it stood.
#[cfg(feature = "cluster")]
async fn a_write_on_one_replica_is_seen_on_another(hub: &ClientHub) {
    let root = Uuid::new_v4();
    let (db, recorder) = recorded_sqlite().await;
    let stores = Stores::new(Backend::Db(db));
    let tenants: Arc<dyn TenantResolverClient> =
        Arc::new(MockTenantResolverClient::single_tenant());
    let a = CachedPlanes::over(stores.clone(), recorder.clone(), tenants.clone());
    let b = CachedPlanes::over(stores, recorder, tenants);
    let (third, stop_third) = super::testing::publisher(hub, Uuid::new_v4()).await;
    third
        .submit(
            Change::RouteDeleted {
                tenant_id: root,
                id: Uuid::new_v4(),
            },
            None,
        )
        .expect("the publisher runs");
    let log = facade(hub).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while read_log(&log).await.is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the log is created");
    stop_third.cancel();
    let watches = [WatchStatus::default(), WatchStatus::default()];
    let feeds = [
        connect(&a, hub, &watches[0]).await,
        connect(&b, hub, &watches[1]).await,
    ];
    // A watch empties its replica's cache when it starts.
    until("both watches are up", || {
        watches.iter().all(|watch| watch.get() == WatchState::Up)
    })
    .await;

    let metrics = ProvisioningMetrics::default();
    let (mut upstreams, routes) = registry_instances(root);
    upstreams.push(upstream_at(root, "other.example.com"));
    let run = async |upstreams: &[ProvisionedUpstream]| {
        let counts = reconcile_registry(
            &*a.provisioner,
            &metrics,
            &Unattended,
            upstreams,
            &routes,
            root,
        )
        .await
        .unwrap();
        assert!(counts.is_clean(), "{counts:?}");
    };
    run(&upstreams).await;
    // Two upstreams and a route created.
    applied(&b, 3).await;

    let api = (root, "api.example.com", Some(("POST", "/v1")));
    let other = (root, "other.example.com", None);
    let before = b.cached(api).await;
    b.cached(other).await.expect("other is served");

    upstreams[0].request.enabled = false;
    run(&upstreams).await;
    // The upstream updated.
    applied(&b, 4).await;
    let after = resolve(&b.cp, api).await;
    assert_ne!(after, before, "B sees A's write");
    assert_eq!(after, resolve(&b.uncached, api).await);

    let selects = b.selects();
    resolve(&b.cp, other).await.expect("other is served");
    assert_eq!(b.selects(), selects, "B's other entry is still cached");
    let flushes: u64 = b
        .metrics
        .counts()
        .iter()
        .filter(|(counter, _)| counter.starts_with("flushes "))
        .map(|(_, n)| n)
        .sum();
    assert_eq!(flushes, 1, "only the flush of the watch start");

    for feed in feeds {
        feed.stop(CancellationToken::new()).await;
    }
}

/// [`a_write_on_one_replica_is_seen_on_another`] over the standalone
/// backend.
///
/// ADR-0020: CACHE-FRESH-003.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn a_write_on_one_replica_is_seen_on_another_after_delivery() {
    let hub = Arc::new(ClientHub::new());
    let cluster = ClusterFixture::standalone(&hub);
    a_write_on_one_replica_is_seen_on_another(&hub).await;
    cluster.stop().await;
}

/// A registry reconcile run's writes do not each wait for the change log
/// (only API callers do): with the log unreachable, a run of five writes
/// waits two seconds once, at its end, not two seconds per write, and its
/// changes all reach the log once it is back.
///
/// ADR-0020: CACHE-LOG-003.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn a_reconcile_run_waits_for_the_change_log_once() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = ClusterFixture::scripted(&hub, |cache| cache);
    let root = Uuid::new_v4();
    let planes = CachedPlanes::new(Arc::new(MockTenantResolverClient::single_tenant())).await;
    let feed = connect(&planes, &hub, &WatchStatus::default()).await;
    let (mut upstreams, routes) = registry_instances(root);
    for alias in ["b.example.com", "c.example.com", "d.example.com"] {
        upstreams.push(upstream_at(root, alias));
    }

    cache.hold();
    let started = std::time::Instant::now();
    let counts = reconcile_registry(
        &*planes.provisioner,
        &ProvisioningMetrics::default(),
        &Unattended,
        &upstreams,
        &routes,
        root,
    )
    .await
    .unwrap();
    let took = started.elapsed();
    assert_eq!(counts.upstreams.created + counts.routes.created, 5);
    assert!(
        // A wait per write would take five times as long.
        took >= PUBLISH_TIMEOUT && took < PUBLISH_TIMEOUT * 4,
        "the run waited once, not per write: {took:?}"
    );

    cache.release();
    let log = facade(&hub).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while read_log(&log).await.map(|log| log.seq()) != Some(5) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the run's five changes are logged");
    assert!(cache.writes() <= 2, "{} appends", cache.writes());

    feed.stop(CancellationToken::new()).await;
    cluster.stop().await;
}

/// A Redis container on the workspace pin (`test_containers::redis()`),
/// and its URL: no snapshots,
/// the keyspace notifications the plugin's watches need (`Kxe`), and the
/// default `noeviction` memory policy, the one the plugin rates safe.
#[cfg(feature = "integration")]
async fn start_redis() -> (impl std::any::Any, String) {
    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;

    let container = test_containers::redis()
        .with_cmd([
            "redis-server",
            "--save",
            "",
            "--notify-keyspace-events",
            "Kxe",
        ])
        .start()
        .await
        .expect("start redis container");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("redis port");
    (container, format!("redis://127.0.0.1:{port}"))
}

/// The cluster gear as the runtime finds it, initialized over the cluster
/// config `config` and `hub`, and started: what a binary linking it runs.
#[cfg(feature = "integration")]
async fn cluster_gear(
    hub: &Arc<ClientHub>,
    config: serde_json::Value,
) -> Arc<dyn toolkit::contracts::RunnableCapability> {
    use toolkit::registry::RunnableCap;
    // Linked, so the inventory resolves the OAGW gear's dependencies.
    use {credstore as _, tenant_resolver as _, types_registry as _};

    struct ClusterSection(serde_json::Value);

    impl toolkit::ConfigProvider for ClusterSection {
        fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
            (gear == "cluster").then_some(&self.0)
        }
    }

    let registry = toolkit::GearRegistry::discover_and_build().expect("the linked gears");
    let entry = registry
        .gears()
        .iter()
        .find(|entry| entry.name() == "cluster")
        .expect("the cluster gear is linked");
    let ctx = toolkit::GearCtx::new(
        "cluster",
        Uuid::new_v4(),
        Arc::new(ClusterSection(serde_json::json!({ "config": config }))),
        Arc::clone(hub),
        CancellationToken::new(),
    );
    registry
        .get_gear("cluster")
        .expect("the cluster gear")
        .init(&ctx)
        .await
        .expect("init the cluster gear");
    let runnable = entry.caps().query::<RunnableCap>().expect("stateful");
    runnable
        .start(CancellationToken::new())
        .await
        .expect("start the cluster gear");
    runnable
}

/// [`a_write_on_one_replica_is_seen_on_another`] with the `oagw` profile on
/// Redis, bound in the cluster gear's config as an operator binds it: the
/// cache on Redis with `watch_mode: publish`, and, since a Redis cache is
/// only eventually consistent, the lock on Redis and leader election on the
/// SDK default with `allow_weak_consistency` (OAGW uses neither, but the
/// profile does not start without them).
///
/// ADR-0020: CACHE-FRESH-003.
#[cfg(feature = "integration")]
#[tokio::test]
async fn a_write_on_one_replica_is_seen_on_another_over_redis() {
    let (_redis, url) = start_redis().await;
    let hub = Arc::new(ClientHub::new());
    let cluster = cluster_gear(
        &hub,
        serde_json::json!({ "profiles": { "oagw": {
            "cache": { "provider": "redis", "url": url, "watch_mode": "publish" },
            "lock": { "provider": "redis", "url": url },
            "leader_election": { "provider": "default", "allow_weak_consistency": true },
        } } }),
    )
    .await;
    assert_eq!(
        facade(&hub).await.consistency(),
        cluster_sdk::CacheConsistency::EventuallyConsistent,
        "the profile runs on Redis"
    );
    a_write_on_one_replica_is_seen_on_another(&hub).await;
    cluster
        .stop(CancellationToken::new())
        .await
        .expect("stop the cluster gear");
}

/// A change another replica never hears of (its watch delivers nothing)
/// shows there once the entry expires: until the TTL the replica answers as
/// before the write, from the TTL on as the database does. The clock is
/// paused only while no statement runs (a paused clock fires the pool's
/// timeouts during `SQLite` calls).
///
/// ADR-0020: CACHE-FRESH-007.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn a_lost_change_shows_on_another_replica_after_the_ttl() {
    const TTL: Duration = Duration::from_secs(30);
    let tenant = Uuid::new_v4();
    let (db, recorder) = recorded_sqlite().await;
    let stores = Stores::new(Backend::Db(db));
    let tenants: Arc<dyn TenantResolverClient> =
        Arc::new(MockTenantResolverClient::single_tenant());
    let a = CachedPlanes::with_ttl(stores.clone(), recorder.clone(), tenants.clone(), TTL);
    let b = CachedPlanes::with_ttl(stores, recorder, tenants, TTL);
    let hub = Arc::new(ClientHub::new());
    // Watches the test never drives: B hears of no change.
    let (cluster, _) = ClusterFixture::scripted(&hub, ScriptedCache::with_manual_watch);
    let watches = [WatchStatus::default(), WatchStatus::default()];
    let feeds = [
        connect(&a, &hub, &watches[0]).await,
        connect(&b, &hub, &watches[1]).await,
    ];
    until("both watches are up", || {
        watches.iter().all(|watch| watch.get() == WatchState::Up)
    })
    .await;

    let ctx = provisioning_ctx(tenant).unwrap();
    let upstream = upstream_at(tenant, "api.example.com");
    let created =
        a.cp.create_upstream(&ctx, upstream.request.clone())
            .await
            .unwrap();
    let lookup = (tenant, "api.example.com", None);
    let storing = tokio::time::Instant::now();
    let before = b.cached(lookup).await;
    let stored = tokio::time::Instant::now();
    assert!(before.is_ok(), "{before:?}");
    let req = UpdateUpstreamRequest {
        enabled: false,
        ..unchanged(&created)
    };
    a.cp.update_upstream(&ctx, created.id, req).await.unwrap();
    let log = read_log(&facade(&hub).await).await.expect("A logged it");
    assert_eq!(log.seq(), 2, "A published its writes");

    tokio::time::pause();
    let at = |instant: tokio::time::Instant| {
        instant.saturating_duration_since(tokio::time::Instant::now())
    };
    tokio::time::advance(at(storing + TTL - Duration::from_millis(1))).await;
    assert_eq!(resolve(&b.cp, lookup).await, before, "not before the TTL");
    tokio::time::advance(at(stored + TTL)).await;
    tokio::time::resume();
    let after = resolve(&b.cp, lookup).await;
    assert!(is_disabled(&after), "after the TTL: {after:?}");
    assert_eq!(after, resolve(&b.uncached, lookup).await);

    for feed in feeds {
        feed.stop(CancellationToken::new()).await;
    }
    cluster.stop().await;
}

// ---------------------------------------------------------------------------
// The randomized model test
// ---------------------------------------------------------------------------

/// Writes per run on one replica: about 2 s.
const MODEL_STEPS: usize = 300;

/// Writes per run on two replicas, each waiting for its change to reach
/// the other: about 2 s.
#[cfg(feature = "cluster")]
const MODEL_STEPS_REPLICAS: usize = 150;

/// The aliases of the model's upstreams; a tenant holds each at most once.
const MODEL_ALIASES: [&str; 2] = ["a", "b"];

/// What each lookup asks for: the alias alone, and a route at either path
/// the model's routes take.
const MODEL_TARGETS: [Option<(&str, &str)>; 3] =
    [None, Some(("POST", "/v1")), Some(("POST", "/v2"))];

/// The write path that manages a row. Only the registry provisioner renames
/// an upstream or moves a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Api,
    Registry,
}

/// An upstream the model wrote.
#[derive(Debug, Clone, Copy)]
struct ModelUpstream {
    tenant: Uuid,
    id: Uuid,
    alias: &'static str,
    owner: Owner,
}

/// A route the model wrote; its tenant and owner are its upstream's.
#[derive(Debug, Clone, Copy)]
struct ModelRoute {
    tenant: Uuid,
    id: Uuid,
    upstream: Uuid,
    owner: Owner,
}

/// Random writes over a three-level tenant hierarchy, each picked from the
/// rows written so far, and the check that every lookup through the cache
/// answers as without it. The seed comes from `OAGW_MODEL_SEED` when set,
/// else at random; a failure prints it with the writes done.
struct Model {
    seed: u64,
    rng: rand::rngs::StdRng,
    /// Root first.
    tenants: [Uuid; 3],
    upstreams: Vec<ModelUpstream>,
    routes: Vec<ModelRoute>,
    /// Deleted upstreams, whose IDs a create in the same tenant may take
    /// again, as a registry instance removed and added back does.
    deleted: Vec<ModelUpstream>,
    done: Vec<String>,
}

impl Model {
    fn new(tenants: [Uuid; 3]) -> Self {
        use rand::SeedableRng;

        let seed = std::env::var("OAGW_MODEL_SEED").map_or_else(
            |_| rand::random(),
            |seed| seed.parse().expect("OAGW_MODEL_SEED is a u64"),
        );
        eprintln!("model seed {seed}");
        Self {
            seed,
            rng: rand::rngs::StdRng::seed_from_u64(seed),
            tenants,
            upstreams: Vec::new(),
            routes: Vec::new(),
            deleted: Vec::new(),
            done: Vec::new(),
        }
    }

    /// Every tenant's lookup of every alias and target.
    fn lookups(&self) -> Vec<Lookup<'static>> {
        let mut lookups = Vec::new();
        for tenant in self.tenants {
            for alias in MODEL_ALIASES {
                for target in MODEL_TARGETS {
                    lookups.push((tenant, alias, target));
                }
            }
        }
        lookups
    }

    /// Why the model fails: what differs, the seed and the writes done.
    fn failure(&self, what: &str) -> String {
        format!(
            "{what}\nseed {} (replay with OAGW_MODEL_SEED={}), after these writes:\n{}",
            self.seed,
            self.seed,
            self.done.join("\n")
        )
    }

    /// The first lookup that `planes` answers differently through its cache
    /// than without it.
    async fn disagreement(&self, planes: &CachedPlanes) -> Option<String> {
        for lookup in self.lookups() {
            let cached = resolve(&planes.cp, lookup).await;
            let uncached = resolve(&planes.uncached, lookup).await;
            if cached != uncached {
                return Some(format!(
                    "{lookup:?} through the cache: {cached:?}\nwithout it: {uncached:?}"
                ));
            }
        }
        None
    }

    /// Every lookup through `planes`' cache answers as without it.
    async fn check(&self, planes: &CachedPlanes) {
        if let Some(what) = self.disagreement(planes).await {
            panic!("{}", self.failure(&what));
        }
    }

    /// [`check`](Self::check) once `planes` has heard of the last write,
    /// within 5 s.
    #[cfg(feature = "cluster")]
    async fn check_delivered(&self, planes: &CachedPlanes) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while let Some(what) = self.disagreement(planes).await {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{}",
                self.failure(&format!("not delivered within 5 s: {what}"))
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Do one random write through `planes`.
    async fn write(&mut self, planes: &CachedPlanes) {
        use rand::RngExt;

        loop {
            let done = match self.rng.random_range(0..100) {
                0..20 => self.create_upstream(planes).await,
                20..45 => self.update_upstream(planes).await,
                45..55 => self.delete_upstream(planes).await,
                55..75 => self.create_route(planes).await,
                75..85 => self.update_route(planes).await,
                85..93 => self.move_route(planes).await,
                _ => self.delete_route(planes).await,
            };
            if let Some(done) = done {
                self.done.push(done);
                return;
            }
        }
    }

    /// The written value, or a failure naming `what`.
    fn ok<T, E: std::fmt::Debug>(&self, what: &str, result: Result<T, E>) -> T {
        result.unwrap_or_else(|e| panic!("{}", self.failure(&format!("{what}: {e:?}"))))
    }

    /// Whether a route write landed; it may conflict with an enabled route
    /// on the same upstream, path, priority and method (a failed write is a
    /// write the hook sees too). Anything else fails the model.
    fn landed<T>(&self, what: &mut String, result: Result<T, DomainError>) -> bool {
        match result {
            Ok(_) => true,
            Err(DomainError::Conflict { .. }) => {
                what.push_str(" (conflict)");
                false
            }
            Err(e) => panic!("{}", self.failure(&format!("{what}: {e:?}"))),
        }
    }

    /// An endpoint at one of two hosts and ports.
    fn server(&mut self) -> crate::domain::model::Server {
        use crate::domain::model::{Endpoint, Scheme, Server};
        use rand::RngExt;

        Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: format!("10.0.0.{}", self.rng.random_range(1..=2)),
                port: if self.rng.random_bool(0.5) { 443 } else { 8443 },
            }],
        }
    }

    /// An alias `tenant` holds no upstream with.
    fn free_alias(&mut self, tenant: Uuid) -> Option<&'static str> {
        use rand::seq::IndexedRandom;

        let free: Vec<&'static str> = MODEL_ALIASES
            .into_iter()
            .filter(|alias| {
                !self
                    .upstreams
                    .iter()
                    .any(|u| u.tenant == tenant && u.alias == *alias)
            })
            .collect();
        free.choose(&mut self.rng).copied()
    }

    async fn create_upstream(&mut self, planes: &CachedPlanes) -> Option<String> {
        use crate::domain::model::CreateUpstreamRequest;
        use rand::RngExt;
        use rand::seq::IndexedRandom;

        let tenant = *self.tenants.choose(&mut self.rng)?;
        let alias = self.free_alias(tenant)?;
        let again: Vec<usize> = (0..self.deleted.len())
            .filter(|&i| self.deleted[i].tenant == tenant)
            .collect();
        let id = match again.choose(&mut self.rng) {
            Some(&i) if self.rng.random_bool(0.8) => self.deleted.swap_remove(i).id,
            _ => Uuid::new_v4(),
        };
        let owner = if self.rng.random_bool(0.5) {
            Owner::Api
        } else {
            Owner::Registry
        };
        let req = CreateUpstreamRequest {
            id: Some(id),
            server: self.server(),
            protocol: oagw_sdk::HTTP_PROTOCOL_ID.into(),
            alias: Some(alias.into()),
            auth: self.rng.random_bool(0.5).then(shared_auth).flatten(),
            headers: None,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            enabled: self.rng.random_bool(0.8),
        };
        let done = format!("create upstream {id} {alias:?} in {tenant} by {owner:?}: {req:?}");
        let ctx = provisioning_ctx(tenant).unwrap();
        let created = match owner {
            Owner::Api => planes.cp.create_upstream(&ctx, req).await,
            Owner::Registry => planes.provisioner.create_registry_upstream(&ctx, req).await,
        };
        self.ok(&done, created);
        self.upstreams.push(ModelUpstream {
            tenant,
            id,
            alias,
            owner,
        });
        Some(done)
    }

    async fn update_upstream(&mut self, planes: &CachedPlanes) -> Option<String> {
        use rand::RngExt;
        use rand::seq::IndexedRandom;

        let i = (0..self.upstreams.len())
            .collect::<Vec<_>>()
            .choose(&mut self.rng)
            .copied()?;
        let upstream = self.upstreams[i];
        let ctx = provisioning_ctx(upstream.tenant).unwrap();
        let current = self.ok(
            "read upstream",
            planes.uncached.get_upstream(&ctx, upstream.id).await,
        );
        let mut req = unchanged(&current);
        // Renames favoured: only registry upstreams take them.
        let rename = upstream.owner == Owner::Registry && self.rng.random_bool(0.5);
        let what = match if rename {
            3
        } else {
            self.rng.random_range(0..3)
        } {
            0 => {
                req.enabled = !req.enabled;
                "enabled"
            }
            1 => {
                req.auth = if req.auth.is_some() {
                    None
                } else {
                    shared_auth()
                };
                "sharing"
            }
            2 => {
                req.server = self.server();
                "endpoint"
            }
            _ => {
                let alias = self.free_alias(upstream.tenant)?;
                req.alias = Some(alias.into());
                self.upstreams[i].alias = alias;
                "alias"
            }
        };
        let done = format!(
            "update upstream {} ({what}) by {:?}: {req:?}",
            upstream.id, upstream.owner
        );
        let updated = match upstream.owner {
            Owner::Api => planes.cp.update_upstream(&ctx, upstream.id, req).await,
            Owner::Registry => {
                planes
                    .provisioner
                    .update_registry_upstream(&ctx, upstream.id, req)
                    .await
            }
        };
        self.ok(&done, updated);
        Some(done)
    }

    async fn delete_upstream(&mut self, planes: &CachedPlanes) -> Option<String> {
        use rand::seq::IndexedRandom;

        let i = (0..self.upstreams.len())
            .collect::<Vec<_>>()
            .choose(&mut self.rng)
            .copied()?;
        let upstream = self.upstreams[i];
        let ctx = provisioning_ctx(upstream.tenant).unwrap();
        let done = format!("delete upstream {} by {:?}", upstream.id, upstream.owner);
        let deleted = match upstream.owner {
            Owner::Api => {
                self.ok(&done, planes.cp.delete_upstream(&ctx, upstream.id).await);
                true
            }
            Owner::Registry => {
                let outcome = self.ok(
                    &done,
                    planes
                        .provisioner
                        .delete_registry_upstream_if_unused(&ctx, upstream.id)
                        .await,
                );
                outcome == DeleteOutcome::Deleted
            }
        };
        if deleted {
            self.upstreams.swap_remove(i);
            self.routes.retain(|r| r.upstream != upstream.id);
            self.deleted.push(upstream);
        }
        Some(done)
    }

    /// A route request on `upstream` at a random path and priority.
    fn route_request(&mut self, upstream: Uuid) -> crate::domain::model::CreateRouteRequest {
        use crate::domain::model::{
            CreateRouteRequest, HttpMatch, HttpMethod, MatchRules, PathSuffixMode,
        };
        use rand::RngExt;

        CreateRouteRequest {
            id: Some(Uuid::new_v4()),
            upstream_id: upstream,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    methods: vec![HttpMethod::Post],
                    path: if self.rng.random_bool(0.5) {
                        "/v1"
                    } else {
                        "/v2"
                    }
                    .into(),
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::Append,
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: self.rng.random_range(0..3),
            enabled: self.rng.random_bool(0.8),
        }
    }

    async fn create_route(&mut self, planes: &CachedPlanes) -> Option<String> {
        use crate::domain::services::UpstreamCheck;
        use rand::seq::IndexedRandom;

        if self.routes.len() >= 12 {
            return None;
        }
        let upstream = *self.upstreams.choose(&mut self.rng)?;
        let req = self.route_request(upstream.id);
        let id = req.id.unwrap();
        let mut done = format!(
            "create route {id} on {} by {:?}: {req:?}",
            upstream.id, upstream.owner
        );
        let ctx = provisioning_ctx(upstream.tenant).unwrap();
        let created = match upstream.owner {
            Owner::Api => planes.cp.create_route(&ctx, req).await,
            Owner::Registry => {
                planes
                    .provisioner
                    .create_registry_route(&ctx, req, UpstreamCheck::Read)
                    .await
            }
        };
        if self.landed(&mut done, created) {
            self.routes.push(ModelRoute {
                tenant: upstream.tenant,
                id,
                upstream: upstream.id,
                owner: upstream.owner,
            });
        }
        Some(done)
    }

    /// The full replacement that keeps route `route` as it is.
    async fn unchanged_route(
        &self,
        planes: &CachedPlanes,
        route: ModelRoute,
    ) -> crate::domain::model::UpdateRouteRequest {
        let ctx = provisioning_ctx(route.tenant).unwrap();
        let current = self.ok(
            "read route",
            planes.uncached.get_route(&ctx, route.id).await,
        );
        crate::domain::model::UpdateRouteRequest {
            match_rules: current.match_rules,
            plugins: current.plugins,
            rate_limit: current.rate_limit,
            cors: current.cors,
            tags: current.tags,
            priority: current.priority,
            enabled: current.enabled,
        }
    }

    /// Replace route `route` with `req` on `upstream`; whether it landed.
    async fn replace_route(
        &self,
        planes: &CachedPlanes,
        route: ModelRoute,
        upstream: Uuid,
        req: crate::domain::model::UpdateRouteRequest,
        done: &mut String,
    ) -> bool {
        use crate::domain::services::UpstreamCheck;

        let ctx = provisioning_ctx(route.tenant).unwrap();
        let updated = match route.owner {
            Owner::Api => planes.cp.update_route(&ctx, route.id, req).await,
            Owner::Registry => {
                planes
                    .provisioner
                    .update_registry_route(&ctx, route.id, upstream, req, UpstreamCheck::Read)
                    .await
            }
        };
        self.landed(done, updated)
    }

    async fn update_route(&mut self, planes: &CachedPlanes) -> Option<String> {
        use rand::RngExt;
        use rand::seq::IndexedRandom;

        let route = *self.routes.choose(&mut self.rng)?;
        let mut req = self.unchanged_route(planes, route).await;
        let what = match self.rng.random_range(0..3) {
            0 => {
                let http = req.match_rules.http.as_mut().unwrap();
                http.path = if http.path == "/v1" { "/v2" } else { "/v1" }.into();
                "path"
            }
            1 => {
                req.priority = self.rng.random_range(0..3);
                "priority"
            }
            _ => {
                req.enabled = !req.enabled;
                "enabled"
            }
        };
        let mut done = format!(
            "update route {} ({what}) by {:?}: {req:?}",
            route.id, route.owner
        );
        self.replace_route(planes, route, route.upstream, req, &mut done)
            .await;
        Some(done)
    }

    async fn move_route(&mut self, planes: &CachedPlanes) -> Option<String> {
        use rand::seq::IndexedRandom;

        let movable: Vec<usize> = (0..self.routes.len())
            .filter(|&i| self.routes[i].owner == Owner::Registry)
            .collect();
        let i = *movable.choose(&mut self.rng)?;
        let route = self.routes[i];
        let targets: Vec<Uuid> = self
            .upstreams
            .iter()
            .filter(|u| {
                u.owner == Owner::Registry && u.tenant == route.tenant && u.id != route.upstream
            })
            .map(|u| u.id)
            .collect();
        let to = *targets.choose(&mut self.rng)?;
        let req = self.unchanged_route(planes, route).await;
        let mut done = format!("move route {} from {} to {to}", route.id, route.upstream);
        if self.replace_route(planes, route, to, req, &mut done).await {
            self.routes[i].upstream = to;
        }
        Some(done)
    }

    async fn delete_route(&mut self, planes: &CachedPlanes) -> Option<String> {
        use rand::seq::IndexedRandom;

        let i = (0..self.routes.len())
            .collect::<Vec<_>>()
            .choose(&mut self.rng)
            .copied()?;
        let route = self.routes[i];
        let ctx = provisioning_ctx(route.tenant).unwrap();
        let done = format!("delete route {} by {:?}", route.id, route.owner);
        let deleted = match route.owner {
            Owner::Api => planes.cp.delete_route(&ctx, route.id).await,
            Owner::Registry => {
                planes
                    .provisioner
                    .delete_registry_route(&ctx, route.id)
                    .await
            }
        };
        self.ok(&done, deleted);
        self.routes.swap_remove(i);
        Some(done)
    }
}

/// Three tenants, root first, each the parent of the next.
fn model_tenants() -> ([Uuid; 3], Arc<dyn TenantResolverClient>) {
    let tenants: [Uuid; 3] = std::array::from_fn(|_| Uuid::new_v4());
    let resolver = MockTenantResolverClient::with_hierarchy(tenants.map(TenantId).to_vec());
    (tenants, Arc::new(resolver))
}

/// Random upstream and route writes over a three-level hierarchy, through the
/// Management API and the registry provisioner: after each, every tenant's
/// lookup of every alias and target answers through the cache as without
/// it.
///
/// ADR-0020: CACHE-EVICT-007.
#[tokio::test]
async fn after_random_writes_every_cached_lookup_answers_as_uncached() {
    let (tenants, resolver) = model_tenants();
    let (db, recorder) = recorded_sqlite().await;
    let f = CachedPlanes::over(Stores::new(Backend::Db(db)), recorder, resolver);
    let mut model = Model::new(tenants);
    model.check(&f).await;
    for _ in 0..MODEL_STEPS {
        model.write(&f).await;
        model.check(&f).await;
    }
}

/// [`after_random_writes_every_cached_lookup_answers_as_uncached`] on two
/// replicas over one database and one cluster: each write goes to either,
/// and both answer as without the cache, the other once the change is
/// delivered.
///
/// ADR-0020: CACHE-EVICT-007.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn after_random_writes_on_two_replicas_every_cached_lookup_answers_as_uncached() {
    use rand::RngExt;

    let (tenants, resolver) = model_tenants();
    let (db, recorder) = recorded_sqlite().await;
    let stores = Stores::new(Backend::Db(db));
    let replicas = [
        CachedPlanes::over(stores.clone(), recorder.clone(), resolver.clone()),
        CachedPlanes::over(stores, recorder, resolver),
    ];
    let hub = Arc::new(ClientHub::new());
    let cluster = ClusterFixture::standalone(&hub);
    let watches = [WatchStatus::default(), WatchStatus::default()];
    let feeds = [
        connect(&replicas[0], &hub, &watches[0]).await,
        connect(&replicas[1], &hub, &watches[1]).await,
    ];
    until("both watches are up", || {
        watches.iter().all(|watch| watch.get() == WatchState::Up)
    })
    .await;

    let mut model = Model::new(tenants);
    for replica in &replicas {
        model.check(replica).await;
    }
    for _ in 0..MODEL_STEPS_REPLICAS {
        let writer = model.rng.random_range(0..2);
        model.write(&replicas[writer]).await;
        model.check(&replicas[writer]).await;
        model.check_delivered(&replicas[1 - writer]).await;
    }

    for feed in feeds {
        feed.stop(CancellationToken::new()).await;
    }
    cluster.stop().await;
}
