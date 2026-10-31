use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use super::{AliasKey, CachedProxyConfigReader, Change, ProxyConfigCache};
use crate::domain::gts_helpers::HTTP_PROTOCOL_ID;
use crate::domain::model::{
    Endpoint, HttpMatch, HttpMethod, ManagedBy, MatchRules, PathSuffixMode, Route, Scheme, Server,
    Upstream,
};
use crate::domain::ports::NoopMetrics;
use crate::domain::repo::{ProxyConfigReader, RouteRepository, RowKey, UpstreamRepository};
use crate::infra::storage::{InMemoryRouteRepo, InMemoryUpstreamRepo};

struct Fixture {
    cache: Arc<ProxyConfigCache>,
    upstreams: Arc<InMemoryUpstreamRepo>,
    routes: Arc<InMemoryRouteRepo>,
    reader: CachedProxyConfigReader,
}

fn fixture() -> Fixture {
    fixture_holding(100)
}

/// Sections of at most `max_entries`.
fn fixture_holding(max_entries: usize) -> Fixture {
    let cache = Arc::new(ProxyConfigCache::new(
        Duration::from_secs(60),
        max_entries,
        Arc::new(NoopMetrics),
    ));
    let upstreams = Arc::new(InMemoryUpstreamRepo::new());
    let routes = Arc::new(InMemoryRouteRepo::new());
    let reader = CachedProxyConfigReader::new(
        Arc::clone(&cache),
        Arc::clone(&upstreams) as Arc<dyn UpstreamRepository>,
        Arc::clone(&routes) as Arc<dyn RouteRepository>,
    );
    Fixture {
        cache,
        upstreams,
        routes,
        reader,
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
        tags: vec!["tagged".into()],
        managed_by: ManagedBy::Api,
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
        managed_by: ManagedBy::Api,
    }
}

fn row_key(upstream: &Upstream) -> RowKey {
    RowKey {
        tenant_id: upstream.tenant_id,
        id: upstream.id,
    }
}

fn paths(routes: &[Route]) -> Vec<&str> {
    routes
        .iter()
        .map(|r| r.match_rules.http.as_ref().unwrap().path.as_str())
        .collect()
}

#[tokio::test]
async fn upstreams_by_alias_serves_the_stored_candidates_until_a_change_evicts_them() {
    let f = fixture();
    let (parent, child) = (Uuid::new_v4(), Uuid::new_v4());
    let mut disabled = upstream(child, "a");
    disabled.enabled = false;
    let inherited = f.upstreams.create(upstream(parent, "a")).await.unwrap();
    f.upstreams.create(disabled.clone()).await.unwrap();

    let mut got = f
        .reader
        .upstreams_by_alias("a", &[child, parent])
        .await
        .unwrap()
        .to_vec();
    got.sort_by_key(|u| u.tenant_id == parent);
    let expected: Vec<Upstream> = [disabled.clone(), inherited]
        .into_iter()
        .map(|u| Upstream { tags: vec![], ..u })
        .collect();
    assert_eq!(
        got, expected,
        "every candidate, disabled ones too, without tags"
    );

    // A write the cache has not heard of yet is not seen.
    disabled.enabled = true;
    f.upstreams.update(disabled.clone()).await.unwrap();
    let got = f
        .reader
        .upstreams_by_alias("a", &[child, parent])
        .await
        .unwrap();
    assert!(
        got.iter().any(|u| u.id == disabled.id && !u.enabled),
        "stored"
    );

    f.cache.apply(&Change::Upstream {
        tenant_id: child,
        id: disabled.id,
        alias: "a".into(),
    });
    let got = f
        .reader
        .upstreams_by_alias("a", &[child, parent])
        .await
        .unwrap();
    assert!(
        got.iter().any(|u| u.id == disabled.id && u.enabled),
        "reloaded"
    );

    let none = f
        .reader
        .upstreams_by_alias("b", &[child, parent])
        .await
        .unwrap();
    assert!(none.is_empty(), "not found is an empty answer");
}

#[tokio::test]
async fn routes_of_answers_in_key_order_and_reloads_only_evicted_upstreams() {
    let f = fixture();
    let tenant = Uuid::new_v4();
    let (a, b, empty) = (
        upstream(tenant, "a"),
        upstream(tenant, "b"),
        upstream(tenant, "c"),
    );
    for u in [&a, &b, &empty] {
        f.upstreams.create(u.clone()).await.unwrap();
    }
    f.routes.create(route(&a, "/a1")).await.unwrap();
    f.routes.create(route(&b, "/b1")).await.unwrap();

    let keys = [row_key(&a), row_key(&b), row_key(&empty), row_key(&a)];
    let got = f.reader.routes_of(&keys).await.unwrap();
    let got: Vec<_> = got.iter().map(|routes| paths(routes)).collect();
    assert_eq!(got, vec![vec!["/a1"], vec!["/b1"], vec![], vec!["/a1"]]);
    assert!(f.reader.routes_of(&[]).await.unwrap().is_empty());

    let added = f.routes.create(route(&a, "/a2")).await.unwrap();
    f.routes.create(route(&b, "/b2")).await.unwrap();
    f.cache.apply(&Change::Route {
        tenant_id: tenant,
        id: added.id,
        upstream_id: a.id,
    });

    let got = f.reader.routes_of(&keys[..2]).await.unwrap();
    let got: Vec<_> = got.iter().map(|routes| paths(routes)).collect();
    let mut a_paths = got[0].clone();
    a_paths.sort_unstable();
    assert_eq!(a_paths, vec!["/a1", "/a2"], "a was evicted and reloaded");
    assert_eq!(got[1], vec!["/b1"], "b is still served from the cache");
}

/// ADR-0020: CACHE-HIT-004. Unknown aliases filling a full section do not
/// push out an alias that is read between them.
#[tokio::test]
async fn a_full_alias_section_keeps_an_alias_read_in_between() {
    let f = fixture_holding(2);
    let tenant = Uuid::new_v4();
    let hot = f.upstreams.create(upstream(tenant, "hot")).await.unwrap();
    let hot_key = AliasKey {
        alias: "hot".into(),
        chain: vec![tenant],
    };

    for n in 0..10 {
        let got = f.reader.upstreams_by_alias("hot", &[tenant]).await.unwrap();
        assert_eq!(got.iter().map(|u| u.id).collect::<Vec<_>>(), vec![hot.id]);
        let unknown = format!("unknown-{n}");
        let got = f
            .reader
            .upstreams_by_alias(&unknown, &[tenant])
            .await
            .unwrap();
        assert!(got.is_empty());
        assert!(f.cache.aliases.contains(&hot_key), "round {n}");
    }
    assert_eq!(f.cache.aliases.len(), 2);
    f.cache.aliases.assert_consistent();
}
