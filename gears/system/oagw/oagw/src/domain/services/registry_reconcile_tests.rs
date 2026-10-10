//! The registry reconcile against a Control Plane over in-memory or
//! scripted repositories: what each boot creates, updates, removes and keeps,
//! the write order, and replicas booting together. Boots of a real gear on a
//! database are in `gear_tests.rs`, which shares the fixtures here.

use std::sync::Arc;

use async_trait::async_trait;
use tracing_test::traced_test;
use uuid::Uuid;

use super::*;
use crate::domain::model::{
    Endpoint, HttpMatch, HttpMethod, MatchRules, PathSuffixMode, Route, Scheme, Server, Upstream,
};
use crate::domain::repo::{RepositoryError, RouteRepository, Tags, UpstreamRepository};
use crate::domain::services::{ControlPlaneService, ControlPlaneServiceImpl};
use crate::domain::ssrf::SsrfGuard;
use crate::domain::test_support::{
    MockCredStoreClient, MockTenantResolverClient, UnavailableStorage, allow_all_enforcer,
};
use crate::infra::storage::{InMemoryRouteRepo, InMemoryUpstreamRepo};

pub(crate) fn registry_instances(
    tenant_id: Uuid,
) -> (Vec<ProvisionedUpstream>, Vec<ProvisionedRoute>) {
    let upstream_id = Uuid::new_v4();
    let upstream = ProvisionedUpstream {
        tenant_id: Some(tenant_id),
        request: CreateUpstreamRequest {
            id: Some(upstream_id),
            server: Server {
                endpoints: vec![Endpoint {
                    scheme: Scheme::Https,
                    host: "api.example.com".into(),
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
    };
    let route = ProvisionedRoute {
        tenant_id: Some(tenant_id),
        request: CreateRouteRequest {
            id: Some(Uuid::new_v4()),
            upstream_id,
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
        },
    };
    (vec![upstream], vec![route])
}

pub(crate) const NONE: Tally = Tally {
    created: 0,
    updated: 0,
    unchanged: 0,
    removed: 0,
    kept: 0,
};

pub(crate) const CREATED_ALL: ProvisioningCounts = ProvisioningCounts {
    upstreams: Tally { created: 1, ..NONE },
    routes: Tally { created: 1, ..NONE },
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
};

fn control_plane_over(
    upstreams: Arc<dyn UpstreamRepository>,
    routes: Arc<dyn RouteRepository>,
) -> ControlPlaneServiceImpl {
    ControlPlaneServiceImpl::new(
        upstreams,
        routes,
        Arc::new(MockTenantResolverClient::single_tenant()),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::empty()),
        Arc::new(SsrfGuard::disabled()),
    )
}

pub(crate) fn in_memory_control_plane() -> ControlPlaneServiceImpl {
    control_plane_over(
        Arc::new(InMemoryUpstreamRepo::new()),
        Arc::new(InMemoryRouteRepo::new()),
    )
}

#[tokio::test]
#[traced_test]
async fn provisioning_twice_leaves_existing_instances_unchanged() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);

    let first = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    assert_eq!(first, CREATED_ALL);

    let second = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    assert_eq!(second, UNCHANGED_ALL);
    assert!(!logs_contain("Updated"));
}

/// Registry content changed since the stored rows were provisioned: the
/// registry wins, and the log names what changed.
#[tokio::test]
#[traced_test]
async fn provisioning_applies_changed_registry_content() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (mut upstreams, mut routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();

    upstreams[0].request.tags = vec!["new".into()];
    routes[0].request.priority = 7;
    let counts = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    assert_eq!(
        counts,
        ProvisioningCounts {
            upstreams: Tally { updated: 1, ..NONE },
            routes: Tally { updated: 1, ..NONE },
        }
    );
    let upstream = cp
        .get_upstream(&ctx, upstreams[0].request.id.unwrap())
        .await
        .unwrap();
    let route = cp
        .get_route(&ctx, routes[0].request.id.unwrap())
        .await
        .unwrap();
    assert_eq!(upstream.tags, vec!["new".to_owned()]);
    assert_eq!(route.priority, 7);
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
}

/// A registry upstream whose endpoints now derive another alias takes it in
/// place, by the create rules: its ID and its routes, API routes included,
/// stay. The Management API keeps rejecting an alias change.
#[tokio::test]
async fn provisioning_changes_the_alias_of_a_registry_upstream() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (mut upstreams, routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    let upstream_id = upstreams[0].request.id.unwrap();
    let mut api_route = routes[0].request.clone();
    api_route.id = None;
    api_route.match_rules.http.as_mut().unwrap().path = "/user".into();
    let api_route = cp.create_route(&ctx, api_route).await.unwrap();

    upstreams[0].request.server.endpoints[0].host = "api.other.com".into();
    let counts = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .expect("a registry upstream may change its alias");
    assert_eq!(
        counts,
        ProvisioningCounts {
            upstreams: Tally { updated: 1, ..NONE },
            routes: Tally {
                unchanged: 1,
                ..NONE
            },
        }
    );
    assert_eq!(
        cp.get_upstream(&ctx, upstream_id).await.unwrap().alias,
        "api.other.com"
    );
    assert_eq!(
        cp.get_route(&ctx, api_route.id).await.unwrap().upstream_id,
        upstream_id
    );
    assert_eq!(
        reconcile_registry(&cp, &upstreams, &routes, root)
            .await
            .unwrap(),
        UNCHANGED_ALL
    );

    // hostname → IP, which needs the explicit alias create would need.
    upstreams[0].request.server.endpoints[0].host = "10.0.0.1".into();
    upstreams[0].request.alias = Some("internal-api".into());
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .expect("a registry upstream may move to IP endpoints");
    assert_eq!(
        cp.get_upstream(&ctx, upstream_id).await.unwrap().alias,
        "internal-api"
    );

    // An API upstream still keeps its alias.
    let mut api_upstream = upstreams[0].request.clone();
    api_upstream.id = None;
    api_upstream.alias = Some("api-owned".into());
    let api_upstream = cp.create_upstream(&ctx, api_upstream).await.unwrap();
    let mut renamed = upstream_update(&upstreams[0].request);
    renamed.alias = Some("api-renamed".into());
    let err = cp
        .update_upstream(&ctx, api_upstream.id, renamed)
        .await
        .expect_err("the Management API cannot change an alias");
    assert!(err.to_string().contains("alias cannot be changed"), "{err}");
}

/// A new instance takes over the alias a stored one gives up, in one boot,
/// wherever it sits in the registry's list.
#[tokio::test]
async fn provisioning_hands_an_alias_to_a_new_upstream() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (mut upstreams, routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();

    let mut successor = upstreams[0].clone();
    let successor_id = Uuid::new_v4();
    successor.request.id = Some(successor_id);
    upstreams[0].request.server.endpoints[0].host = "api.other.com".into();
    upstreams.insert(0, successor);
    let counts = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .expect("the renamed upstream frees its alias first");
    assert_eq!(
        counts.upstreams,
        Tally {
            created: 1,
            updated: 1,
            ..NONE
        }
    );
    assert_eq!(
        cp.get_upstream(&ctx, successor_id).await.unwrap().alias,
        "api.example.com"
    );
}

/// A stored instance cannot take the alias of an upstream the removal phase
/// kept for its API routes; startup fails naming those routes.
#[tokio::test]
async fn provisioning_names_the_routes_that_keep_an_alias_an_update_wants() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (mut upstreams, routes) = registry_instances(root);
    let mut other = upstreams[0].clone();
    other.request.id = Some(Uuid::new_v4());
    other.request.server.endpoints[0].host = "api.other.com".into();
    upstreams.push(other);
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    let holder = upstreams[0].request.id.unwrap();
    let mut api_route = routes[0].request.clone();
    api_route.id = None;
    api_route.match_rules.http.as_mut().unwrap().path = "/user".into();
    let api_route = cp.create_route(&ctx, api_route).await.unwrap();

    let mut other = upstreams.remove(1);
    other.request.server.endpoints[0].host = "api.example.com".into();
    let err = reconcile_registry(&cp, &[other], &[], root)
        .await
        .expect_err("a kept upstream holds the alias")
        .to_string();
    assert!(
        err.contains(&format!("is still held by upstream {holder}")),
        "{err}"
    );
    assert!(err.contains(&api_route.id.to_string()), "{err}");
}

/// A route that moves to another upstream is replaced under the same ID.
#[tokio::test]
async fn provisioning_replaces_a_route_that_moved_to_another_upstream() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (mut upstreams, mut routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();

    let mut other = upstreams[0].clone();
    let other_id = Uuid::new_v4();
    other.request.id = Some(other_id);
    other.request.alias = Some("other.example.com".into());
    other.request.server.endpoints[0].host = "other.example.com".into();
    upstreams.push(other);
    routes[0].request.upstream_id = other_id;

    let counts = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    assert_eq!(
        counts,
        ProvisioningCounts {
            upstreams: Tally {
                created: 1,
                unchanged: 1,
                ..NONE
            },
            routes: Tally { updated: 1, ..NONE },
        }
    );
    let route = cp
        .get_route(&ctx, routes[0].request.id.unwrap())
        .await
        .unwrap();
    assert_eq!(route.upstream_id, other_id);
    assert_eq!(route.managed_by, ManagedBy::Registry);
}

/// Instances gone from the registry are removed, routes and upstreams.
#[tokio::test]
async fn provisioning_removes_instances_the_registry_no_longer_has() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (upstreams, routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();

    let counts = reconcile_registry(&cp, &[], &[], root).await.unwrap();
    assert_eq!(
        counts,
        ProvisioningCounts {
            upstreams: Tally { removed: 1, ..NONE },
            routes: Tally { removed: 1, ..NONE },
        }
    );
    let upstream_id = upstreams[0].request.id.unwrap();
    assert!(matches!(
        cp.get_upstream(&ctx, upstream_id).await,
        Err(DomainError::NotFound { .. })
    ));
    assert!(matches!(
        cp.get_route(&ctx, routes[0].request.id.unwrap()).await,
        Err(DomainError::NotFound { .. })
    ));
}

/// A removed upstream that an API route still uses is kept, with a warning;
/// the API route survives.
#[tokio::test]
#[traced_test]
async fn provisioning_keeps_a_removed_upstream_that_api_routes_use() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (upstreams, routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    let upstream_id = upstreams[0].request.id.unwrap();
    let mut api_route = routes[0].request.clone();
    api_route.id = None;
    api_route.match_rules.http.as_mut().unwrap().path = "/user".into();
    let api_route = cp.create_route(&ctx, api_route).await.unwrap();

    let counts = reconcile_registry(&cp, &[], &[], root).await.unwrap();
    assert_eq!(
        counts,
        ProvisioningCounts {
            upstreams: Tally { kept: 1, ..NONE },
            routes: Tally { removed: 1, ..NONE },
        }
    );
    assert!(logs_contain("routes still use it"));
    assert!(cp.get_upstream(&ctx, upstream_id).await.is_ok());
    assert_eq!(
        cp.get_route(&ctx, api_route.id).await.unwrap().managed_by,
        ManagedBy::Api
    );
}

/// An instance that moves to another tenant is re-created there.
#[tokio::test]
async fn provisioning_moves_an_instance_to_its_new_tenant() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let elsewhere = Uuid::new_v4();
    let (mut upstreams, mut routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();

    upstreams[0].tenant_id = Some(elsewhere);
    routes[0].tenant_id = Some(elsewhere);
    let counts = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    let moved = Tally {
        created: 1,
        removed: 1,
        ..NONE
    };
    assert_eq!(
        counts,
        ProvisioningCounts {
            upstreams: moved,
            routes: moved,
        }
    );
    let upstream_id = upstreams[0].request.id.unwrap();
    let ctx = provisioning_ctx(elsewhere).unwrap();
    assert!(cp.get_upstream(&ctx, upstream_id).await.is_ok());
    let ctx = provisioning_ctx(root).unwrap();
    assert!(cp.get_upstream(&ctx, upstream_id).await.is_err());
}

/// A boot that stopped after the upstreams creates only the routes.
#[tokio::test]
async fn provisioning_after_a_partial_boot_creates_only_what_is_missing() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &[], root)
        .await
        .unwrap();

    let counts = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    assert_eq!(
        counts,
        ProvisioningCounts {
            upstreams: Tally {
                unchanged: 1,
                ..NONE
            },
            routes: Tally { created: 1, ..NONE },
        }
    );
}

/// A second registry route on the first upstream of `registry_instances`.
pub(crate) fn sibling_route(of: &ProvisionedRoute, path: &str) -> ProvisionedRoute {
    let mut route = of.clone();
    route.request.id = Some(Uuid::new_v4());
    route.request.match_rules.http.as_mut().unwrap().path = path.into();
    route
}

pub(crate) fn set_path(route: &mut ProvisionedRoute, path: &str) {
    set_path_of(&mut route.request, path);
}

fn set_path_of(route: &mut CreateRouteRequest, path: &str) {
    route.match_rules.http.as_mut().unwrap().path = path.into();
}

/// The upstream gets a new ID and keeps its alias, and the route follows it:
/// the old upstream is freed in the removal phase, so the new one can take
/// the alias.
#[tokio::test]
#[traced_test]
async fn provisioning_moves_a_route_to_a_replacement_upstream_with_the_same_alias() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (mut upstreams, mut routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    let old_id = upstreams[0].request.id.unwrap();
    let new_id = Uuid::new_v4();
    upstreams[0].request.id = Some(new_id);
    routes[0].request.upstream_id = new_id;

    let counts = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .expect("the old upstream no longer holds the alias");
    assert_eq!(
        counts,
        ProvisioningCounts {
            upstreams: Tally {
                created: 1,
                removed: 1,
                ..NONE
            },
            routes: Tally { updated: 1, ..NONE },
        }
    );
    let moved = (format!("from={old_id}"), format!("to={new_id}"));
    logs_assert(|lines| {
        lines
            .iter()
            .any(|l| {
                l.contains("Replacing route moved to another upstream")
                    && l.contains(&moved.0)
                    && l.contains(&moved.1)
            })
            .then_some(())
            .ok_or_else(|| "no replacement line names both upstreams".to_owned())
    });
    assert!(!logs_contain("Removed route"));
    let route = cp
        .get_route(&ctx, routes[0].request.id.unwrap())
        .await
        .unwrap();
    assert_eq!(route.upstream_id, new_id);
    assert!(cp.get_upstream(&ctx, old_id).await.is_err());
    assert_eq!(
        reconcile_registry(&cp, &upstreams, &routes, root)
            .await
            .unwrap(),
        UNCHANGED_ALL
    );
}

/// The same replacement while an API route still uses the old upstream: the
/// old upstream is kept, by policy, so the new one cannot take its alias, and
/// startup fails naming the route that holds it.
#[tokio::test]
async fn provisioning_names_the_routes_that_keep_an_alias_in_use() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (mut upstreams, mut routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    let old_id = upstreams[0].request.id.unwrap();
    let mut api_route = routes[0].request.clone();
    api_route.id = None;
    set_path_of(&mut api_route, "/user");
    let api_route = cp.create_route(&ctx, api_route).await.unwrap();
    let new_id = Uuid::new_v4();
    upstreams[0].request.id = Some(new_id);
    routes[0].request.upstream_id = new_id;

    let err = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .expect_err("the kept upstream holds the alias")
        .to_string();
    assert!(
        err.contains(&format!(
            "Failed to provision upstream {new_id} (tenant={root}): alias 'api.example.com' \
             is still held by upstream {old_id}"
        )),
        "{err}"
    );
    assert!(
        err.contains(&format!("routes still use ({})", api_route.id)),
        "{err}"
    );
}

/// Two registry routes swap paths: neither write conflicts with the other's
/// stored row.
#[tokio::test]
async fn provisioning_swaps_paths_between_registry_routes() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let (upstreams, mut routes) = registry_instances(root);
    routes.push(sibling_route(&routes[0], "/v2"));
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();

    set_path(&mut routes[0], "/v2");
    set_path(&mut routes[1], "/v1");
    let counts = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .expect("a swap is a consistent final state");
    assert_eq!(counts.routes, Tally { updated: 2, ..NONE });
    let counts = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    assert_eq!(
        counts.routes,
        Tally {
            unchanged: 2,
            ..NONE
        }
    );
}

/// R1 leaves `/v1` for `/v3` and R2 takes `/v1`, in either listing order.
#[tokio::test]
async fn provisioning_hands_a_path_from_one_registry_route_to_another() {
    for r2_first in [false, true] {
        let cp = in_memory_control_plane();
        let root = Uuid::new_v4();
        let (upstreams, mut routes) = registry_instances(root);
        routes.push(sibling_route(&routes[0], "/v2"));
        reconcile_registry(&cp, &upstreams, &routes, root)
            .await
            .unwrap();

        set_path(&mut routes[0], "/v3");
        set_path(&mut routes[1], "/v1");
        if r2_first {
            routes.reverse();
        }
        let counts = reconcile_registry(&cp, &upstreams, &routes, root)
            .await
            .unwrap_or_else(|e| panic!("r2_first={r2_first}: {e}"));
        assert_eq!(counts.routes, Tally { updated: 2, ..NONE });
    }
}

/// A new registry route takes the path an existing one leaves, listed first.
#[tokio::test]
async fn provisioning_creates_a_route_at_a_path_another_registry_route_leaves() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let (upstreams, mut routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();

    set_path(&mut routes[0], "/v2");
    let newcomer = sibling_route(&routes[0], "/v1");
    routes.insert(0, newcomer);
    let counts = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .expect("the new route's path is free in the final state");
    assert_eq!(
        counts.routes,
        Tally {
            created: 1,
            updated: 1,
            ..NONE
        }
    );
}

/// Registry content whose own routes overlap fails startup before any write.
#[tokio::test]
async fn provisioning_rejects_registry_routes_that_overlap_each_other() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (upstreams, mut routes) = registry_instances(root);
    let first = routes[0].request.id.unwrap();
    let mut twin = sibling_route(&routes[0], "/v1");
    twin.request.match_rules.http.as_mut().unwrap().methods =
        vec![HttpMethod::Get, HttpMethod::Post];
    let second = twin.request.id.unwrap();
    routes.push(twin);

    let err = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .expect_err("overlapping registry routes fail startup")
        .to_string();
    let (a, b) = if first < second {
        (first, second)
    } else {
        (second, first)
    };
    assert!(
        err.contains(&format!("types-registry routes {a} and {b} overlap")),
        "{err}"
    );
    assert!(err.contains("path '/v1', priority 0, method Post"), "{err}");
    assert!(
        cp.list_upstreams(&ctx, &ListQuery { top: 10, skip: 0 })
            .await
            .unwrap()
            .is_empty(),
        "nothing was written"
    );

    // Not an overlap: another method, priority, or a disabled twin.
    let mut fine = routes.clone();
    fine[1].request.match_rules.http.as_mut().unwrap().methods = vec![HttpMethod::Get];
    reconcile_registry(&in_memory_control_plane(), &upstreams, &fine, root)
        .await
        .unwrap();
    let mut fine = routes.clone();
    fine[1].request.priority = 1;
    reconcile_registry(&in_memory_control_plane(), &upstreams, &fine, root)
        .await
        .unwrap();
    let mut fine = routes;
    fine[1].request.enabled = false;
    reconcile_registry(&in_memory_control_plane(), &upstreams, &fine, root)
        .await
        .unwrap();
}

/// Registry routes still conflict with API routes, in both directions.
#[tokio::test]
async fn registry_and_api_routes_still_conflict() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (upstreams, routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &[], root)
        .await
        .unwrap();
    let mut api_route = routes[0].request.clone();
    api_route.id = None;
    let api_route = cp.create_route(&ctx, api_route).await.unwrap();

    let err = reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .expect_err("a registry route may not overlap an API route")
        .to_string();
    assert!(err.contains("route overlap"), "{err}");

    cp.delete_route(&ctx, api_route.id).await.unwrap();
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    let mut api_route = routes[0].request.clone();
    api_route.id = None;
    assert!(matches!(
        cp.create_route(&ctx, api_route).await,
        Err(DomainError::Conflict { .. })
    ));
}

/// An IP upstream needs an explicit alias; dropping it from the registry
/// fails startup on an existing store as on a fresh one, instead of reading
/// as drift on every boot.
#[tokio::test]
async fn provisioning_fails_when_a_stored_instance_becomes_invalid() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let (mut upstreams, _) = registry_instances(root);
    upstreams[0].request.server.endpoints[0].host = "10.0.0.1".into();
    upstreams[0].request.alias = Some("my-ip".into());
    reconcile_registry(&cp, &upstreams, &[], root)
        .await
        .unwrap();

    upstreams[0].request.alias = None;
    let id = upstreams[0].request.id.unwrap();
    let err = reconcile_registry(&cp, &upstreams, &[], root)
        .await
        .expect_err("an invalid instance fails provisioning")
        .to_string();
    assert!(
        err.contains(&format!(
            "Failed to provision upstream {id} (tenant={root})"
        )),
        "{err}"
    );
    assert!(err.contains("explicit alias is required"), "{err}");
}

/// What one `get_by_id` of [`RouteRace`] returns.
enum Lookup {
    Missing,
    Stale(Box<Route>),
}

/// A route store that answers `get_by_id` from a script and can hide its
/// rows from `list_registry_keys`, to model another replica writing between
/// this replica's reads. Once the script runs out, it answers from `inner`.
struct RouteRace {
    inner: Arc<InMemoryRouteRepo>,
    hide_from_listing: bool,
    lookups: std::sync::Mutex<std::collections::VecDeque<Lookup>>,
}

impl RouteRace {
    fn new(inner: Arc<InMemoryRouteRepo>, hide_from_listing: bool, lookups: Vec<Lookup>) -> Self {
        Self {
            inner,
            hide_from_listing,
            lookups: std::sync::Mutex::new(lookups.into()),
        }
    }
}

#[async_trait]
impl RouteRepository for RouteRace {
    async fn create(&self, route: Route) -> Result<Route, RepositoryError> {
        self.inner.create(route).await
    }
    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Route, RepositoryError> {
        let next = self.lookups.lock().unwrap().pop_front();
        match next {
            None => self.inner.get_by_id(tenant_id, id).await,
            Some(Lookup::Missing) => Err(RepositoryError::NotFound {
                entity: "route",
                id,
            }),
            Some(Lookup::Stale(route)) => Ok(*route),
        }
    }
    async fn list(
        &self,
        tenant_id: Uuid,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, RepositoryError> {
        self.inner.list(tenant_id, upstream_id, query).await
    }
    async fn find_matching_in_tenants(
        &self,
        tenant_chain: &[Uuid],
        upstream_ids: &[Uuid],
        method: &str,
        path: &str,
        tags: Tags,
    ) -> Result<Route, RepositoryError> {
        self.inner
            .find_matching_in_tenants(tenant_chain, upstream_ids, method, path, tags)
            .await
    }
    async fn update(&self, route: Route) -> Result<Route, RepositoryError> {
        self.inner.update(route).await
    }
    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        self.inner.delete(tenant_id, id).await
    }
    async fn delete_by_upstream(
        &self,
        tenant_id: Uuid,
        upstream_id: Uuid,
    ) -> Result<(), RepositoryError> {
        self.inner.delete_by_upstream(tenant_id, upstream_id).await
    }
    async fn list_registry_keys(&self) -> Result<Vec<RowKey>, RepositoryError> {
        if self.hide_from_listing {
            return Ok(vec![]);
        }
        self.inner.list_registry_keys().await
    }
}

/// Replicas booting together: the route did not exist when this replica
/// listed and looked it up, and another replica created it before this
/// replica's create.
#[tokio::test]
async fn provisioning_accepts_a_route_another_replica_just_created() {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let upstream_store: Arc<dyn UpstreamRepository> = Arc::new(InMemoryUpstreamRepo::new());
    let route_store = Arc::new(InMemoryRouteRepo::new());
    let winner = control_plane_over(upstream_store.clone(), route_store.clone());
    reconcile_registry(&winner, &upstreams, &routes, root)
        .await
        .unwrap();

    let loser = control_plane_over(
        upstream_store,
        Arc::new(RouteRace::new(route_store, true, vec![Lookup::Missing])),
    );
    let counts = reconcile_registry(&loser, &upstreams, &routes, root)
        .await
        .expect("a concurrent create is not a failure");
    assert_eq!(counts, UNCHANGED_ALL);
}

/// Replicas booting together: a route another replica removed between this
/// replica's listing and its delete counts as removed by that replica.
#[tokio::test]
async fn provisioning_accepts_a_route_another_replica_just_removed() {
    let root = Uuid::new_v4();
    let (upstreams, routes) = registry_instances(root);
    let upstream_store: Arc<dyn UpstreamRepository> = Arc::new(InMemoryUpstreamRepo::new());
    let route_store = Arc::new(InMemoryRouteRepo::new());
    let winner = control_plane_over(upstream_store.clone(), route_store.clone());
    reconcile_registry(&winner, &upstreams, &routes, root)
        .await
        .unwrap();

    let loser = control_plane_over(
        upstream_store,
        Arc::new(RouteRace::new(route_store, false, vec![Lookup::Missing])),
    );
    let counts = reconcile_registry(&loser, &upstreams, &[], root)
        .await
        .expect("a concurrent delete is not a failure");
    assert_eq!(counts.routes, NONE);
}

/// Replicas booting together on a route move: this replica saw the route on
/// its old upstream, and another replica deleted it and re-created it on the
/// new upstream in between.
#[tokio::test]
async fn provisioning_accepts_a_route_another_replica_just_moved() {
    let root = Uuid::new_v4();
    let (mut upstreams, mut routes) = registry_instances(root);
    let old = routes[0].request.upstream_id;
    let mut other = upstreams[0].clone();
    let new_id = Uuid::new_v4();
    other.request.id = Some(new_id);
    other.request.server.endpoints[0].host = "other.example.com".into();
    upstreams.push(other);
    routes[0].request.upstream_id = new_id;

    let upstream_store: Arc<dyn UpstreamRepository> = Arc::new(InMemoryUpstreamRepo::new());
    let route_store = Arc::new(InMemoryRouteRepo::new());
    let winner = control_plane_over(upstream_store.clone(), route_store.clone());
    reconcile_registry(&winner, &upstreams, &routes, root)
        .await
        .unwrap();
    let ctx = provisioning_ctx(root).unwrap();
    let mut stale = winner
        .get_route(&ctx, routes[0].request.id.unwrap())
        .await
        .unwrap();
    stale.upstream_id = old;

    let loser = control_plane_over(
        upstream_store,
        Arc::new(RouteRace::new(
            route_store,
            true,
            vec![Lookup::Stale(Box::new(stale)), Lookup::Missing],
        )),
    );
    let counts = reconcile_registry(&loser, &upstreams, &routes, root)
        .await
        .expect("a concurrent move is not a failure");
    assert_eq!(
        counts.routes,
        Tally {
            unchanged: 1,
            ..NONE
        }
    );
}

/// The row a replica finds after its re-create conflicts is accepted only
/// when it is the registry's route as wanted: other content, or an API row
/// with the ID, fails startup.
#[tokio::test]
async fn provisioning_fails_when_a_moved_route_conflicts_with_another_row() {
    for api_row in [false, true] {
        let root = Uuid::new_v4();
        let ctx = provisioning_ctx(root).unwrap();
        let (mut upstreams, mut routes) = registry_instances(root);
        let old = routes[0].request.upstream_id;
        let mut other = upstreams[0].clone();
        let new_id = Uuid::new_v4();
        other.request.id = Some(new_id);
        other.request.server.endpoints[0].host = "other.example.com".into();
        upstreams.push(other);
        routes[0].request.upstream_id = new_id;

        let upstream_store: Arc<dyn UpstreamRepository> = Arc::new(InMemoryUpstreamRepo::new());
        let route_store = Arc::new(InMemoryRouteRepo::new());
        let racer = control_plane_over(upstream_store.clone(), route_store.clone());
        reconcile_registry(&racer, &upstreams, &[], root)
            .await
            .unwrap();
        let mut found = routes[0].request.clone();
        if api_row {
            racer.create_route(&ctx, found).await.unwrap();
        } else {
            found.priority = 5;
            racer.create_registry_route(&ctx, found).await.unwrap();
        }
        let mut stale = racer
            .get_route(&ctx, routes[0].request.id.unwrap())
            .await
            .unwrap();
        stale.upstream_id = old;
        stale.managed_by = ManagedBy::Registry;

        let cp = control_plane_over(
            upstream_store,
            Arc::new(RouteRace::new(
                route_store,
                true,
                vec![Lookup::Stale(Box::new(stale)), Lookup::Missing],
            )),
        );
        let err = reconcile_registry(&cp, &upstreams, &routes, root)
            .await
            .expect_err("only the wanted route is accepted")
            .to_string();
        assert!(
            err.contains(&format!(
                "Failed to provision route {}",
                routes[0].request.id.unwrap()
            )),
            "api_row={api_row}: {err}"
        );
        assert!(err.contains("already exists"), "api_row={api_row}: {err}");
    }
}

/// Hides every upstream from the first `get_by_id`, as if another replica
/// created it just after that lookup.
struct CreatedAfterFirstLookup {
    inner: Arc<InMemoryUpstreamRepo>,
    looked_up: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl UpstreamRepository for CreatedAfterFirstLookup {
    async fn create(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        self.inner.create(upstream).await
    }
    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Upstream, RepositoryError> {
        if self
            .looked_up
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.inner.get_by_id(tenant_id, id).await
        } else {
            Err(RepositoryError::NotFound {
                entity: "upstream",
                id,
            })
        }
    }
    async fn list(
        &self,
        tenant_id: Uuid,
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        self.inner.list(tenant_id, query).await
    }
    async fn update(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        self.inner.update(upstream).await
    }
    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        self.inner.delete(tenant_id, id).await
    }
    async fn list_by_alias_for_tenants(
        &self,
        alias: &str,
        tenant_ids: &std::collections::HashSet<Uuid>,
        tags: Tags,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        self.inner
            .list_by_alias_for_tenants(alias, tenant_ids, tags)
            .await
    }
    async fn list_registry_keys(&self) -> Result<Vec<RowKey>, RepositoryError> {
        self.inner.list_registry_keys().await
    }
}

/// Replicas booting together: the loser's create conflicts with the
/// winner's row, and the instance is skipped instead of failing startup.
#[tokio::test]
async fn provisioning_accepts_an_instance_another_replica_just_created() {
    let root = Uuid::new_v4();
    let (upstreams, _) = registry_instances(root);
    let store = Arc::new(InMemoryUpstreamRepo::new());
    let routes: Arc<dyn RouteRepository> = Arc::new(InMemoryRouteRepo::new());
    let winner = control_plane_over(store.clone(), routes.clone());
    reconcile_registry(&winner, &upstreams, &[], root)
        .await
        .unwrap();

    let loser = control_plane_over(
        Arc::new(CreatedAfterFirstLookup {
            inner: store,
            looked_up: false.into(),
        }),
        routes,
    );
    let counts = reconcile_registry(&loser, &upstreams, &[], root)
        .await
        .expect("a concurrent create is not a failure");
    assert_eq!(
        counts.upstreams,
        Tally {
            unchanged: 1,
            ..NONE
        }
    );
}

/// A conflict with an API upstream (here the alias) fails startup and names
/// the instance.
#[tokio::test]
async fn provisioning_fails_when_a_create_conflicts_with_an_api_upstream() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let (upstreams, _) = registry_instances(root);
    let mut same_alias = upstreams[0].request.clone();
    same_alias.id = None;
    cp.create_upstream(&provisioning_ctx(root).unwrap(), same_alias)
        .await
        .unwrap();

    let id = upstreams[0].request.id.unwrap();
    let err = reconcile_registry(&cp, &upstreams, &[], root)
        .await
        .expect_err("an alias clash fails provisioning")
        .to_string();
    assert!(
        err.contains(&format!(
            "Failed to provision upstream {id} (tenant={root})"
        )),
        "{err}"
    );
}

/// Storage that fails is not "nothing stored": provisioning stops instead of
/// removing or creating.
#[tokio::test]
async fn provisioning_fails_when_storage_fails() {
    let cp = control_plane_over(Arc::new(UnavailableStorage), Arc::new(UnavailableStorage));
    let root = Uuid::new_v4();
    let (upstreams, _) = registry_instances(root);

    let err = reconcile_registry(&cp, &upstreams, &[], root)
        .await
        .expect_err("a storage failure fails provisioning")
        .to_string();
    assert!(
        err.contains("Failed to list registry-managed routes"),
        "{err}"
    );
}

/// A lookup that fails is not "absent": provisioning stops instead of
/// creating.
#[tokio::test]
async fn provisioning_fails_when_a_lookup_fails() {
    let root = Uuid::new_v4();
    let (upstreams, _) = registry_instances(root);
    let id = upstreams[0].request.id.unwrap();
    let cp = control_plane_over(
        Arc::new(LookupFails(InMemoryUpstreamRepo::new())),
        Arc::new(InMemoryRouteRepo::new()),
    );

    let err = reconcile_registry(&cp, &upstreams, &[], root)
        .await
        .expect_err("a failed lookup fails provisioning")
        .to_string();
    assert!(
        err.contains(&format!("Failed to look up upstream {id} (tenant={root})")),
        "{err}"
    );
}

/// An upstream store whose `get_by_id` always fails.
struct LookupFails(InMemoryUpstreamRepo);

#[async_trait]
impl UpstreamRepository for LookupFails {
    async fn create(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        self.0.create(upstream).await
    }
    async fn get_by_id(&self, _: Uuid, _: Uuid) -> Result<Upstream, RepositoryError> {
        Err(RepositoryError::Internal("storage unavailable".into()))
    }
    async fn list(
        &self,
        tenant_id: Uuid,
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        self.0.list(tenant_id, query).await
    }
    async fn update(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        self.0.update(upstream).await
    }
    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        self.0.delete(tenant_id, id).await
    }
    async fn list_by_alias_for_tenants(
        &self,
        alias: &str,
        tenant_ids: &std::collections::HashSet<Uuid>,
        tags: Tags,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        self.0
            .list_by_alias_for_tenants(alias, tenant_ids, tags)
            .await
    }
    async fn list_registry_keys(&self) -> Result<Vec<RowKey>, RepositoryError> {
        self.0.list_registry_keys().await
    }
}

/// A registry instance whose ID an API row holds is never overwritten.
#[tokio::test]
async fn provisioning_refuses_to_take_over_an_api_row() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let (upstreams, _) = registry_instances(root);
    let id = upstreams[0].request.id.unwrap();
    // Only an in-process caller can choose an ID; this one goes through the API path.
    cp.create_upstream(
        &provisioning_ctx(root).unwrap(),
        upstreams[0].request.clone(),
    )
    .await
    .unwrap();

    let err = reconcile_registry(&cp, &upstreams, &[], root)
        .await
        .expect_err("an API row is not taken over")
        .to_string();
    assert!(
        err.contains(&format!("upstream {id} (tenant={root})")),
        "{err}"
    );
    assert!(err.contains("created through the Management API"), "{err}");
}

/// A registry route that names an API upstream fails the boot that would
/// create it. Accepted, it would be deleted with that upstream through the
/// API, and every later boot would fail to re-create it.
#[tokio::test]
async fn provisioning_rejects_a_route_on_an_api_upstream() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let (upstreams, mut routes) = registry_instances(root);
    let mut api_upstream = upstreams[0].request.clone();
    api_upstream.id = None;
    let api_upstream = cp
        .create_upstream(&provisioning_ctx(root).unwrap(), api_upstream)
        .await
        .unwrap();
    routes[0].request.upstream_id = api_upstream.id;

    let id = routes[0].request.id.unwrap();
    let err = reconcile_registry(&cp, &[], &routes, root)
        .await
        .expect_err("a registry route needs a registry upstream")
        .to_string();
    assert!(
        err.contains(&format!("Failed to provision route {id} (tenant={root})")),
        "{err}"
    );
    assert!(err.contains("must name a types-registry upstream"), "{err}");
}

// ---------------------------------------------------------------------------
// Registry-managed rows are read-only through the Management API
// ---------------------------------------------------------------------------

#[track_caller]
fn assert_registry_managed(result: Result<(), DomainError>, entity: &str, id: Uuid) {
    match result {
        Err(DomainError::RegistryManaged { entity: e, id: i }) => assert_eq!((e, i), (entity, id)),
        other => panic!("expected RegistryManaged for {entity} {id}, got {other:?}"),
    }
}

#[tokio::test]
async fn api_writes_to_registry_rows_are_rejected() {
    let cp = in_memory_control_plane();
    let root = Uuid::new_v4();
    let ctx = provisioning_ctx(root).unwrap();
    let (upstreams, routes) = registry_instances(root);
    reconcile_registry(&cp, &upstreams, &routes, root)
        .await
        .unwrap();
    let upstream_id = upstreams[0].request.id.unwrap();
    let route_id = routes[0].request.id.unwrap();
    assert_registry_managed(
        cp.update_upstream(&ctx, upstream_id, upstream_update(&upstreams[0].request))
            .await
            .map(drop),
        "upstream",
        upstream_id,
    );
    assert_registry_managed(
        cp.delete_upstream(&ctx, upstream_id).await,
        "upstream",
        upstream_id,
    );
    assert_registry_managed(
        cp.update_route(&ctx, route_id, route_update(&routes[0].request))
            .await
            .map(drop),
        "route",
        route_id,
    );
    assert_registry_managed(cp.delete_route(&ctx, route_id).await, "route", route_id);

    // Nothing changed, and an API route may still attach to the upstream.
    assert_eq!(
        reconcile_registry(&cp, &upstreams, &routes, root)
            .await
            .unwrap(),
        UNCHANGED_ALL
    );
    let mut api_route = routes[0].request.clone();
    api_route.id = None;
    api_route.match_rules.http.as_mut().unwrap().path = "/user".into();
    let api_route = cp.create_route(&ctx, api_route).await.unwrap();
    assert_eq!(api_route.managed_by, ManagedBy::Api);
    cp.delete_route(&ctx, api_route.id).await.unwrap();
}
