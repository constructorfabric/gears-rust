//! Each decorated write evicts the entries of the change its arguments
//! make, whether it succeeds or fails; reads evict nothing (ADR-0020).

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future;
use uuid::Uuid;

#[cfg(feature = "cluster")]
use tokio_util::sync::CancellationToken;
#[cfg(feature = "cluster")]
use toolkit::client_hub::ClientHub;
#[cfg(feature = "cluster")]
use tracing_test::traced_test;

use super::{FlushingRegistryRowRepo, FlushingRouteRepo, FlushingUpstreamRepo, WriteHook};
#[cfg(feature = "cluster")]
use super::{LATE_COMMIT_WINDOW, PUBLISH_TIMEOUT, route_written, upstream_written};
use crate::domain::gts_helpers::HTTP_PROTOCOL_ID;
use crate::domain::model::{
    Endpoint, HttpMatch, HttpMethod, ListQuery, ManagedBy, MatchRules, PathSuffixMode, Route,
    Scheme, Server, Upstream,
};
#[cfg(feature = "cluster")]
use crate::domain::repo::one_wait_at_the_end;
use crate::domain::repo::{
    DeleteOutcome, RegistryRowRepository, RepositoryError, RouteRepository, RowKey, Tags,
    UpstreamRepository,
};
#[cfg(feature = "cluster")]
use crate::infra::proxy_cache::publisher::Publisher;
use crate::infra::proxy_cache::testing::CacheMetrics;
#[cfg(feature = "cluster")]
use crate::infra::proxy_cache::testing::{
    ClusterFixture, connection_lost, facade, publisher_recording, read_log, resolver, until,
};
use crate::infra::proxy_cache::{AliasKey, ProxyConfigCache};
use crate::infra::storage::{InMemoryRegistryRowRepo, InMemoryRouteRepo, InMemoryUpstreamRepo};

fn upstream(tenant_id: Uuid, alias: &str) -> Upstream {
    Upstream {
        id: Uuid::new_v4(),
        tenant_id,
        alias: alias.into(),
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: "api.example.com".into(),
                port: 443,
            }],
        },
        protocol: HTTP_PROTOCOL_ID.into(),
        enabled: true,
        auth: None,
        headers: None,
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
        managed_by: ManagedBy::Registry,
    }
}

fn route(upstream: &Upstream, path: &str) -> Route {
    Route {
        id: Uuid::new_v4(),
        tenant_id: upstream.tenant_id,
        upstream_id: upstream.id,
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
        managed_by: ManagedBy::Registry,
    }
}

fn row_key(upstream: &Upstream) -> RowKey {
    RowKey {
        tenant_id: upstream.tenant_id,
        id: upstream.id,
    }
}

/// A partner `p` with a customer `c`, and an unrelated tenant `s`. The
/// store holds the partner's upstreams `u` (alias `a`, route `r`) and `v`
/// (alias `v`, route `r2`). The cache holds, by name:
///
/// - `hold`: `a` in `c`'s chain, holding `u`; `hold_v`: `v` there, holding `v`;
/// - `join`: `b` in `c`'s chain, not found; `other`: `b` in `s`'s chain;
/// - `routes_u`, `routes_v`: the routes of `u` and `v`.
struct Fixture {
    p: Uuid,
    u: Upstream,
    v: Upstream,
    r: Route,
    r2: Route,
    cache: Arc<ProxyConfigCache>,
    metrics: Arc<CacheMetrics>,
    upstreams: FlushingUpstreamRepo,
    routes: FlushingRouteRepo,
    rows: FlushingRegistryRowRepo,
    /// The store's routes, undecorated.
    store_routes: Arc<InMemoryRouteRepo>,
    aliases: Vec<(&'static str, AliasKey)>,
    route_keys: Vec<(&'static str, RowKey)>,
}

impl Fixture {
    async fn new() -> Self {
        let [p, c, s] = std::array::from_fn(|_| Uuid::new_v4());
        let (u, v) = (upstream(p, "a"), upstream(p, "v"));
        let (r, r2) = (route(&u, "/r"), route(&v, "/r2"));
        let store_upstreams = Arc::new(InMemoryUpstreamRepo::new());
        let store_routes = Arc::new(InMemoryRouteRepo::new());
        for upstream in [&u, &v] {
            store_upstreams.create(upstream.clone()).await.unwrap();
        }
        for route in [&r, &r2] {
            store_routes.create(route.clone()).await.unwrap();
        }

        let metrics = Arc::new(CacheMetrics::default());
        let cache = Arc::new(ProxyConfigCache::new(
            Duration::from_secs(60),
            100,
            metrics.clone(),
        ));
        let alias = |alias: &str, chain: Vec<Uuid>| AliasKey {
            alias: alias.into(),
            chain,
        };
        let aliases = vec![
            ("hold", alias("a", vec![c, p]), vec![u.clone()]),
            ("hold_v", alias("v", vec![c, p]), vec![v.clone()]),
            ("join", alias("b", vec![c, p]), vec![]),
            ("other", alias("b", vec![s]), vec![]),
        ];
        for (_, key, value) in &aliases {
            let value = value.clone();
            cache
                .aliases
                .get_or_load(key.clone(), move |_| future::ready(Ok(value)).boxed())
                .await
                .unwrap();
        }
        let route_keys = vec![
            ("routes_u", row_key(&u), vec![r.clone()]),
            ("routes_v", row_key(&v), vec![r2.clone()]),
        ];
        for (_, key, value) in &route_keys {
            let value = value.clone();
            cache
                .routes
                .get_or_load(*key, move |_| future::ready(Ok(value)).boxed())
                .await
                .unwrap();
        }

        let hook = WriteHook::new(Arc::clone(&cache));
        Self {
            p,
            cache,
            metrics,
            upstreams: FlushingUpstreamRepo::new(store_upstreams.clone(), hook.clone()),
            routes: FlushingRouteRepo::new(store_routes.clone(), hook.clone()),
            rows: FlushingRegistryRowRepo::new(
                Arc::new(InMemoryRegistryRowRepo::new(
                    store_upstreams,
                    store_routes.clone(),
                )),
                hook,
            ),
            store_routes,
            aliases: aliases.into_iter().map(|(n, k, _)| (n, k)).collect(),
            route_keys: route_keys.into_iter().map(|(n, k, _)| (n, k)).collect(),
            u,
            v,
            r,
            r2,
        }
    }

    /// The names of the seeded entries no longer cached.
    fn evicted(&self) -> BTreeSet<&'static str> {
        self.cache.aliases.assert_consistent();
        self.cache.routes.assert_consistent();
        let aliases = self
            .aliases
            .iter()
            .filter(|(_, key)| !self.cache.aliases.contains(key))
            .map(|(name, _)| *name);
        let routes = self
            .route_keys
            .iter()
            .filter(|(_, key)| !self.cache.routes.contains(key))
            .map(|(name, _)| *name);
        aliases.chain(routes).collect()
    }
}

/// What `write` evicts from a fresh fixture.
async fn evicted_by(write: impl AsyncFnOnce(&Fixture)) -> BTreeSet<&'static str> {
    let f = Fixture::new().await;
    assert!(f.evicted().is_empty(), "the fixture is cached");
    write(&f).await;
    f.evicted()
}

fn names<const N: usize>(names: [&'static str; N]) -> BTreeSet<&'static str> {
    names.into()
}

/// Upstream create and update evict the entries holding the upstream and
/// those of its written alias in a chain holding its tenant; a delete
/// evicts the entries holding it and its routes.
///
/// ADR-0020: CACHE-FRESH-002.
#[tokio::test]
async fn upstream_writes_evict_their_change() {
    let created = evicted_by(async |f: &Fixture| {
        f.upstreams.create(upstream(f.p, "b")).await.unwrap();
    })
    .await;
    assert_eq!(created, names(["join"]));

    let updated = evicted_by(async |f: &Fixture| {
        f.upstreams.update(f.u.clone()).await.unwrap();
    })
    .await;
    assert_eq!(updated, names(["hold"]));

    let renamed = evicted_by(async |f: &Fixture| {
        let renamed = Upstream {
            alias: "b".into(),
            ..f.u.clone()
        };
        f.upstreams.update(renamed).await.unwrap();
    })
    .await;
    assert_eq!(renamed, names(["hold", "join"]));

    let deleted = evicted_by(async |f: &Fixture| {
        f.upstreams.delete(f.p, f.u.id).await.unwrap();
    })
    .await;
    assert_eq!(deleted, names(["hold", "routes_u"]));
}

/// Route create and update evict the entry of the upstream they write to
/// and the entry holding the route (a move evicts both); a delete evicts
/// the entry holding the route, and a delete by upstream that upstream's.
///
/// ADR-0020: CACHE-FRESH-002.
#[tokio::test]
async fn route_writes_evict_their_change() {
    let created = evicted_by(async |f: &Fixture| {
        f.routes.create(route(&f.u, "/new")).await.unwrap();
    })
    .await;
    assert_eq!(created, names(["routes_u"]));

    let updated = evicted_by(async |f: &Fixture| {
        f.routes.update(f.r.clone()).await.unwrap();
    })
    .await;
    assert_eq!(updated, names(["routes_u"]));

    let moved = evicted_by(async |f: &Fixture| {
        let moved = Route {
            upstream_id: f.u.id,
            ..f.r2.clone()
        };
        f.routes.update(moved).await.unwrap();
    })
    .await;
    assert_eq!(moved, names(["routes_u", "routes_v"]));

    let deleted = evicted_by(async |f: &Fixture| {
        f.routes.delete(f.p, f.r.id).await.unwrap();
    })
    .await;
    assert_eq!(deleted, names(["routes_u"]));

    let deleted_by_upstream = evicted_by(async |f: &Fixture| {
        f.routes.delete_by_upstream(f.p, f.u.id).await.unwrap();
    })
    .await;
    assert_eq!(deleted_by_upstream, names(["routes_u"]));
}

/// The registry's delete evicts as an upstream delete, also when the
/// upstream is kept because a route still uses it.
///
/// ADR-0020: CACHE-FRESH-002, CACHE-EVICT-003.
#[tokio::test]
async fn the_registry_delete_evicts_as_an_upstream_delete() {
    let deleted = evicted_by(async |f: &Fixture| {
        f.store_routes.delete(f.p, f.r2.id).await.unwrap();
        let outcome = f.rows.delete_upstream_if_unused(f.p, f.v.id).await;
        assert_eq!(outcome.unwrap(), DeleteOutcome::Deleted);
    })
    .await;
    assert_eq!(deleted, names(["hold_v", "routes_v"]));

    let kept = evicted_by(async |f: &Fixture| {
        let outcome = f.rows.delete_upstream_if_unused(f.p, f.u.id).await;
        assert_eq!(outcome.unwrap(), DeleteOutcome::InUse);
    })
    .await;
    assert_eq!(kept, names(["hold", "routes_u"]));
}

/// A write that fails evicts as one that succeeds: it may have committed.
///
/// ADR-0020: CACHE-FRESH-002.
#[tokio::test]
async fn a_failed_write_evicts_its_change() {
    let failed = evicted_by(async |f: &Fixture| {
        let missing = upstream(f.p, "b");
        let result = f.upstreams.update(missing).await;
        assert!(
            matches!(result, Err(RepositoryError::NotFound { .. })),
            "{result:?}"
        );
    })
    .await;
    assert_eq!(failed, names(["join"]));

    let failed = evicted_by(async |f: &Fixture| {
        let overlapping = route(&f.u, "/r");
        let result = f.routes.create(overlapping).await;
        assert!(
            matches!(result, Err(RepositoryError::RouteOverlap(_))),
            "{result:?}"
        );
    })
    .await;
    assert_eq!(failed, names(["routes_u"]));
}

/// Reads pass through and evict nothing.
#[tokio::test]
async fn reads_evict_nothing() {
    let read = evicted_by(async |f: &Fixture| {
        let query = ListQuery::default();
        let chain: HashSet<Uuid> = [f.p].into();
        f.upstreams.get_by_id(f.p, f.u.id).await.unwrap();
        assert_eq!(f.upstreams.list(f.p, &query).await.unwrap().len(), 2);
        let found = f
            .upstreams
            .list_by_alias_for_tenants("a", &chain, Tags::Skip);
        assert_eq!(found.await.unwrap(), std::slice::from_ref(&f.u));

        f.routes.get_by_id(f.p, f.r.id).await.unwrap();
        assert_eq!(f.routes.list(f.p, None, &query).await.unwrap().len(), 2);
        let matched = f
            .routes
            .find_matching_in_tenants(&[f.p], &[f.u.id], "POST", "/r", Tags::Skip)
            .await;
        assert_eq!(matched.unwrap().id, f.r.id);
        let proxy = f.routes.list_proxy_routes(&[row_key(&f.u)]).await.unwrap();
        assert_eq!(proxy, [(row_key(&f.u), vec![f.r.clone()])]);

        assert_eq!(f.rows.list_upstreams().await.unwrap().len(), 2);
        assert_eq!(f.rows.list_routes().await.unwrap().len(), 2);
    })
    .await;
    assert!(read.is_empty(), "{read:?}");
}

// ---------------------------------------------------------------------------
// Publishing to the cluster change log
// ---------------------------------------------------------------------------

/// Publish `f`'s writes through a publisher on the cluster in `hub`, as
/// `origin`.
#[cfg(feature = "cluster")]
async fn publish_through(f: &Fixture, hub: &ClientHub, origin: Uuid) -> CancellationToken {
    let (publisher, stop) = publisher_recording(hub, origin, f.metrics.clone()).await;
    f.upstreams.hook.start_publishing(publisher);
    stop
}

/// A write returns once its change is in the change log, logged once; a
/// failed write is published too, since it may have committed.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn a_write_returns_once_its_change_is_logged() {
    let hub = Arc::new(ClientHub::new());
    let cluster = ClusterFixture::standalone(&hub);
    let f = Fixture::new().await;
    let me = Uuid::new_v4();
    let stop = publish_through(&f, &hub, me).await;

    f.upstreams.update(f.u.clone()).await.unwrap();
    let log = read_log(&facade(&hub).await).await.expect("logged");
    let logged: Vec<_> = log.changes().map(|l| (l.seq, l.origin, l.change)).collect();
    assert_eq!(logged, [(1, me, upstream_written(&f.u))]);

    let missing = upstream(f.p, "b");
    assert!(f.upstreams.update(missing.clone()).await.is_err());
    let log = read_log(&facade(&hub).await).await.unwrap();
    assert_eq!(log.seq(), 2);
    assert_eq!(
        log.changes().last().map(|l| l.change),
        Some(upstream_written(&missing))
    );

    stop.cancel();
    cluster.stop().await;
}

/// When the change cannot be appended, the write still succeeds, its
/// entries are evicted here, and the failure is logged and counted once.
///
/// ADR-0020: CACHE-FAIL-002.
#[cfg(feature = "cluster")]
#[tokio::test(start_paused = true)]
#[traced_test]
async fn when_the_append_fails_the_write_succeeds_and_evicts() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = ClusterFixture::scripted(&hub, |cache| cache);
    cache.fail_writes(Some(connection_lost()));
    let f = Fixture::new().await;
    let stop = publish_through(&f, &hub, Uuid::new_v4()).await;

    f.upstreams
        .update(f.u.clone())
        .await
        .expect("the write stands");
    assert_eq!(f.evicted(), names(["hold"]));
    assert!(logs_contain(
        "Could not append proxy configuration changes to the cluster change log"
    ));
    assert!(read_log(&facade(&hub).await).await.is_none());
    assert_eq!(f.metrics.count("publish_failures append_failed"), 1);
    assert_eq!(f.metrics.publish_failures(), 1, "counted once");

    stop.cancel();
    cluster.stop().await;
}

/// A write waits for the change log at most two seconds, then answers with
/// its own result, and logs the change it did not see published; the
/// change still goes out, and is not counted as a failure once it lands.
///
/// ADR-0020: CACHE-FAIL-002.
#[cfg(feature = "cluster")]
#[tokio::test(start_paused = true)]
#[traced_test]
async fn a_write_waits_for_the_log_at_most_two_seconds() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = ClusterFixture::scripted(&hub, |cache| cache);
    let f = Fixture::new().await;
    let stop = publish_through(&f, &hub, Uuid::new_v4()).await;

    cache.hold();
    let started = tokio::time::Instant::now();
    f.upstreams
        .update(f.u.clone())
        .await
        .expect("the write stands");
    assert_eq!(started.elapsed(), PUBLISH_TIMEOUT);
    assert_eq!(f.evicted(), names(["hold"]));
    assert!(logs_contain("was not published in time"));
    assert!(
        logs_contain(&upstream_written(&f.u).to_string()),
        "names the change"
    );
    assert_eq!(f.metrics.publish_failures(), 0, "the append may still land");

    cache.release();
    let log_store = facade(&hub).await;
    until("the change is logged", || cache.writes() == 1).await;
    assert_eq!(read_log(&log_store).await.unwrap().seq(), 1);
    assert_eq!(f.metrics.publish_failures(), 0, "the append landed");

    stop.cancel();
    cluster.stop().await;
}

/// A write whose append fails after the write stopped waiting is counted
/// once, by the publisher, not also for the wait.
///
/// ADR-0020: CACHE-FAIL-002.
#[cfg(feature = "cluster")]
#[tokio::test(start_paused = true)]
async fn a_late_failed_append_counts_once() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = ClusterFixture::scripted(&hub, |cache| cache);
    let f = Fixture::new().await;
    let stop = publish_through(&f, &hub, Uuid::new_v4()).await;

    cache.fail_writes(Some(connection_lost()));
    cache.hold();
    f.upstreams
        .update(f.u.clone())
        .await
        .expect("the write stands");
    assert_eq!(f.metrics.publish_failures(), 0);
    cache.release();
    until("the append failed", || f.metrics.publish_failures() > 0).await;
    tokio::time::sleep(PUBLISH_TIMEOUT).await;
    assert_eq!(f.metrics.count("publish_failures append_failed"), 1);
    assert_eq!(f.metrics.publish_failures(), 1);

    stop.cancel();
    cluster.stop().await;
}

/// A write whose future is dropped mid-call evicts here at once and hands
/// its change to the publisher without waiting for the append; it is
/// logged once.
///
/// ADR-0020: CACHE-FRESH-002.
#[cfg(feature = "cluster")]
#[tokio::test]
async fn a_dropped_write_hands_its_change_over_without_waiting() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = ClusterFixture::scripted(&hub, |cache| cache);
    let f = Fixture::new().await;
    let me = Uuid::new_v4();
    let stop = publish_through(&f, &hub, me).await;

    cache.hold();
    let change = upstream_written(&f.u);
    let mut write = Box::pin(f.upstreams.hook.write(change.clone(), || {
        future::pending::<Result<(), RepositoryError>>()
    }));
    assert!((&mut write).now_or_never().is_none(), "the call hangs");
    drop(write);
    assert_eq!(f.evicted(), names(["hold"]), "evicted at the drop");
    assert_eq!(f.metrics.count("changes local"), 1);

    cache.release();
    until("the change is logged", || cache.writes() == 1).await;
    let log = read_log(&facade(&hub).await).await.unwrap();
    let logged: Vec<_> = log.changes().map(|l| (l.seq, l.origin, l.change)).collect();
    assert_eq!(logged, [(1, me, change)]);

    stop.cancel();
    cluster.stop().await;
}

/// Each write applies one change here, whatever its outcome, and counts it
/// as local.
#[tokio::test]
async fn each_write_counts_one_local_change() {
    let f = Fixture::new().await;
    f.upstreams.update(f.u.clone()).await.unwrap();
    assert!(f.upstreams.update(upstream(f.p, "b")).await.is_err());
    f.routes.delete(f.r.tenant_id, f.r.id).await.unwrap();
    assert_eq!(f.metrics.count("changes local"), 3);
}

/// A write whose future was dropped may still commit after a load on some
/// replica read the old row. Its change is applied and published once more
/// after the late-commit window: the entry stored since is evicted again,
/// here and, through the change log, on the other replicas.
///
/// ADR-0020: CACHE-FRESH-002.
#[cfg(feature = "cluster")]
#[tokio::test(start_paused = true)]
async fn a_dropped_write_is_applied_and_published_again_after_a_late_commit() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = ClusterFixture::scripted(&hub, |cache| cache);
    let f = Fixture::new().await;
    let me = Uuid::new_v4();
    let stop = publish_through(&f, &hub, me).await;

    let change = upstream_written(&f.u);
    let mut write = Box::pin(f.upstreams.hook.write(change.clone(), || {
        future::pending::<Result<(), RepositoryError>>()
    }));
    assert!((&mut write).now_or_never().is_none(), "the call hangs");
    drop(write);
    assert_eq!(f.evicted(), names(["hold"]), "evicted at the drop");
    until("the change is logged", || cache.writes() == 1).await;

    // A load between the drop and the late commit stores the old row.
    let (_, hold) = f.aliases[0].clone();
    let old = vec![f.u.clone()];
    f.cache
        .aliases
        .get_or_load(hold, move |_| future::ready(Ok(old)).boxed())
        .await
        .unwrap();
    tokio::time::sleep(LATE_COMMIT_WINDOW - Duration::from_secs(1)).await;
    assert!(f.evicted().is_empty(), "not before the window ends");
    assert_eq!(cache.writes(), 1);

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(f.evicted(), names(["hold"]), "evicted again");
    assert_eq!(f.metrics.count("changes local"), 2);
    until("the change is logged again", || cache.writes() == 2).await;
    let log = read_log(&facade(&hub).await).await.unwrap();
    let logged: Vec<_> = log.changes().map(|l| (l.seq, l.origin, l.change)).collect();
    assert_eq!(logged, [(1, me, change.clone()), (2, me, change)]);

    stop.cancel();
    cluster.stop().await;
}

/// A write that completes is not applied again: its statement has
/// finished.
#[cfg(feature = "cluster")]
#[tokio::test(start_paused = true)]
async fn a_completed_write_is_applied_once() {
    let f = Fixture::new().await;
    f.upstreams.update(f.u.clone()).await.unwrap();
    assert!(f.upstreams.update(upstream(f.p, "b")).await.is_err());
    tokio::time::sleep(LATE_COMMIT_WINDOW * 2).await;
    assert_eq!(f.metrics.count("changes local"), 2);
}

/// Once the change feed has stopped, a write still evicts here, and its
/// unpublished change is counted and logged with its IDs once, also for a
/// dropped write (no re-apply after the late-commit window). Before the
/// feed starts, nothing is counted.
///
/// ADR-0020: CACHE-FAIL-002.
#[cfg(feature = "cluster")]
#[tokio::test(start_paused = true)]
#[traced_test]
async fn after_the_feed_stops_changes_are_counted_as_not_published() {
    let f = Fixture::new().await;
    f.upstreams.update(f.u.clone()).await.unwrap();
    assert_eq!(f.metrics.publish_failures(), 0, "no feed yet");

    f.upstreams.hook.stop_publishing();
    f.routes.update(f.r.clone()).await.unwrap();
    assert_eq!(f.evicted(), names(["hold", "routes_u"]));
    assert_eq!(f.metrics.count("publish_failures publisher_stopped"), 1);
    assert!(logs_contain("publisher has stopped"));
    assert!(logs_contain(&route_written(&f.r).to_string()));

    let dropped = upstream_written(&f.v);
    let mut write = Box::pin(f.upstreams.hook.write(dropped.clone(), || {
        future::pending::<Result<(), RepositoryError>>()
    }));
    assert!((&mut write).now_or_never().is_none(), "the call hangs");
    drop(write);
    assert_eq!(f.metrics.count("publish_failures publisher_stopped"), 2);
    assert!(logs_contain(&dropped.to_string()));
    let applied = f.metrics.count("changes local");

    tokio::time::sleep(LATE_COMMIT_WINDOW * 2).await;
    assert_eq!(f.metrics.count("publish_failures publisher_stopped"), 2);
    assert_eq!(
        f.metrics.count("changes local"),
        applied,
        "not applied again"
    );
}

/// A write dropped while the change feed runs is applied and published
/// again after the late-commit window only while the feed still runs: once
/// it stops, the pending re-apply ends without applying, publishing or
/// counting anything.
///
/// ADR-0020: CACHE-FRESH-002.
#[cfg(feature = "cluster")]
#[tokio::test(start_paused = true)]
async fn a_dropped_write_is_not_applied_again_once_the_feed_stops() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = ClusterFixture::scripted(&hub, |cache| cache);
    let f = Fixture::new().await;
    let stop = publish_through(&f, &hub, Uuid::new_v4()).await;

    let mut write = Box::pin(f.upstreams.hook.write(upstream_written(&f.u), || {
        future::pending::<Result<(), RepositoryError>>()
    }));
    assert!((&mut write).now_or_never().is_none(), "the call hangs");
    drop(write);
    until("the change is logged", || cache.writes() == 1).await;
    assert_eq!(f.metrics.count("changes local"), 1);

    f.upstreams.hook.stop_publishing();
    stop.cancel();
    tokio::time::sleep(LATE_COMMIT_WINDOW * 2).await;
    assert_eq!(f.metrics.count("changes local"), 1, "not applied again");
    assert_eq!(cache.writes(), 1, "not published again");
    assert_eq!(f.metrics.publish_failures(), 0, "not counted");

    cluster.stop().await;
}

/// A publisher whose task has ended refuses the change; the write counts
/// and logs it.
///
/// ADR-0020: CACHE-FAIL-002.
#[cfg(feature = "cluster")]
#[tokio::test]
#[traced_test]
async fn a_change_the_ended_publisher_refuses_is_counted() {
    use crate::infra::proxy_cache::WatchStatus;

    let hub = Arc::new(ClientHub::new());
    let cluster = ClusterFixture::standalone(&hub);
    let f = Fixture::new().await;
    let cancel = CancellationToken::new();
    let (publisher, task) = Publisher::spawn(
        facade(&hub).await,
        Uuid::new_v4(),
        f.metrics.clone(),
        WatchStatus::default(),
        cancel.clone(),
    );
    f.upstreams.hook.start_publishing(publisher);
    cancel.cancel();
    task.await.unwrap();

    f.upstreams.update(f.u.clone()).await.unwrap();
    assert_eq!(f.evicted(), names(["hold"]));
    assert_eq!(f.metrics.count("publish_failures publisher_stopped"), 1);
    assert!(logs_contain(&upstream_written(&f.u).to_string()));
    cluster.stop().await;
}

/// `ChangeFeed::stop` with its deadline passed aborts a publisher still
/// appending, which counts and logs the change it held. Cancellation alone
/// would wait out the append (five 1 s attempts on a hung cluster), so the
/// abort is what bounds `stop`.
///
/// ADR-0020: CACHE-FAIL-002.
#[cfg(feature = "cluster")]
#[tokio::test(start_paused = true)]
#[traced_test]
async fn stop_at_its_deadline_aborts_a_publisher_still_appending() {
    use crate::infra::proxy_cache::{ChangeFeed, WatchState, WatchStatus};

    let hub = Arc::new(ClientHub::new());
    let (cluster, scripted) = ClusterFixture::scripted(&hub, |cache| cache);
    let f = Fixture::new().await;
    let status = WatchStatus::default();
    let feed = ChangeFeed::start(
        resolver(&hub),
        &f.upstreams.hook,
        Uuid::new_v4(),
        &status,
        CancellationToken::new(),
    )
    .await
    .expect("the oagw profile");
    until("the watch is up", || status.get() == WatchState::Up).await;
    scripted.hold();
    let change = upstream_written(&f.u);
    assert!(f.upstreams.hook.submit(change.clone(), None), "submitted");
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(f.metrics.publish_failures(), 0, "the append is under way");

    let deadline = CancellationToken::new();
    deadline.cancel();
    tokio::time::timeout(Duration::from_secs(1), feed.stop(deadline))
        .await
        .expect("stop returns at its deadline");
    assert!(logs_contain("cut off at the shutdown deadline"));
    assert_eq!(f.metrics.count("publish_failures publisher_stopped"), 1);
    assert!(logs_contain(&change.to_string()));
    scripted.release();
    cluster.stop().await;
}

/// In [`one_wait_at_the_end`], writes return without waiting for the change
/// log; the scope waits once, at its end, at most two seconds, and the
/// changes queued meanwhile go out in one append. Outside of it a write
/// waits again.
///
/// ADR-0020: CACHE-LOG-003.
#[cfg(feature = "cluster")]
#[tokio::test(start_paused = true)]
async fn writes_in_one_wait_at_the_end_wait_once() {
    let hub = Arc::new(ClientHub::new());
    let (cluster, cache) = ClusterFixture::scripted(&hub, |cache| cache);
    let f = Fixture::new().await;
    let stop = publish_through(&f, &hub, Uuid::new_v4()).await;

    let started = tokio::time::Instant::now();
    let written = one_wait_at_the_end(async {
        f.upstreams.update(f.u.clone()).await.unwrap();
        f.upstreams.update(f.v.clone()).await.unwrap();
        f.routes.update(f.r.clone()).await.unwrap();
        started.elapsed()
    })
    .await;
    assert_eq!(written, Duration::ZERO, "no write waited");
    assert_eq!(cache.writes(), 1, "one append, awaited at the end");
    assert_eq!(read_log(&facade(&hub).await).await.unwrap().seq(), 3);

    cache.hold();
    let started = tokio::time::Instant::now();
    let written = one_wait_at_the_end(async {
        f.upstreams.update(f.u.clone()).await.unwrap();
        f.routes.update(f.r2.clone()).await.unwrap();
        started.elapsed()
    })
    .await;
    assert_eq!(written, Duration::ZERO, "no write waited");
    assert_eq!(started.elapsed(), PUBLISH_TIMEOUT, "the scope waited once");

    let started = tokio::time::Instant::now();
    f.upstreams.update(f.u.clone()).await.unwrap();
    assert_eq!(started.elapsed(), PUBLISH_TIMEOUT, "outside, a write waits");
    cache.release();
    until("every change is logged", || cache.writes() >= 2).await;

    stop.cancel();
    cluster.stop().await;
}
