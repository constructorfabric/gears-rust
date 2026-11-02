//! One test per row of the eviction table (ADR-0020): each checks what the
//! change evicts and what it keeps.

use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future;
use uuid::Uuid;

use super::Change;
use crate::domain::gts_helpers::HTTP_PROTOCOL_ID;
use crate::domain::model::{
    Endpoint, HttpMatch, HttpMethod, ManagedBy, MatchRules, PathSuffixMode, Route, Scheme, Server,
    Upstream,
};
use crate::domain::ports::NoopMetrics;
use crate::domain::repo::RowKey;
use crate::infra::proxy_cache::{AliasKey, ProxyConfigCache};

fn cache() -> ProxyConfigCache {
    ProxyConfigCache::new(Duration::from_secs(60), 100, Arc::new(NoopMetrics))
}

/// A root tenant `r` with a partner `p`, the partner's customer `c`, and
/// the root's other partner `s`. Chains run from the tenant to the root.
struct Tenants {
    r: Uuid,
    p: Uuid,
    c: Uuid,
    s: Uuid,
}

impl Tenants {
    fn new() -> Self {
        Self {
            r: Uuid::new_v4(),
            p: Uuid::new_v4(),
            c: Uuid::new_v4(),
            s: Uuid::new_v4(),
        }
    }

    fn c_chain(&self) -> Vec<Uuid> {
        vec![self.c, self.p, self.r]
    }

    fn p_chain(&self) -> Vec<Uuid> {
        vec![self.p, self.r]
    }

    fn s_chain(&self) -> Vec<Uuid> {
        vec![self.s, self.r]
    }
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
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
        managed_by: ManagedBy::Api,
    }
}

fn route(upstream: &Upstream) -> Route {
    Route {
        id: Uuid::new_v4(),
        tenant_id: upstream.tenant_id,
        upstream_id: upstream.id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Post],
                path: "/v1".into(),
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
        managed_by: ManagedBy::Api,
    }
}

fn row_key(upstream: &Upstream) -> RowKey {
    RowKey {
        tenant_id: upstream.tenant_id,
        id: upstream.id,
    }
}

async fn seed_alias(
    cache: &ProxyConfigCache,
    alias: &str,
    chain: Vec<Uuid>,
    upstreams: &[&Upstream],
) -> AliasKey {
    let key = AliasKey {
        alias: alias.into(),
        chain,
    };
    let value: Vec<Upstream> = upstreams.iter().map(|&u| u.clone()).collect();
    cache
        .aliases
        .get_or_load(key.clone(), move |_| future::ready(Ok(value)).boxed())
        .await
        .unwrap();
    key
}

async fn seed_routes(cache: &ProxyConfigCache, upstream: &Upstream, routes: &[&Route]) -> RowKey {
    let key = row_key(upstream);
    let value: Vec<Route> = routes.iter().map(|&r| r.clone()).collect();
    cache
        .routes
        .get_or_load(key, move |_| future::ready(Ok(value)).boxed())
        .await
        .unwrap();
    key
}

fn assert_aliases(cache: &ProxyConfigCache, evicted: &[&AliasKey], kept: &[&AliasKey]) {
    for key in evicted {
        assert!(!cache.aliases.contains(key), "evicted: {key:?}");
    }
    for key in kept {
        assert!(cache.aliases.contains(key), "kept: {key:?}");
    }
    cache.aliases.assert_consistent();
}

fn assert_routes(cache: &ProxyConfigCache, evicted: &[&RowKey], kept: &[&RowKey]) {
    for key in evicted {
        assert!(!cache.routes.contains(key), "evicted: {key:?}");
    }
    for key in kept {
        assert!(cache.routes.contains(key), "kept: {key:?}");
    }
    cache.routes.assert_consistent();
}

/// ADR-0020: CACHE-EVICT-001.
#[tokio::test]
async fn an_upstream_create_evicts_its_alias_where_the_chain_holds_its_tenant() {
    let (cache, t) = (cache(), Tenants::new());
    let (root_a, root_b) = (upstream(t.r, "a"), upstream(t.r, "b"));
    let c_a = seed_alias(&cache, "a", t.c_chain(), &[&root_a]).await;
    let p_a = seed_alias(&cache, "a", t.p_chain(), &[]).await;
    let s_a = seed_alias(&cache, "a", t.s_chain(), &[&root_a]).await;
    let c_b = seed_alias(&cache, "b", t.c_chain(), &[&root_b]).await;
    let root_routes = seed_routes(&cache, &root_a, &[&route(&root_a)]).await;

    let created = upstream(t.p, "a");
    cache.apply(&Change::Upstream {
        tenant_id: t.p,
        id: created.id,
        alias: "a".into(),
    });

    // `c` now finds the partner's upstream before the root's; `p` finds one
    // where it found none.
    assert_aliases(&cache, &[&c_a, &p_a], &[&s_a, &c_b]);
    assert_routes(&cache, &[], &[&root_routes]);
}

/// ADR-0020: CACHE-EVICT-002.
#[tokio::test]
async fn an_upstream_update_evicts_the_entries_holding_it() {
    let (cache, t) = (cache(), Tenants::new());
    let (root_a, root_b) = (upstream(t.r, "a"), upstream(t.r, "b"));
    let updated = upstream(t.p, "a");
    let c_a = seed_alias(&cache, "a", t.c_chain(), &[&updated, &root_a]).await;
    let p_a = seed_alias(&cache, "a", t.p_chain(), &[&updated, &root_a]).await;
    let s_a = seed_alias(&cache, "a", t.s_chain(), &[&root_a]).await;
    let c_b = seed_alias(&cache, "b", t.c_chain(), &[&root_b]).await;
    let routes = seed_routes(&cache, &updated, &[&route(&updated)]).await;

    // Disabled, or its sharing changed: the alias stays.
    cache.apply(&Change::Upstream {
        tenant_id: t.p,
        id: updated.id,
        alias: "a".into(),
    });

    assert_aliases(&cache, &[&c_a, &p_a], &[&s_a, &c_b]);
    assert_routes(&cache, &[], &[&routes]);
}

/// ADR-0020: CACHE-EVICT-002.
#[tokio::test]
async fn an_alias_rename_evicts_the_old_and_the_new_alias_entries() {
    let (cache, t) = (cache(), Tenants::new());
    let (root_a, root_b, root_x) = (upstream(t.r, "a"), upstream(t.r, "b"), upstream(t.r, "x"));
    let renamed = upstream(t.p, "a");
    let c_a = seed_alias(&cache, "a", t.c_chain(), &[&renamed, &root_a]).await;
    let c_b = seed_alias(&cache, "b", t.c_chain(), &[&root_b]).await;
    let p_b = seed_alias(&cache, "b", t.p_chain(), &[]).await;
    let s_a = seed_alias(&cache, "a", t.s_chain(), &[&root_a]).await;
    let s_b = seed_alias(&cache, "b", t.s_chain(), &[&root_b]).await;
    let c_x = seed_alias(&cache, "x", t.c_chain(), &[&root_x]).await;

    cache.apply(&Change::Upstream {
        tenant_id: t.p,
        id: renamed.id,
        alias: "b".into(),
    });

    assert_aliases(&cache, &[&c_a, &c_b, &p_b], &[&s_a, &s_b, &c_x]);
}

/// ADR-0020: CACHE-EVICT-003.
#[tokio::test]
async fn an_upstream_delete_evicts_the_entries_holding_it_and_its_routes() {
    let (cache, t) = (cache(), Tenants::new());
    let (root_a, root_b) = (upstream(t.r, "a"), upstream(t.r, "b"));
    let deleted = upstream(t.p, "a");
    let c_a = seed_alias(&cache, "a", t.c_chain(), &[&deleted, &root_a]).await;
    let p_a = seed_alias(&cache, "a", t.p_chain(), &[&deleted, &root_a]).await;
    let s_a = seed_alias(&cache, "a", t.s_chain(), &[&root_a]).await;
    let c_b = seed_alias(&cache, "b", t.c_chain(), &[&root_b]).await;
    let deleted_routes = seed_routes(&cache, &deleted, &[&route(&deleted)]).await;
    let root_routes = seed_routes(&cache, &root_a, &[&route(&root_a)]).await;

    cache.apply(&Change::UpstreamDeleted {
        tenant_id: t.p,
        id: deleted.id,
    });

    assert_aliases(&cache, &[&c_a, &p_a], &[&s_a, &c_b]);
    assert_routes(&cache, &[&deleted_routes], &[&root_routes]);
}

/// ADR-0020: CACHE-EVICT-003.
#[tokio::test]
async fn deleting_the_routes_of_an_upstream_evicts_its_route_entry() {
    let (cache, t) = (cache(), Tenants::new());
    let (deleted, other) = (upstream(t.p, "a"), upstream(t.p, "b"));
    let c_a = seed_alias(&cache, "a", t.c_chain(), &[&deleted]).await;
    let deleted_routes = seed_routes(&cache, &deleted, &[&route(&deleted)]).await;
    let other_routes = seed_routes(&cache, &other, &[&route(&other)]).await;

    cache.apply(&Change::RoutesOfUpstream {
        tenant_id: t.p,
        upstream_id: deleted.id,
    });

    assert_routes(&cache, &[&deleted_routes], &[&other_routes]);
    assert_aliases(&cache, &[], &[&c_a]);
}

/// ADR-0020: CACHE-EVICT-004.
#[tokio::test]
async fn a_route_create_or_update_evicts_its_upstreams_route_entry() {
    let (cache, t) = (cache(), Tenants::new());
    let (target, other) = (upstream(t.p, "a"), upstream(t.p, "b"));
    let updated = route(&target);
    let c_a = seed_alias(&cache, "a", t.c_chain(), &[&target]).await;
    let target_routes = seed_routes(&cache, &target, &[&updated]).await;
    let other_routes = seed_routes(&cache, &other, &[&route(&other)]).await;

    // A create names a route no entry holds yet; an update one that it does.
    for id in [Uuid::new_v4(), updated.id] {
        seed_routes(&cache, &target, &[&updated]).await;
        cache.apply(&Change::Route {
            tenant_id: t.p,
            id,
            upstream_id: target.id,
        });
        assert_routes(&cache, &[&target_routes], &[&other_routes]);
    }
    assert_aliases(&cache, &[], &[&c_a]);
}

/// ADR-0020: CACHE-EVICT-004.
#[tokio::test]
async fn a_route_move_evicts_both_upstreams_route_entries() {
    let (cache, t) = (cache(), Tenants::new());
    let (from, to, other) = (upstream(t.p, "a"), upstream(t.p, "b"), upstream(t.p, "c"));
    let moved = route(&from);
    let from_routes = seed_routes(&cache, &from, &[&moved, &route(&from)]).await;
    let to_routes = seed_routes(&cache, &to, &[&route(&to)]).await;
    let other_routes = seed_routes(&cache, &other, &[&route(&other)]).await;

    cache.apply(&Change::Route {
        tenant_id: t.p,
        id: moved.id,
        upstream_id: to.id,
    });

    assert_routes(&cache, &[&from_routes, &to_routes], &[&other_routes]);
}

/// ADR-0020: CACHE-EVICT-004.
#[tokio::test]
async fn a_route_delete_evicts_the_entry_holding_it() {
    let (cache, t) = (cache(), Tenants::new());
    let (owner, other) = (upstream(t.p, "a"), upstream(t.p, "b"));
    let deleted = route(&owner);
    let c_a = seed_alias(&cache, "a", t.c_chain(), &[&owner]).await;
    let owner_routes = seed_routes(&cache, &owner, &[&deleted, &route(&owner)]).await;
    let other_routes = seed_routes(&cache, &other, &[&route(&other)]).await;

    cache.apply(&Change::RouteDeleted {
        tenant_id: t.p,
        id: deleted.id,
    });

    assert_routes(&cache, &[&owner_routes], &[&other_routes]);
    assert_aliases(&cache, &[], &[&c_a]);
}

#[tokio::test]
async fn all_evicts_every_entry() {
    let (cache, t) = (cache(), Tenants::new());
    let root_a = upstream(t.r, "a");
    let c_a = seed_alias(&cache, "a", t.c_chain(), &[&root_a]).await;
    let routes = seed_routes(&cache, &root_a, &[&route(&root_a)]).await;

    cache.apply(&Change::All);

    assert_aliases(&cache, &[&c_a], &[]);
    assert_routes(&cache, &[&routes], &[]);
}

/// Start a load in each section that never answers.
fn start_loads(cache: &ProxyConfigCache, t: &Tenants) {
    let key = AliasKey {
        alias: "a".into(),
        chain: t.c_chain(),
    };
    let pending = cache
        .aliases
        .get_or_load(key, |_| future::pending().boxed())
        .now_or_never();
    assert!(pending.is_none());
    let key = RowKey {
        tenant_id: t.p,
        id: Uuid::new_v4(),
    };
    let pending = cache
        .routes
        .get_or_load(key, |_| future::pending().boxed())
        .now_or_never();
    assert!(pending.is_none());
    assert_eq!((cache.aliases.loads(), cache.routes.loads()), (1, 1));
}

/// ADR-0020: CACHE-EVICT-006.
#[tokio::test]
async fn a_change_detaches_the_loads_of_the_sections_it_touches_only() {
    let t = Tenants::new();
    let upstream_id = Uuid::new_v4();
    let cases = [
        (
            Change::Upstream {
                tenant_id: t.p,
                id: upstream_id,
                alias: "a".into(),
            },
            (0, 1),
        ),
        (
            Change::UpstreamDeleted {
                tenant_id: t.p,
                id: upstream_id,
            },
            (0, 0),
        ),
        (
            Change::Route {
                tenant_id: t.p,
                id: Uuid::new_v4(),
                upstream_id,
            },
            (1, 0),
        ),
        (
            Change::RouteDeleted {
                tenant_id: t.p,
                id: Uuid::new_v4(),
            },
            (1, 0),
        ),
        (
            Change::RoutesOfUpstream {
                tenant_id: t.p,
                upstream_id,
            },
            (1, 0),
        ),
        (Change::All, (0, 0)),
    ];
    for (change, loads) in cases {
        let cache = cache();
        start_loads(&cache, &t);
        cache.apply(&change);
        assert_eq!(
            (cache.aliases.loads(), cache.routes.loads()),
            loads,
            "(alias, route) loads left after {change:?}"
        );
    }
}

/// A log line names the first 16 changes of a batch, then how many more
/// there are; a batch of 16 names them all.
#[cfg(feature = "cluster")]
#[test]
fn a_batch_names_its_first_16_changes_then_how_many_more() {
    use super::Changes;

    let tenant_id = Uuid::new_v4();
    let changes: Vec<Change> = (0..17)
        .map(|_| Change::UpstreamDeleted {
            tenant_id,
            id: Uuid::new_v4(),
        })
        .collect();
    let line = Changes(&changes).to_string();
    let named: Vec<&str> = line.split("; ").collect();
    assert_eq!(named.len(), 17, "{line}");
    assert_eq!(
        named[..16],
        changes[..16]
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    assert_eq!(named[16], "and 1 more");

    let line = Changes(&changes[..16]).to_string();
    assert!(!line.contains("more"), "{line}");
    assert_eq!(line.split("; ").count(), 16);
}
