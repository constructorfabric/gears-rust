//! Statement counts of the database repositories, recorded on `SQLite`
//! (ADR-0018 *Repository Behavior*): reads issue a fixed number of `SELECT`s
//! whatever the hierarchy depth or page size, management reads, creates and
//! updates each run in one transaction, proxy reads run in none, and an
//! upstream delete is one statement.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tenant_resolver_sdk::TenantId;
use toolkit_db::DBProvider;
use toolkit_db::test_support::{QueryKind, QueryRecorder};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::route_db_repo::DbRouteRepo;
use super::testing::recorded_sqlite;
use super::upstream_db_repo::DbUpstreamRepo;
use crate::domain::gts_helpers::{HTTP_PROTOCOL_ID, TIMEOUT_GUARD_PLUGIN_ID};
use crate::domain::model::{
    CreateUpstreamRequest, Endpoint, HttpMatch, HttpMethod, ListQuery, ManagedBy, MatchRules,
    PathSuffixMode, PluginBinding, PluginsConfig, Route, Scheme, Server, SharingMode,
    UpdateUpstreamRequest, Upstream,
};
use crate::domain::repo::{RepositoryError, RouteRepository, Tags, UpstreamRepository};
use crate::domain::services::ControlPlaneService;
use crate::domain::services::management::ControlPlaneServiceImpl;
use crate::domain::ssrf::SsrfGuard;
use crate::domain::test_support::{
    MockCredStoreClient, MockTenantResolverClient, allow_all_enforcer,
};

struct Repos {
    upstreams: Arc<DbUpstreamRepo>,
    routes: Arc<DbRouteRepo>,
    recorder: QueryRecorder,
}

async fn repos() -> Repos {
    let (db, recorder) = recorded_sqlite().await;
    let db = DBProvider::<RepositoryError>::new(db);
    Repos {
        upstreams: Arc::new(DbUpstreamRepo::new(db.clone())),
        routes: Arc::new(DbRouteRepo::new(db)),
        recorder,
    }
}

fn selects(recorder: &QueryRecorder) -> usize {
    recorder
        .events()
        .iter()
        .filter(|e| e.kind == QueryKind::Select)
        .count()
}

/// The `SELECT`s recorded since the last `clear`, then clear.
fn take_selects(recorder: &QueryRecorder) -> usize {
    let count = selects(recorder);
    recorder.clear();
    count
}

/// The `SELECT`s recorded since the last `clear`, all in one and the same
/// transaction, then clear.
#[track_caller]
fn take_snapshot_selects(recorder: &QueryRecorder, what: &str) -> usize {
    let count = selects(recorder);
    assert_one_transaction(recorder, what);
    count
}

/// Every recorded statement ran in one and the same transaction.
#[track_caller]
fn assert_one_transaction(recorder: &QueryRecorder, what: &str) {
    let events = recorder.events();
    assert!(!events.is_empty(), "{what}: no statements recorded");
    assert!(
        events.iter().all(|e| e.in_tx) && recorder.all_in_one_transaction(),
        "{what}: statements outside one transaction:\n{}",
        recorder.dump()
    );
    recorder.clear();
}

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
        plugins: Some(plugins()),
        rate_limit: None,
        cors: None,
        tags: vec!["a".into(), "b".into()],
        managed_by: ManagedBy::Api,
    }
}

fn plugins() -> PluginsConfig {
    PluginsConfig {
        sharing: SharingMode::Private,
        items: vec![PluginBinding {
            plugin_ref: TIMEOUT_GUARD_PLUGIN_ID.into(),
            config: HashMap::from([("timeout_ms".to_owned(), "500".to_owned())]),
        }],
    }
}

fn route(tenant_id: Uuid, upstream_id: Uuid, path: &str) -> Route {
    Route {
        id: Uuid::new_v4(),
        tenant_id,
        upstream_id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Get, HttpMethod::Post],
                path: path.into(),
                query_allowlist: vec![],
                path_suffix_mode: PathSuffixMode::Append,
            }),
            grpc: None,
        },
        plugins: Some(plugins()),
        rate_limit: None,
        cors: None,
        tags: vec!["t".into()],
        priority: 0,
        enabled: true,
        managed_by: ManagedBy::Api,
    }
}

/// The proxy's two repository calls for a tenant chain of `depth` levels, one
/// `api` upstream per level. The only route sits on the root's upstream, so
/// at depth > 1 it is found through the ancestor-upstream fallback.
/// Returns the `SELECT` count of the two calls.
async fn proxy_resolution_selects(depth: usize, tags: Tags) -> usize {
    let r = repos().await;
    let chain: Vec<Uuid> = (0..depth).map(|_| Uuid::new_v4()).collect();
    let mut upstream_ids = Vec::new();
    for tenant in &chain {
        upstream_ids.push(
            r.upstreams
                .create(upstream(*tenant, "api"))
                .await
                .unwrap()
                .id,
        );
    }
    let root = chain.len() - 1;
    let mut expected = r
        .routes
        .create(route(chain[root], upstream_ids[root], "/v1"))
        .await
        .unwrap();
    // A non-matching route on the closest upstream: still a candidate.
    r.routes
        .create(route(chain[0], upstream_ids[0], "/v2"))
        .await
        .unwrap();
    r.recorder.clear();

    let tenants: HashSet<Uuid> = chain.iter().copied().collect();
    let found = r
        .upstreams
        .list_by_alias_for_tenants("api", &tenants, tags)
        .await
        .unwrap();
    assert_eq!(found.len(), depth);
    assert!(found.iter().all(|u| u.plugins == Some(plugins())));
    // The bindings join on the composite key: its tenant keeps them in the
    // scoped upstream's tenant.
    let alias_lookup = &r.recorder.events()[0].sql;
    assert!(
        alias_lookup
            .contains(r#""oagw_upstream_plugin"."tenant_id" = "oagw_upstream"."tenant_id""#),
        "{}",
        r.recorder.dump()
    );
    let matched = r
        .routes
        .find_matching_in_tenants(&chain, &upstream_ids, "POST", "/v1/chat", tags)
        .await
        .unwrap();
    if tags == Tags::Skip {
        expected.tags.clear();
    }
    assert_eq!(matched, expected);
    // The proxy path reads without a transaction (ADR-0018 *Transactions*).
    assert!(
        r.recorder.events().iter().all(|e| !e.in_tx),
        "{}",
        r.recorder.dump()
    );
    take_selects(&r.recorder)
}

/// Without tags: the alias lookup with its plugins joined in, the candidate
/// join, and the winner's plugins.
#[tokio::test]
async fn proxy_resolution_is_three_selects_at_any_depth() {
    let depth_1 = proxy_resolution_selects(1, Tags::Skip).await;
    let depth_5 = proxy_resolution_selects(5, Tags::Skip).await;
    assert_eq!((depth_1, depth_5), (3, 3));
}

/// With tags (the SDK's `resolve_proxy_target`): two more, one per tag table.
#[tokio::test]
async fn resolution_with_tags_is_five_selects_at_any_depth() {
    let depth_1 = proxy_resolution_selects(1, Tags::Load).await;
    let depth_5 = proxy_resolution_selects(5, Tags::Load).await;
    assert_eq!((depth_1, depth_5), (5, 5));
}

/// `resolve_proxy_target` through the Control Plane for one tenant: its
/// `SELECT` count, its tag-table reads, and the effective upstream's and the
/// route's tags.
async fn service_resolution(tags: Tags) -> (usize, usize, Vec<String>, Vec<String>) {
    let r = repos().await;
    let tenant = Uuid::new_v4();
    let up = r.upstreams.create(upstream(tenant, "api")).await.unwrap();
    r.routes.create(route(tenant, up.id, "/v1")).await.unwrap();
    let cp = ControlPlaneServiceImpl::new(
        r.upstreams.clone(),
        r.routes.clone(),
        Arc::new(MockTenantResolverClient::with_hierarchy(vec![TenantId(
            tenant,
        )])),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::empty()),
        Arc::new(SsrfGuard::disabled()),
    );
    let ctx = SecurityContext::builder()
        .subject_tenant_id(tenant)
        .subject_id(Uuid::new_v4())
        .build()
        .expect("security context");
    r.recorder.clear();

    let (effective, route) = cp
        .resolve_proxy_target(&ctx, "api", "POST", "/v1/chat", tags)
        .await
        .unwrap();
    let tag_reads = r
        .recorder
        .events()
        .iter()
        .filter(|e| {
            matches!(
                e.table.as_deref(),
                Some("oagw_upstream_tag" | "oagw_route_tag")
            )
        })
        .count();
    (
        take_selects(&r.recorder),
        tag_reads,
        effective.tags,
        route.tags,
    )
}

/// The proxy's `Tags::Skip` reaches both repository reads through the
/// Control Plane: 3 `SELECT`s and no tags; with tags, both tag tables too.
#[tokio::test]
async fn control_plane_resolution_skips_tags_only_when_asked() {
    let (selects, tag_reads, upstream_tags, route_tags) = service_resolution(Tags::Skip).await;
    assert_eq!((selects, tag_reads), (3, 0));
    assert!(upstream_tags.is_empty() && route_tags.is_empty());

    let (selects, tag_reads, upstream_tags, route_tags) = service_resolution(Tags::Load).await;
    assert_eq!((selects, tag_reads), (5, 2));
    assert_eq!(route_tags, vec!["t".to_owned()]);
    for tag in ["a", "b", "t"] {
        assert!(
            upstream_tags.iter().any(|t| t == tag),
            "{tag} in {upstream_tags:?}"
        );
    }
}

/// Routes with several methods: the candidate load is one join of route, HTTP
/// match and methods, filtered on the request method and selecting no JSON
/// column; one more join loads the winner in full with its methods and
/// plugins, and its tags follow when loaded.
async fn find_matching_selects(tags: Tags) -> usize {
    let r = repos().await;
    let tenant = Uuid::new_v4();
    let up = r.upstreams.create(upstream(tenant, "api")).await.unwrap();
    for i in 0..5 {
        r.routes
            .create(route(tenant, up.id, &format!("/v{i}")))
            .await
            .unwrap();
    }
    r.recorder.clear();

    r.routes
        .find_matching_in_tenants(&[tenant], &[up.id], "GET", "/v3/x", tags)
        .await
        .unwrap();
    let trace = r.recorder.events();
    let candidates = &trace[0].sql;
    assert!(
        candidates.contains(r#"JOIN "oagw_route_http_match""#)
            && candidates.contains(r#"JOIN "oagw_route_method""#)
            && candidates.contains(r#""oagw_route_method"."method" = "#)
            && !candidates.contains("match_config")
            && !candidates.contains("rate_limit"),
        "{}",
        r.recorder.dump()
    );
    // Every child joins on its composite key: its tenant keeps the child in
    // the scoped route's tenant.
    for child in ["oagw_route_http_match", "oagw_route_method"] {
        let key = format!(r#""{child}"."tenant_id" = "oagw_route"."tenant_id""#);
        assert!(candidates.contains(&key), "{}", r.recorder.dump());
    }
    let winner = &trace[1].sql;
    assert!(
        winner.contains(r#"INNER JOIN "oagw_route_method""#)
            && winner.contains(r#"LEFT JOIN "oagw_route_plugin""#)
            && winner.contains("match_config"),
        "{}",
        r.recorder.dump()
    );
    for child in ["oagw_route_method", "oagw_route_plugin"] {
        let key = format!(r#""{child}"."tenant_id" = "oagw_route"."tenant_id""#);
        assert!(winner.contains(&key), "{}", r.recorder.dump());
    }
    let tag_load = trace
        .iter()
        .find(|e| e.table.as_deref() == Some("oagw_route_tag"));
    match tags {
        // Tenant scope + one route ID.
        Tags::Load => assert_eq!(
            tag_load.map(|e| e.param_count),
            Some(2),
            "{}",
            r.recorder.dump()
        ),
        Tags::Skip => assert!(tag_load.is_none(), "{}", r.recorder.dump()),
    }
    take_selects(&r.recorder)
}

#[tokio::test]
async fn find_matching_loads_children_for_the_winner_only() {
    assert_eq!(find_matching_selects(Tags::Skip).await, 2);
    assert_eq!(find_matching_selects(Tags::Load).await, 3);
}

#[tokio::test]
async fn list_and_get_selects_do_not_grow_with_page_size() {
    let r = repos().await;
    let tenant = Uuid::new_v4();
    let mut upstream_ids = Vec::new();
    for i in 0..3 {
        upstream_ids.push(
            r.upstreams
                .create(upstream(tenant, &format!("api-{i}")))
                .await
                .unwrap()
                .id,
        );
    }
    for i in 0..3 {
        r.routes
            .create(route(tenant, upstream_ids[0], &format!("/v{i}")))
            .await
            .unwrap();
    }
    r.recorder.clear();

    let mut counts = Vec::new();
    for top in [1, 50] {
        let query = ListQuery { top, skip: 0 };
        r.upstreams.list(tenant, &query).await.unwrap();
        let upstreams = take_snapshot_selects(&r.recorder, "upstream list");
        r.routes.list(tenant, None, &query).await.unwrap();
        let routes = take_snapshot_selects(&r.recorder, "route list");
        r.routes
            .list(tenant, Some(upstream_ids[0]), &query)
            .await
            .unwrap();
        let routes_of_upstream = take_snapshot_selects(&r.recorder, "route list by upstream");
        counts.push((top, upstreams, routes, routes_of_upstream));
    }
    assert_eq!(counts, vec![(1, 3, 6, 6), (50, 3, 6, 6)]);

    r.upstreams
        .get_by_id(tenant, upstream_ids[0])
        .await
        .unwrap();
    assert_eq!(take_snapshot_selects(&r.recorder, "upstream get"), 3);
    let any_route = r
        .routes
        .list(tenant, None, &ListQuery::default())
        .await
        .unwrap()
        .remove(0);
    r.recorder.clear();
    r.routes.get_by_id(tenant, any_route.id).await.unwrap();
    // Optional children are loaded without joins, so no route is dropped.
    assert!(
        r.recorder.events().iter().all(|e| !e.sql.contains("JOIN")),
        "{}",
        r.recorder.dump()
    );
    assert_eq!(take_snapshot_selects(&r.recorder, "route get"), 6);
}

#[tokio::test]
async fn every_write_runs_in_one_transaction() {
    let r = repos().await;
    let tenant = Uuid::new_v4();

    let up = r.upstreams.create(upstream(tenant, "api")).await.unwrap();
    assert_one_transaction(&r.recorder, "upstream create");
    r.upstreams
        .update(Upstream {
            tags: vec!["c".into()],
            ..up.clone()
        })
        .await
        .unwrap();
    assert_one_transaction(&r.recorder, "upstream update");

    let created = r.routes.create(route(tenant, up.id, "/v1")).await.unwrap();
    assert_one_transaction(&r.recorder, "route create");
    r.routes
        .update(Route {
            priority: 3,
            ..created
        })
        .await
        .unwrap();
    assert_one_transaction(&r.recorder, "route update");
}

/// `delete_upstream` writes with one `DELETE`; the foreign keys cascade, so
/// there is no window between route and upstream removal. Only the owner check
/// reads before it.
#[tokio::test]
async fn delete_upstream_is_one_statement() {
    let r = repos().await;
    let tenant = Uuid::new_v4();
    let cp = ControlPlaneServiceImpl::new(
        r.upstreams.clone(),
        r.routes.clone(),
        Arc::new(MockTenantResolverClient::single_tenant()),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::empty()),
        Arc::new(SsrfGuard::disabled()),
    );
    let ctx = SecurityContext::builder()
        .subject_tenant_id(tenant)
        .subject_id(Uuid::new_v4())
        .build()
        .expect("security context");
    let up = r.upstreams.create(upstream(tenant, "api")).await.unwrap();
    let doomed = r.routes.create(route(tenant, up.id, "/v1")).await.unwrap();
    r.recorder.clear();

    cp.delete_upstream(&ctx, up.id).await.unwrap();

    let events = r.recorder.events();
    let writes: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                QueryKind::Insert | QueryKind::Update | QueryKind::Delete
            )
        })
        .collect();
    assert!(
        writes.len() == 1
            && writes[0].kind == QueryKind::Delete
            && writes[0].table.as_deref() == Some("oagw_upstream")
            && events.last().is_some_and(|e| e.kind == QueryKind::Delete),
        "{}",
        r.recorder.dump()
    );
    assert!(matches!(
        r.routes.get_by_id(tenant, doomed.id).await,
        Err(RepositoryError::NotFound { .. })
    ));
}

/// `create_upstream`, then `update_upstream`, for the leaf of a chain with
/// `ancestors` levels above it, binding to the root's upstream: the
/// `oagw_upstream` `SELECT` count of each.
async fn create_and_update_upstream_selects(ancestors: usize) -> (usize, usize) {
    let r = repos().await;
    // Root first, as the mock resolver expects.
    let chain: Vec<Uuid> = (0..=ancestors).map(|_| Uuid::new_v4()).collect();
    let cp = ControlPlaneServiceImpl::new(
        r.upstreams.clone(),
        r.routes.clone(),
        Arc::new(MockTenantResolverClient::with_hierarchy(
            chain.iter().copied().map(TenantId).collect(),
        )),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::empty()),
        Arc::new(SsrfGuard::disabled()),
    );
    let request = CreateUpstreamRequest {
        id: None,
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: "api.openai.com".into(),
                port: 443,
            }],
        },
        protocol: HTTP_PROTOCOL_ID.into(),
        alias: None,
        auth: None,
        headers: None,
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
        enabled: true,
    };
    let ctx = |tenant: Uuid| {
        SecurityContext::builder()
            .subject_tenant_id(tenant)
            .subject_id(Uuid::new_v4())
            .build()
            .expect("security context")
    };
    let root = cp
        .create_upstream(&ctx(chain[0]), request.clone())
        .await
        .unwrap();
    r.recorder.clear();

    let upstream_selects = || {
        let count = r
            .recorder
            .events()
            .iter()
            .filter(|e| e.kind == QueryKind::Select && e.table.as_deref() == Some("oagw_upstream"))
            .count();
        r.recorder.clear();
        count
    };

    let leaf_ctx = ctx(chain[ancestors]);
    let leaf = cp.create_upstream(&leaf_ctx, request).await.unwrap();
    assert_eq!(leaf.alias, root.alias);
    let create = upstream_selects();

    let update = UpdateUpstreamRequest {
        server: leaf.server.clone(),
        protocol: leaf.protocol.clone(),
        alias: Some(leaf.alias.clone()),
        auth: leaf.auth.clone(),
        headers: leaf.headers.clone(),
        plugins: leaf.plugins.clone(),
        rate_limit: leaf.rate_limit.clone(),
        cors: leaf.cors.clone(),
        tags: vec!["updated".into()],
        enabled: leaf.enabled,
    };
    cp.update_upstream(&leaf_ctx, leaf.id, update)
        .await
        .unwrap();
    (create, upstream_selects())
}

/// The closest ancestor is found with one lookup, not one per level, on
/// create and on update.
#[tokio::test]
async fn upstream_ancestor_lookup_does_not_grow_with_depth() {
    let depth_1 = create_and_update_upstream_selects(1).await;
    let depth_5 = create_and_update_upstream_selects(5).await;
    assert_eq!(depth_1, depth_5);
    // Create: the lookup. Update: the read of the row, then the lookup.
    assert_eq!(depth_1, (1, 2));
}
