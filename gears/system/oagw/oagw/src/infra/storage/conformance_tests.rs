//! Backend-generic conformance suite for `UpstreamRepository` and `RouteRepository`.
//!
//! Every case talks to the repository traits only, through a [`Fixture`], so
//! each backend runs the same cases with one `conformance_suite!` line
//! (`cpt-cf-oagw-dod-domain-repo-conformance`): in-memory, `SQLite`, and
//! `PostgreSQL` and `MySQL` behind the `integration` feature. Cases assert
//! whole domain aggregates, not just IDs.
//!
//! Cases respect the database schema: every route belongs to an existing
//! upstream of the same tenant (composite FK `(tenant_id, upstream_id)`).
//! Because of that, tenant chain position never splits two routes of one
//! upstream; the suite pins it as a filter, and the tie-break itself is covered
//! by the `route_matching` unit tests.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use toolkit_db::{DBProvider, Db};
use uuid::Uuid;

use crate::domain::gts_helpers::{
    APIKEY_AUTH_PLUGIN_ID, AUTH_PLUGIN_SCHEMA, GRPC_PROTOCOL_ID, GUARD_PLUGIN_SCHEMA,
    HTTP_PROTOCOL_ID, REQUEST_ID_TRANSFORM_PLUGIN_ID, TIMEOUT_GUARD_PLUGIN_ID,
    TRANSFORM_PLUGIN_SCHEMA,
};
use crate::domain::model::{
    AuthConfig, BudgetConfig, BudgetMode, BurstConfig, CorsConfig, CorsHttpMethod, Endpoint,
    GrpcMatch, HeadersConfig, HttpMatch, HttpMethod, ListQuery, ManagedBy, MatchRules,
    PassthroughMode, PathSuffixMode, PluginBinding, PluginsConfig, RateLimitAlgorithm,
    RateLimitConfig, RateLimitScope, RateLimitStrategy, RequestHeaderRules, ResponseHeaderRules,
    Route, Scheme, Server, SharingMode, SustainedRate, Upstream, Window,
};
use crate::domain::repo::{RepositoryError, RouteRepository, RowKey, Tags, UpstreamRepository};
use crate::infra::storage::route_db_repo::DbRouteRepo;
use crate::infra::storage::testing;
use crate::infra::storage::upstream_db_repo::DbUpstreamRepo;
use crate::infra::storage::{InMemoryRouteRepo, InMemoryUpstreamRepo};

/// The repositories of one backend, as the Control Plane and Data Plane see them.
struct Fixture {
    upstreams: Arc<dyn UpstreamRepository>,
    routes: Arc<dyn RouteRepository>,
    /// Whatever must outlive the cases, such as a database container.
    _keep_alive: Option<Box<dyn std::any::Any>>,
}

/// Instantiate the suite for a backend.
///
/// `$factory` is an async fn (or closure returning a future) yielding an empty
/// [`Fixture`].
///
/// - `conformance_suite!(name, factory)` expands one `#[tokio::test]` per case,
///   each on a fresh fixture.
/// - `conformance_suite!(sequential name, factory)` expands one test that runs
///   every case in order on one fixture, for backends that are expensive to
///   start. Cases draw fresh tenant and row IDs, so they don't interfere.
macro_rules! conformance_suite {
    ($mod_name:ident, $factory:expr) => {
        conformance_suite!(@cases expand $mod_name, $factory);
    };
    (sequential $mod_name:ident, $factory:expr) => {
        conformance_suite!(@cases sequential $mod_name, $factory);
    };
    (@cases $arm:ident $mod_name:ident, $factory:expr) => {
        conformance_suite!(@$arm $mod_name, $factory;
            case_upstream_round_trip_all_fields,
            case_upstream_absent_and_empty_options_are_distinct,
            case_route_absent_and_empty_options_are_distinct,
            case_upstream_alias_conflict,
            case_upstream_duplicate_id_conflict,
            case_route_duplicate_id_conflict,
            case_upstream_tenant_isolation,
            case_route_tenant_isolation,
            case_upstream_list_paginates_by_id,
            case_route_list_paginates_by_id,
            case_route_round_trip_http,
            case_route_round_trip_grpc,
            case_route_update_replaces_children_and_keeps_upstream,
            case_upstream_delete_removes_only_its_routes,
            case_upstream_delete_removes_every_child_kind,
            case_list_by_alias_for_tenants_over_chain,
            case_proxy_reads_skip_only_tags,
            case_find_matching_prefers_upstream_order,
            case_find_matching_falls_back_to_ancestor_upstream,
            case_find_matching_longest_prefix_wins,
            case_find_matching_higher_priority_wins,
            case_find_matching_lowest_id_breaks_ties,
            case_find_matching_skips_disabled_grpc_and_method_mismatch,
            case_tags_and_methods_keep_canonical_order,
            case_tags_and_aliases_compare_byte_wise,
            case_managed_by_is_kept_on_update,
            case_list_registry_keys_spans_tenants,
        );
    };
    (@expand $mod_name:ident, $factory:expr; $($case:ident),+ $(,)?) => {
        mod $mod_name {
            use super::*;

            $(
                #[tokio::test]
                async fn $case() {
                    let fixture = ($factory)().await;
                    super::$case(&fixture).await;
                }
            )+
        }
    };
    (@sequential $mod_name:ident, $factory:expr; $($case:ident),+ $(,)?) => {
        mod $mod_name {
            use super::*;

            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn all_cases_in_order() {
                let fixture = ($factory)().await;
                $(super::$case(&fixture).await;)+
            }
        }
    };
}

async fn in_memory_fixture() -> Fixture {
    Fixture {
        upstreams: Arc::new(InMemoryUpstreamRepo::new()),
        routes: Arc::new(InMemoryRouteRepo::new()),
        _keep_alive: None,
    }
}

fn db_fixture(db: Db, keep_alive: Option<Box<dyn std::any::Any>>) -> Fixture {
    let db = DBProvider::<RepositoryError>::new(db);
    Fixture {
        upstreams: Arc::new(DbUpstreamRepo::new(db.clone())),
        routes: Arc::new(DbRouteRepo::new(db)),
        _keep_alive: keep_alive,
    }
}

/// A fresh, private in-memory `SQLite` database per test.
async fn sqlite_fixture() -> Fixture {
    db_fixture(testing::shared_sqlite().await, None)
}

/// One `PostgreSQL` container for the whole suite.
#[cfg(feature = "integration")]
async fn postgres_fixture() -> Fixture {
    let (container, db) = testing::postgres().await;
    db_fixture(db, Some(Box::new(container)))
}

/// One `MySQL` container for the whole suite.
#[cfg(feature = "integration")]
async fn mysql_fixture() -> Fixture {
    let (container, db) = testing::mysql().await;
    db_fixture(db, Some(Box::new(container)))
}

conformance_suite!(in_memory, in_memory_fixture);
conformance_suite!(sqlite, sqlite_fixture);
#[cfg(feature = "integration")]
conformance_suite!(sequential postgres, postgres_fixture);
#[cfg(feature = "integration")]
conformance_suite!(sequential mysql, mysql_fixture);

// ---------------------------------------------------------------------------
// Builders and helpers
// ---------------------------------------------------------------------------

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// Tenant chain, closest first: `[self, parent, ..., root]`.
fn tenant_chain(depth: usize) -> Vec<Uuid> {
    (0..depth).map(|_| Uuid::new_v4()).collect()
}

/// Upstream with every optional field left empty.
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
        managed_by: ManagedBy::Api,
    }
}

fn rate_limit() -> RateLimitConfig {
    RateLimitConfig {
        sharing: SharingMode::Enforce,
        algorithm: RateLimitAlgorithm::SlidingWindow,
        sustained: SustainedRate {
            rate: 100,
            window: Window::Minute,
        },
        burst: Some(BurstConfig { capacity: 20 }),
        budget: Some(BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(1000),
            overcommit_ratio: Some(1.5),
        }),
        scope: RateLimitScope::User,
        strategy: RateLimitStrategy::Queue,
        cost: 2,
        response_headers: false,
        // Merge-time only, never stored.
        pool_owner_id: None,
    }
}

fn cors(sharing: SharingMode) -> CorsConfig {
    CorsConfig {
        sharing,
        enabled: true,
        allowed_origins: strings(&["https://app.example.com"]),
        allowed_methods: vec![
            CorsHttpMethod::Get,
            CorsHttpMethod::Post,
            CorsHttpMethod::Options,
        ],
        expose_headers: strings(&["x-request-id"]),
        allow_credentials: true,
    }
}

/// Upstream with every optional field populated.
fn full_upstream(tenant_id: Uuid, alias: &str) -> Upstream {
    Upstream {
        server: Server {
            endpoints: vec![
                Endpoint {
                    scheme: Scheme::Https,
                    host: "api.example.com".into(),
                    port: 443,
                },
                Endpoint {
                    scheme: Scheme::Https,
                    host: "api-2.example.com".into(),
                    port: 8443,
                },
            ],
        },
        auth: Some(AuthConfig {
            plugin_type: format!("{AUTH_PLUGIN_SCHEMA}{}", Uuid::new_v4()),
            sharing: SharingMode::Inherit,
            config: Some(map(&[
                ("header", "X-Api-Key"),
                ("secret_ref", "cred://openai"),
            ])),
        }),
        headers: Some(HeadersConfig {
            request: Some(RequestHeaderRules {
                set: map(&[("x-env", "prod")]),
                add: map(&[("x-trace", "on")]),
                remove: strings(&["cookie"]),
                passthrough: PassthroughMode::Allowlist,
                passthrough_allowlist: strings(&["x-request-id", "accept"]),
            }),
            response: Some(ResponseHeaderRules {
                set: map(&[("cache-control", "no-store")]),
                add: HashMap::new(),
                remove: strings(&["server"]),
            }),
        }),
        plugins: Some(PluginsConfig {
            sharing: SharingMode::Enforce,
            items: vec![
                PluginBinding {
                    plugin_ref: TIMEOUT_GUARD_PLUGIN_ID.into(),
                    config: map(&[("timeout_ms", "5000")]),
                },
                PluginBinding {
                    plugin_ref: format!("{TRANSFORM_PLUGIN_SCHEMA}{}", Uuid::new_v4()),
                    config: HashMap::new(),
                },
            ],
        }),
        rate_limit: Some(rate_limit()),
        cors: Some(cors(SharingMode::Inherit)),
        tags: strings(&["ai", "llm"]),
        ..upstream(tenant_id, alias)
    }
}

/// Minimal enabled HTTP route matching `POST <path>`.
fn http_route(tenant_id: Uuid, upstream_id: Uuid, path: &str, priority: i32) -> Route {
    Route {
        id: Uuid::new_v4(),
        tenant_id,
        upstream_id,
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
        priority,
        enabled: true,
        managed_by: ManagedBy::Api,
    }
}

/// HTTP route on `/v1/chat` with every optional field populated.
fn full_http_route(tenant_id: Uuid, upstream_id: Uuid) -> Route {
    Route {
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Get, HttpMethod::Post, HttpMethod::Patch],
                path: "/v1/chat".into(),
                // Not canonicalized anywhere: stored as given.
                query_allowlist: strings(&["stream", "api-version"]),
                path_suffix_mode: PathSuffixMode::Disabled,
            }),
            grpc: None,
        },
        plugins: Some(PluginsConfig {
            sharing: SharingMode::Inherit,
            items: vec![
                PluginBinding {
                    plugin_ref: REQUEST_ID_TRANSFORM_PLUGIN_ID.into(),
                    config: map(&[("header", "x-request-id")]),
                },
                PluginBinding {
                    plugin_ref: format!("{GUARD_PLUGIN_SCHEMA}{}", Uuid::new_v4()),
                    config: map(&[("max_body", "1024")]),
                },
            ],
        }),
        rate_limit: Some(rate_limit()),
        // Route CORS keeps `sharing` inside its JSON.
        cors: Some(cors(SharingMode::Enforce)),
        tags: strings(&["chat", "v1"]),
        priority: 7,
        ..http_route(tenant_id, upstream_id, "/v1/chat", 0)
    }
}

fn grpc_match() -> MatchRules {
    MatchRules {
        http: None,
        grpc: Some(GrpcMatch {
            service: "example.chat.v1.Chat".into(),
            method: "Complete".into(),
        }),
    }
}

fn grpc_route(tenant_id: Uuid, upstream_id: Uuid) -> Route {
    Route {
        match_rules: grpc_match(),
        tags: strings(&["grpc"]),
        ..http_route(tenant_id, upstream_id, "/", 0)
    }
}

async fn add_upstream(f: &Fixture, upstream: Upstream) -> Upstream {
    f.upstreams.create(upstream).await.expect("create upstream")
}

async fn add_route(f: &Fixture, route: Route) -> Route {
    f.routes.create(route).await.expect("create route")
}

/// Delete an upstream as the Control Plane does: the upstream, then whatever
/// routes the backend left on it.
async fn delete_upstream(f: &Fixture, tenant_id: Uuid, id: Uuid) {
    f.upstreams
        .delete(tenant_id, id)
        .await
        .expect("delete upstream");
    f.routes
        .delete_by_upstream(tenant_id, id)
        .await
        .expect("delete routes of upstream");
}

async fn list_upstreams(f: &Fixture, tenant_id: Uuid, query: &ListQuery) -> Vec<Upstream> {
    f.upstreams
        .list(tenant_id, query)
        .await
        .expect("list upstreams")
}

async fn list_routes(
    f: &Fixture,
    tenant_id: Uuid,
    upstream_id: Option<Uuid>,
    query: &ListQuery,
) -> Vec<Route> {
    f.routes
        .list(tenant_id, upstream_id, query)
        .await
        .expect("list routes")
}

/// `list_by_alias_for_tenants`, sorted by ID: its result order is unspecified.
async fn by_alias(f: &Fixture, alias: &str, tenants: &HashSet<Uuid>) -> Vec<Upstream> {
    let mut found = f
        .upstreams
        .list_by_alias_for_tenants(alias, tenants, Tags::Load)
        .await
        .expect("list by alias");
    found.sort_by_key(|u| u.id);
    found
}

async fn find(
    f: &Fixture,
    chain: &[Uuid],
    upstream_ids: &[Uuid],
    method: &str,
    path: &str,
) -> Result<Route, RepositoryError> {
    f.routes
        .find_matching_in_tenants(chain, upstream_ids, method, path, Tags::Load)
        .await
}

fn sorted_upstreams(mut upstreams: Vec<Upstream>) -> Vec<Upstream> {
    upstreams.sort_by_key(|u| u.id);
    upstreams
}

fn sorted_routes(mut routes: Vec<Route>) -> Vec<Route> {
    routes.sort_by_key(|r| r.id);
    routes
}

#[track_caller]
fn assert_not_found<T: std::fmt::Debug>(result: Result<T, RepositoryError>) {
    assert!(
        matches!(result, Err(RepositoryError::NotFound { .. })),
        "expected NotFound, got {result:?}"
    );
}

#[track_caller]
fn assert_conflict<T: std::fmt::Debug>(result: Result<T, RepositoryError>, expected: &str) {
    assert!(
        matches!(&result, Err(RepositoryError::Conflict { entity, .. }) if *entity == expected),
        "expected {expected} Conflict, got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Upstream round trips
// ---------------------------------------------------------------------------

async fn case_upstream_round_trip_all_fields(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let tenants = HashSet::from([tenant]);
    let original = full_upstream(tenant, "openai");

    assert_eq!(
        f.upstreams.create(original.clone()).await.unwrap(),
        original
    );
    assert_eq!(
        f.upstreams.get_by_id(tenant, original.id).await.unwrap(),
        original
    );
    assert_eq!(
        list_upstreams(f, tenant, &ListQuery::default()).await,
        vec![original.clone()]
    );
    assert_eq!(
        by_alias(f, "openai", &tenants).await,
        vec![original.clone()]
    );

    let mut updated = original.clone();
    updated.alias = "openai-v2".into();
    updated.server.endpoints.truncate(1);
    updated.enabled = false;
    updated.auth = Some(AuthConfig {
        plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Enforce,
        config: None,
    });
    updated.headers = Some(HeadersConfig {
        request: None,
        response: Some(ResponseHeaderRules::default()),
    });
    if let Some(plugins) = updated.plugins.as_mut() {
        plugins.sharing = SharingMode::Private;
        // Plugins keep their position order.
        plugins.items.reverse();
    }
    updated.rate_limit = Some(RateLimitConfig {
        budget: Some(BudgetConfig {
            mode: BudgetMode::Shared,
            total: Some(500),
            overcommit_ratio: None,
        }),
        burst: None,
        ..rate_limit()
    });
    updated.cors = None;
    updated.tags = strings(&["llm"]);

    assert_eq!(f.upstreams.update(updated.clone()).await.unwrap(), updated);
    assert_eq!(
        f.upstreams.get_by_id(tenant, original.id).await.unwrap(),
        updated
    );
    assert!(by_alias(f, "openai", &tenants).await.is_empty());
    assert_eq!(
        by_alias(f, "openai-v2", &tenants).await,
        vec![updated.clone()]
    );

    f.upstreams.delete(tenant, original.id).await.unwrap();
    assert_not_found(f.upstreams.get_by_id(tenant, original.id).await);
    assert!(
        list_upstreams(f, tenant, &ListQuery::default())
            .await
            .is_empty()
    );
    assert!(by_alias(f, "openai-v2", &tenants).await.is_empty());
    assert_not_found(f.upstreams.delete(tenant, original.id).await);
}

async fn case_upstream_absent_and_empty_options_are_distinct(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let no_auth = upstream(tenant, "no-auth");
    let absent = Upstream {
        auth: Some(AuthConfig {
            plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Private,
            config: None,
        }),
        ..upstream(tenant, "absent")
    };
    let empty = Upstream {
        auth: Some(AuthConfig {
            plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Private,
            config: Some(HashMap::new()),
        }),
        headers: Some(HeadersConfig::default()),
        plugins: Some(PluginsConfig {
            sharing: SharingMode::Private,
            items: vec![],
        }),
        ..upstream(tenant, "empty")
    };

    for u in [&no_auth, &absent, &empty] {
        add_upstream(f, u.clone()).await;
    }
    for u in [&no_auth, &absent, &empty] {
        assert_eq!(&f.upstreams.get_by_id(tenant, u.id).await.unwrap(), u);
    }

    // Updates write the same distinction: swap the options of the two rows.
    let absent_to_empty = Upstream {
        auth: empty.auth.clone(),
        headers: empty.headers.clone(),
        plugins: empty.plugins.clone(),
        ..absent.clone()
    };
    let empty_to_absent = Upstream {
        auth: absent.auth.clone(),
        headers: None,
        plugins: None,
        ..empty.clone()
    };
    for u in [&absent_to_empty, &empty_to_absent] {
        f.upstreams.update(u.clone()).await.unwrap();
        assert_eq!(&f.upstreams.get_by_id(tenant, u.id).await.unwrap(), u);
    }
}

async fn case_route_absent_and_empty_options_are_distinct(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let up = add_upstream(f, upstream(tenant, "api")).await;
    let absent = http_route(tenant, up.id, "/absent", 0);
    let empty = Route {
        plugins: Some(PluginsConfig {
            sharing: SharingMode::Private,
            items: vec![],
        }),
        ..http_route(tenant, up.id, "/empty", 0)
    };

    for r in [&absent, &empty] {
        add_route(f, r.clone()).await;
    }
    for r in [&absent, &empty] {
        assert_eq!(&f.routes.get_by_id(tenant, r.id).await.unwrap(), r);
    }

    let absent_to_empty = Route {
        plugins: empty.plugins.clone(),
        ..absent.clone()
    };
    let empty_to_absent = Route {
        plugins: None,
        ..empty.clone()
    };
    for r in [&absent_to_empty, &empty_to_absent] {
        f.routes.update(r.clone()).await.unwrap();
        assert_eq!(&f.routes.get_by_id(tenant, r.id).await.unwrap(), r);
    }
}

// ---------------------------------------------------------------------------
// Conflicts and tenant isolation
// ---------------------------------------------------------------------------

async fn case_upstream_alias_conflict(f: &Fixture) {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let tenants = HashSet::from([tenant]);
    let openai = add_upstream(f, upstream(tenant, "openai")).await;
    let anthropic = add_upstream(f, upstream(tenant, "anthropic")).await;

    let duplicate = upstream(tenant, "openai");
    assert_conflict(f.upstreams.create(duplicate.clone()).await, "upstream");
    assert_not_found(f.upstreams.get_by_id(tenant, duplicate.id).await);

    let mut renamed = anthropic.clone();
    renamed.alias = "openai".into();
    let renamed_conflict = f.upstreams.update(renamed).await;
    // On update only the alias can clash; the message must not blame the ID.
    assert!(
        matches!(&renamed_conflict, Err(RepositoryError::Conflict { detail, .. })
            if detail == "alias 'openai' already exists for tenant"),
        "{renamed_conflict:?}"
    );

    assert_eq!(
        list_upstreams(f, tenant, &ListQuery::default()).await,
        sorted_upstreams(vec![openai.clone(), anthropic.clone()])
    );
    assert_eq!(by_alias(f, "openai", &tenants).await, vec![openai]);
    assert_eq!(by_alias(f, "anthropic", &tenants).await, vec![anthropic]);

    // Aliases are unique per tenant only.
    let elsewhere = upstream(other, "openai");
    assert_eq!(
        f.upstreams.create(elsewhere.clone()).await.unwrap(),
        elsewhere
    );
}

async fn case_upstream_duplicate_id_conflict(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let tenants = HashSet::from([tenant]);
    let original = add_upstream(f, upstream(tenant, "a")).await;

    let duplicate = Upstream {
        alias: "b".into(),
        ..original.clone()
    };
    assert_conflict(f.upstreams.create(duplicate).await, "upstream");

    // The original row and its alias are untouched; alias "b" stays free.
    assert_eq!(
        f.upstreams.get_by_id(tenant, original.id).await.unwrap(),
        original
    );
    assert_eq!(by_alias(f, "a", &tenants).await, vec![original]);
    assert!(by_alias(f, "b", &tenants).await.is_empty());
    add_upstream(f, upstream(tenant, "b")).await;
}

async fn case_route_duplicate_id_conflict(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let up = add_upstream(f, upstream(tenant, "api")).await;
    let original = add_route(f, http_route(tenant, up.id, "/a", 1)).await;

    let duplicate = Route {
        priority: 2,
        ..original.clone()
    };
    assert_conflict(f.routes.create(duplicate).await, "route");

    assert_eq!(
        list_routes(f, tenant, Some(up.id), &ListQuery::default()).await,
        vec![original]
    );
}

async fn case_upstream_tenant_isolation(f: &Fixture) {
    let (owner, intruder) = (Uuid::new_v4(), Uuid::new_v4());
    let original = add_upstream(f, full_upstream(owner, "openai")).await;

    assert_not_found(f.upstreams.get_by_id(intruder, original.id).await);
    let foreign = Upstream {
        tenant_id: intruder,
        alias: "hijacked".into(),
        ..original.clone()
    };
    assert_not_found(f.upstreams.update(foreign).await);
    assert_not_found(f.upstreams.delete(intruder, original.id).await);
    assert!(
        list_upstreams(f, intruder, &ListQuery::default())
            .await
            .is_empty()
    );
    assert!(
        by_alias(f, "openai", &HashSet::from([intruder]))
            .await
            .is_empty()
    );

    assert_eq!(
        f.upstreams.get_by_id(owner, original.id).await.unwrap(),
        original
    );
    assert_eq!(
        by_alias(f, "openai", &HashSet::from([owner])).await,
        vec![original]
    );
}

async fn case_route_tenant_isolation(f: &Fixture) {
    let (owner, intruder) = (Uuid::new_v4(), Uuid::new_v4());
    let up = add_upstream(f, upstream(owner, "api")).await;
    let original = add_route(f, full_http_route(owner, up.id)).await;

    assert_not_found(f.routes.get_by_id(intruder, original.id).await);
    let foreign = Route {
        tenant_id: intruder,
        priority: 99,
        ..original.clone()
    };
    assert_not_found(f.routes.update(foreign).await);
    assert_not_found(f.routes.delete(intruder, original.id).await);
    assert!(
        list_routes(f, intruder, None, &ListQuery::default())
            .await
            .is_empty()
    );
    assert!(
        list_routes(f, intruder, Some(up.id), &ListQuery::default())
            .await
            .is_empty()
    );
    f.routes.delete_by_upstream(intruder, up.id).await.unwrap();
    assert_not_found(find(f, &[intruder], &[up.id], "POST", "/v1/chat").await);

    assert_eq!(
        f.routes.get_by_id(owner, original.id).await.unwrap(),
        original
    );
    assert_eq!(
        list_routes(f, owner, Some(up.id), &ListQuery::default()).await,
        vec![original]
    );
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

async fn case_upstream_list_paginates_by_id(f: &Fixture) {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let mut all = Vec::new();
    for i in 0..7 {
        all.push(add_upstream(f, upstream(tenant, &format!("svc-{i}"))).await);
    }
    add_upstream(f, upstream(other, "svc-0")).await;
    let all = sorted_upstreams(all);

    let mut pages = Vec::new();
    for skip in [0, 3, 6] {
        pages.push(list_upstreams(f, tenant, &ListQuery { top: 3, skip }).await);
    }
    assert_eq!(
        pages,
        vec![all[0..3].to_vec(), all[3..6].to_vec(), all[6..].to_vec()]
    );
    assert!(
        list_upstreams(f, tenant, &ListQuery { top: 3, skip: 7 })
            .await
            .is_empty()
    );
}

async fn case_route_list_paginates_by_id(f: &Fixture) {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let first = add_upstream(f, upstream(tenant, "first")).await;
    let second = add_upstream(f, upstream(tenant, "second")).await;
    let foreign = add_upstream(f, upstream(other, "first")).await;

    let mut on_first = Vec::new();
    let mut all = Vec::new();
    for i in 0..7 {
        let upstream_id = if i < 4 { first.id } else { second.id };
        let route = add_route(f, http_route(tenant, upstream_id, &format!("/r{i}"), 0)).await;
        if upstream_id == first.id {
            on_first.push(route.clone());
        }
        all.push(route);
    }
    add_route(f, http_route(other, foreign.id, "/r0", 0)).await;
    let all = sorted_routes(all);
    let on_first = sorted_routes(on_first);

    let mut pages = Vec::new();
    for skip in [0, 3, 6] {
        pages.push(list_routes(f, tenant, None, &ListQuery { top: 3, skip }).await);
    }
    assert_eq!(
        pages,
        vec![all[0..3].to_vec(), all[3..6].to_vec(), all[6..].to_vec()]
    );

    assert_eq!(
        list_routes(f, tenant, Some(first.id), &ListQuery::default()).await,
        on_first
    );
    assert_eq!(
        list_routes(f, tenant, Some(first.id), &ListQuery { top: 2, skip: 2 }).await,
        on_first[2..4].to_vec()
    );
    assert!(
        list_routes(f, tenant, Some(foreign.id), &ListQuery::default())
            .await
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Route round trips and updates
// ---------------------------------------------------------------------------

async fn case_route_round_trip_http(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let up = add_upstream(f, upstream(tenant, "api")).await;
    let route = full_http_route(tenant, up.id);

    assert_eq!(f.routes.create(route.clone()).await.unwrap(), route);
    assert_eq!(f.routes.get_by_id(tenant, route.id).await.unwrap(), route);
    assert_eq!(
        list_routes(f, tenant, Some(up.id), &ListQuery::default()).await,
        vec![route.clone()]
    );
    assert_eq!(
        list_routes(f, tenant, None, &ListQuery::default()).await,
        vec![route.clone()]
    );

    f.routes.delete(tenant, route.id).await.unwrap();
    assert_not_found(f.routes.get_by_id(tenant, route.id).await);
    assert_not_found(f.routes.delete(tenant, route.id).await);
}

async fn case_route_round_trip_grpc(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let up = add_upstream(
        f,
        Upstream {
            protocol: GRPC_PROTOCOL_ID.into(),
            ..upstream(tenant, "grpc")
        },
    )
    .await;
    let route = grpc_route(tenant, up.id);

    assert_eq!(f.routes.create(route.clone()).await.unwrap(), route);
    assert_eq!(f.routes.get_by_id(tenant, route.id).await.unwrap(), route);
    assert_eq!(
        list_routes(f, tenant, Some(up.id), &ListQuery::default()).await,
        vec![route]
    );
}

async fn case_route_update_replaces_children_and_keeps_upstream(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let home = add_upstream(f, upstream(tenant, "home")).await;
    let elsewhere = add_upstream(f, upstream(tenant, "elsewhere")).await;
    let original = add_route(f, full_http_route(tenant, home.id)).await;

    let changed = Route {
        // Ignored: a route never moves to another upstream.
        upstream_id: elsewhere.id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Put],
                path: "/v2".into(),
                query_allowlist: vec![],
                path_suffix_mode: PathSuffixMode::Append,
            }),
            grpc: None,
        },
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: strings(&["v2"]),
        priority: 1,
        ..original.clone()
    };
    let expected = Route {
        upstream_id: home.id,
        ..changed.clone()
    };

    assert_eq!(f.routes.update(changed).await.unwrap(), expected);
    assert_eq!(
        f.routes.get_by_id(tenant, original.id).await.unwrap(),
        expected
    );
    assert_eq!(
        list_routes(f, tenant, Some(home.id), &ListQuery::default()).await,
        vec![expected.clone()]
    );
    assert!(
        list_routes(f, tenant, Some(elsewhere.id), &ListQuery::default())
            .await
            .is_empty()
    );
    // Matching uses the new methods and path only.
    assert_eq!(
        find(f, &[tenant], &[home.id], "PUT", "/v2/x")
            .await
            .unwrap(),
        expected
    );
    assert_not_found(find(f, &[tenant], &[home.id], "POST", "/v1/chat").await);

    // Switching the match type replaces the HTTP match with a gRPC one.
    let grpc = Route {
        match_rules: grpc_match(),
        ..expected
    };
    assert_eq!(f.routes.update(grpc.clone()).await.unwrap(), grpc);
    assert_eq!(f.routes.get_by_id(tenant, grpc.id).await.unwrap(), grpc);
    assert_not_found(find(f, &[tenant], &[home.id], "PUT", "/v2/x").await);
}

// ---------------------------------------------------------------------------
// Deletes
// ---------------------------------------------------------------------------

async fn case_upstream_delete_removes_only_its_routes(f: &Fixture) {
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let target = add_upstream(f, upstream(tenant, "target")).await;
    let sibling = add_upstream(f, upstream(tenant, "sibling")).await;
    let foreign = add_upstream(f, upstream(other, "target")).await;

    let mut doomed = Vec::new();
    for i in 0..3 {
        doomed.push(add_route(f, http_route(tenant, target.id, &format!("/r{i}"), 0)).await);
    }
    let survivor = add_route(f, http_route(tenant, sibling.id, "/s", 0)).await;
    let foreign_route = add_route(f, http_route(other, foreign.id, "/f", 0)).await;

    // Another tenant deletes neither the upstream nor its routes.
    assert_not_found(f.upstreams.delete(other, target.id).await);
    f.routes.delete_by_upstream(other, target.id).await.unwrap();
    assert_eq!(
        list_routes(f, tenant, Some(target.id), &ListQuery::default())
            .await
            .len(),
        doomed.len()
    );

    delete_upstream(f, tenant, target.id).await;

    assert!(
        list_routes(f, tenant, Some(target.id), &ListQuery::default())
            .await
            .is_empty()
    );
    for route in &doomed {
        assert_not_found(f.routes.get_by_id(tenant, route.id).await);
    }
    assert_eq!(
        f.routes.get_by_id(tenant, survivor.id).await.unwrap(),
        survivor
    );
    assert_eq!(
        f.routes.get_by_id(other, foreign_route.id).await.unwrap(),
        foreign_route
    );

    // Deleting again finds no upstream, and no routes are left to remove.
    assert_not_found(f.upstreams.delete(tenant, target.id).await);
    f.routes
        .delete_by_upstream(tenant, target.id)
        .await
        .unwrap();
}

async fn case_upstream_delete_removes_every_child_kind(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let up = add_upstream(f, full_upstream(tenant, "api")).await;
    let routes = [
        add_route(f, full_http_route(tenant, up.id)).await,
        add_route(f, http_route(tenant, up.id, "/other", 0)).await,
    ];

    delete_upstream(f, tenant, up.id).await;

    assert_not_found(f.upstreams.get_by_id(tenant, up.id).await);
    assert!(
        by_alias(f, "api", &HashSet::from([tenant]))
            .await
            .is_empty()
    );
    assert!(
        list_routes(f, tenant, Some(up.id), &ListQuery::default())
            .await
            .is_empty()
    );
    for route in &routes {
        assert_not_found(f.routes.get_by_id(tenant, route.id).await);
    }
    // The alias is free again.
    add_upstream(f, full_upstream(tenant, "api")).await;
}

// ---------------------------------------------------------------------------
// Hierarchy lookups
// ---------------------------------------------------------------------------

async fn case_list_by_alias_for_tenants_over_chain(f: &Fixture) {
    let chain = tenant_chain(5);
    let mut shared = Vec::new();
    for tenant in &chain {
        shared.push(add_upstream(f, upstream(*tenant, "shared")).await);
    }
    add_upstream(f, upstream(chain[2], "other")).await;
    add_upstream(f, upstream(Uuid::new_v4(), "shared")).await;

    let whole_chain: HashSet<Uuid> = chain.iter().copied().collect();
    assert_eq!(
        by_alias(f, "shared", &whole_chain).await,
        sorted_upstreams(shared.clone())
    );
    let subset = HashSet::from([chain[0], chain[2], chain[4]]);
    assert_eq!(
        by_alias(f, "shared", &subset).await,
        sorted_upstreams(vec![
            shared[0].clone(),
            shared[2].clone(),
            shared[4].clone()
        ])
    );
    assert!(by_alias(f, "shared", &HashSet::new()).await.is_empty());
    assert!(by_alias(f, "missing", &whole_chain).await.is_empty());
}

/// The proxy's reads (`Tags::Skip`) return what the full reads return except
/// tags: every plugin of an upstream (several, or none) and every method of a
/// route still come back.
async fn case_proxy_reads_skip_only_tags(f: &Fixture) {
    let chain = tenant_chain(2);
    let full = add_upstream(f, full_upstream(chain[0], "proxy-read")).await;
    let bare = add_upstream(f, upstream(chain[1], "proxy-read")).await;
    let route = add_route(f, full_http_route(chain[0], full.id)).await;
    let tenants: HashSet<Uuid> = chain.iter().copied().collect();

    let mut untagged = full.clone();
    untagged.tags.clear();
    let mut skipped = f
        .upstreams
        .list_by_alias_for_tenants("proxy-read", &tenants, Tags::Skip)
        .await
        .expect("list by alias without tags");
    skipped.sort_by_key(|u| u.id);
    assert_eq!(skipped, sorted_upstreams(vec![untagged, bare.clone()]));
    assert_eq!(
        by_alias(f, "proxy-read", &tenants).await,
        sorted_upstreams(vec![full.clone(), bare])
    );

    let mut untagged = route.clone();
    untagged.tags.clear();
    assert_eq!(
        f.routes
            .find_matching_in_tenants(&chain, &[full.id], "PATCH", "/v1/chat", Tags::Skip)
            .await
            .expect("match without tags"),
        untagged
    );
    assert_eq!(
        find(f, &chain, &[full.id], "PATCH", "/v1/chat")
            .await
            .unwrap(),
        route
    );
}

/// One upstream aliased `api` per tenant of a 5-level chain; IDs closest-first.
async fn upstream_per_tenant(f: &Fixture, chain: &[Uuid]) -> Vec<Uuid> {
    let mut ids = Vec::new();
    for tenant in chain {
        ids.push(add_upstream(f, upstream(*tenant, "api")).await.id);
    }
    ids
}

async fn case_find_matching_prefers_upstream_order(f: &Fixture) {
    let chain = tenant_chain(5);
    let ups = upstream_per_tenant(f, &chain).await;

    let root_route = add_route(f, http_route(chain[4], ups[4], "/v1/chat", 100)).await;
    let mid_route = add_route(f, http_route(chain[2], ups[2], "/v1", 0)).await;

    // A closer upstream wins over a longer prefix and a higher priority.
    assert_eq!(
        find(f, &chain, &ups, "POST", "/v1/chat").await.unwrap(),
        mid_route
    );
    // Upstream order decides before tenant chain position.
    assert_eq!(
        find(f, &chain, &[ups[4], ups[2]], "POST", "/v1/chat")
            .await
            .unwrap(),
        root_route
    );

    let own_route = add_route(f, http_route(chain[0], ups[0], "/", 0)).await;
    assert_eq!(
        find(f, &chain, &ups, "POST", "/v1/chat").await.unwrap(),
        own_route
    );
}

async fn case_find_matching_falls_back_to_ancestor_upstream(f: &Fixture) {
    let chain = tenant_chain(5);
    let ups = upstream_per_tenant(f, &chain).await;
    let root_route = add_route(f, full_http_route(chain[4], ups[4])).await;

    assert_eq!(
        find(f, &chain, &ups, "POST", "/v1/chat/completions")
            .await
            .unwrap(),
        root_route
    );
    // Only reachable through its upstream ID ...
    assert_not_found(find(f, &chain, &ups[..4], "POST", "/v1/chat").await);
    // ... and only while its tenant is in the chain.
    assert_not_found(find(f, &chain[..4], &ups, "POST", "/v1/chat").await);
}

async fn case_find_matching_longest_prefix_wins(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let up = add_upstream(f, upstream(tenant, "api")).await;
    add_route(f, http_route(tenant, up.id, "/v1", 100)).await;
    let long = add_route(f, http_route(tenant, up.id, "/v1/chat", 0)).await;

    assert_eq!(
        find(f, &[tenant], &[up.id], "POST", "/v1/chat/completions")
            .await
            .unwrap(),
        long
    );
}

async fn case_find_matching_higher_priority_wins(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let up = add_upstream(f, upstream(tenant, "api")).await;
    add_route(f, http_route(tenant, up.id, "/v1", 1)).await;
    let high = add_route(f, http_route(tenant, up.id, "/v1", 5)).await;

    assert_eq!(
        find(f, &[tenant], &[up.id], "POST", "/v1/x").await.unwrap(),
        high
    );
}

async fn case_find_matching_lowest_id_breaks_ties(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let up = add_upstream(f, upstream(tenant, "api")).await;
    let mut first = http_route(tenant, up.id, "/v1", 0);
    let mut second = http_route(tenant, up.id, "/v1", 0);
    // Insert the higher ID first so insertion order cannot pick the winner.
    if first.id < second.id {
        std::mem::swap(&mut first, &mut second);
    }
    add_route(f, first).await;
    let lowest = add_route(f, second).await;

    assert_eq!(
        find(f, &[tenant], &[up.id], "POST", "/v1/x").await.unwrap(),
        lowest
    );
}

async fn case_find_matching_skips_disabled_grpc_and_method_mismatch(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let up = add_upstream(f, upstream(tenant, "api")).await;
    add_route(
        f,
        Route {
            enabled: false,
            ..http_route(tenant, up.id, "/v1/chat", 0)
        },
    )
    .await;
    add_route(f, grpc_route(tenant, up.id)).await;
    let enabled = add_route(f, http_route(tenant, up.id, "/v1", 0)).await;

    // The disabled route has the longer prefix but is skipped.
    assert_eq!(
        find(f, &[tenant], &[up.id], "POST", "/v1/chat/x")
            .await
            .unwrap(),
        enabled
    );
    assert_not_found(find(f, &[tenant], &[up.id], "GET", "/v1/chat").await);
    assert_not_found(find(f, &[tenant], &[up.id], "HEAD", "/v1/chat").await);
    assert_not_found(find(f, &[tenant], &[up.id], "POST", "/v2").await);
}

// ---------------------------------------------------------------------------
// Canonical order
// ---------------------------------------------------------------------------

/// The Control Plane always passes tags and methods canonical, and every
/// backend returns them in that order; the database sorts them on read. A
/// backend that sorts tags by a locale collation, or methods by name, returns
/// different aggregates.
async fn case_tags_and_methods_keep_canonical_order(f: &Fixture) {
    let tenant = Uuid::new_v4();
    // Byte order: uppercase before lowercase, '-' before '_'. A locale
    // collation orders these differently.
    let tags = strings(&["Beta", "alpha", "gamma-2", "gamma_1"]);
    let mut canonical = tags.clone();
    canonical.sort();
    assert_eq!(canonical, tags, "fixture tags must be canonical");
    // Declaration order; alphabetical would be DELETE, GET, PATCH, POST, PUT.
    let methods = vec![
        HttpMethod::Get,
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Delete,
        HttpMethod::Patch,
    ];

    let up = add_upstream(
        f,
        Upstream {
            tags: tags.clone(),
            ..upstream(tenant, "api")
        },
    )
    .await;
    let mut route = http_route(tenant, up.id, "/v1", 0);
    route.tags = tags;
    if let Some(http) = route.match_rules.http.as_mut() {
        http.methods = methods;
    }
    let route = add_route(f, route).await;

    assert_eq!(f.upstreams.get_by_id(tenant, up.id).await.unwrap(), up);
    assert_eq!(
        list_upstreams(f, tenant, &ListQuery::default()).await,
        vec![up.clone()]
    );
    assert_eq!(
        by_alias(f, "api", &HashSet::from([tenant])).await,
        vec![up.clone()]
    );
    assert_eq!(f.routes.get_by_id(tenant, route.id).await.unwrap(), route);
    assert_eq!(
        list_routes(f, tenant, None, &ListQuery::default()).await,
        vec![route.clone()]
    );
    assert_eq!(
        find(f, &[tenant], &[up.id], "PATCH", "/v1").await.unwrap(),
        route
    );

    // Updates rewrite the child rows; the order survives that too.
    let up = Upstream {
        tags: strings(&["Zeta", "alpha"]),
        ..up
    };
    f.upstreams.update(up.clone()).await.unwrap();
    assert_eq!(f.upstreams.get_by_id(tenant, up.id).await.unwrap(), up);

    let mut route = Route {
        tags: strings(&["Zeta", "alpha"]),
        ..route
    };
    if let Some(http) = route.match_rules.http.as_mut() {
        http.methods = vec![HttpMethod::Post, HttpMethod::Delete];
    }
    f.routes.update(route.clone()).await.unwrap();
    assert_eq!(f.routes.get_by_id(tenant, route.id).await.unwrap(), route);
}

/// Tags and aliases are equal only when their bytes are, on every backend.
/// `MySQL`'s default collation would treat case, accent and width variants as
/// equal, so its keys would reject such tags and an alias lookup would return
/// another alias.
async fn case_tags_and_aliases_compare_byte_wise(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let tenants = HashSet::from([tenant]);
    // Pairs differing only in case, trailing space, accent or width.
    let tags = strings(&["Prod", "a", "a ", "cafe", "café", "prod", "ａ"]);
    let mut canonical = tags.clone();
    canonical.sort();
    assert_eq!(canonical, tags, "fixture tags must be canonical");

    let up = add_upstream(
        f,
        Upstream {
            tags: tags.clone(),
            ..upstream(tenant, "API.example.com")
        },
    )
    .await;
    let lower = add_upstream(f, upstream(tenant, "api.example.com")).await;
    assert_eq!(f.upstreams.get_by_id(tenant, up.id).await.unwrap(), up);
    assert_eq!(
        by_alias(f, "API.example.com", &tenants).await,
        vec![up.clone()]
    );
    assert_eq!(by_alias(f, "api.example.com", &tenants).await, vec![lower]);

    let mut route = http_route(tenant, up.id, "/v1", 0);
    route.tags = tags;
    let route = add_route(f, route).await;
    assert_eq!(f.routes.get_by_id(tenant, route.id).await.unwrap(), route);

    // Updates rewrite the tag rows under the same keys.
    let up = Upstream {
        tags: strings(&["X", "x"]),
        ..up
    };
    f.upstreams.update(up.clone()).await.unwrap();
    assert_eq!(f.upstreams.get_by_id(tenant, up.id).await.unwrap(), up);
    let route = Route {
        tags: strings(&["X", "x"]),
        ..route
    };
    f.routes.update(route.clone()).await.unwrap();
    assert_eq!(f.routes.get_by_id(tenant, route.id).await.unwrap(), route);
}

/// The owner is written on create and survives an update that names another.
async fn case_managed_by_is_kept_on_update(f: &Fixture) {
    let tenant = Uuid::new_v4();
    let registry = add_upstream(
        f,
        Upstream {
            managed_by: ManagedBy::Registry,
            ..upstream(tenant, "registry")
        },
    )
    .await;
    let api = add_upstream(f, upstream(tenant, "api")).await;
    let route = add_route(
        f,
        Route {
            managed_by: ManagedBy::Registry,
            ..http_route(tenant, registry.id, "/v1", 0)
        },
    )
    .await;
    assert_eq!(
        f.upstreams.get_by_id(tenant, registry.id).await.unwrap(),
        registry
    );
    assert_eq!(f.routes.get_by_id(tenant, route.id).await.unwrap(), route);

    let flipped = |u: &Upstream, managed_by| Upstream {
        managed_by,
        enabled: false,
        ..u.clone()
    };
    for (stored, other) in [(&registry, ManagedBy::Api), (&api, ManagedBy::Registry)] {
        let expected = flipped(stored, stored.managed_by);
        assert_eq!(
            f.upstreams.update(flipped(stored, other)).await.unwrap(),
            expected
        );
        assert_eq!(
            f.upstreams.get_by_id(tenant, stored.id).await.unwrap(),
            expected
        );
    }

    let expected = Route {
        priority: 7,
        ..route.clone()
    };
    let changed = Route {
        managed_by: ManagedBy::Api,
        ..expected.clone()
    };
    assert_eq!(f.routes.update(changed).await.unwrap(), expected);
    assert_eq!(
        f.routes.get_by_id(tenant, route.id).await.unwrap(),
        expected
    );
}

/// Registry keys come from every tenant, in key order, and only registry rows.
async fn case_list_registry_keys_spans_tenants(f: &Fixture) {
    let tenants = [Uuid::new_v4(), Uuid::new_v4()];
    let mut expected_upstreams = Vec::new();
    let mut expected_routes = Vec::new();
    for tenant in tenants {
        let registry = add_upstream(
            f,
            Upstream {
                managed_by: ManagedBy::Registry,
                ..upstream(tenant, "registry")
            },
        )
        .await;
        let api = add_upstream(f, upstream(tenant, "api")).await;
        let registry_route = add_route(
            f,
            Route {
                managed_by: ManagedBy::Registry,
                ..http_route(tenant, registry.id, "/r", 0)
            },
        )
        .await;
        add_route(f, http_route(tenant, registry.id, "/user", 0)).await;
        add_route(f, http_route(tenant, api.id, "/a", 0)).await;
        expected_upstreams.push(RowKey {
            tenant_id: tenant,
            id: registry.id,
        });
        expected_routes.push(RowKey {
            tenant_id: tenant,
            id: registry_route.id,
        });
    }
    expected_upstreams.sort();
    expected_routes.sort();

    // Other cases may share the backend; keep this case's tenants only.
    let own = |keys: Vec<RowKey>| -> Vec<RowKey> {
        keys.into_iter()
            .filter(|k| tenants.contains(&k.tenant_id))
            .collect()
    };
    let upstream_keys = f.upstreams.list_registry_keys().await.unwrap();
    let route_keys = f.routes.list_registry_keys().await.unwrap();
    assert!(upstream_keys.is_sorted() && route_keys.is_sorted());
    assert_eq!(own(upstream_keys), expected_upstreams);
    assert_eq!(own(route_keys), expected_routes);
}
