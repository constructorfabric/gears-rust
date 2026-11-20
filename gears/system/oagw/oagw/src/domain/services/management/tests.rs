use std::sync::Arc;

use crate::domain::model::{
    Endpoint, GrpcMatch, HttpMatch, HttpMethod, MatchRules, PathSuffixMode, ProxyTarget, Scheme,
    Server,
};
use crate::domain::repo::{RowKey, Tags};
use crate::domain::services::registry_reconcile::tests::{
    Backend, CachedPlanes, Failed, Resolved, Stores, in_memory_planes, is_disabled, is_not_found,
};
use crate::domain::services::{RegistryProvisioner, UpstreamCheck};
use crate::domain::ssrf::{SsrfGuard, SsrfPolicy};

use super::*;
use crate::domain::test_support::{
    MockCredStoreClient, MockTenantResolverClient, UnavailableStorage, allow_all_enforcer,
};
use crate::infra::storage::{InMemoryRouteRepo, InMemoryUpstreamRepo};
use oagw_sdk::HTTP_PROTOCOL_ID;
use tenant_resolver_sdk::TenantId;

fn make_service() -> ControlPlaneServiceImpl {
    ControlPlaneServiceImpl::new(
        Arc::new(InMemoryUpstreamRepo::new()),
        Arc::new(InMemoryRouteRepo::new()),
        Arc::new(MockTenantResolverClient::single_tenant()),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::empty()),
        Arc::new(SsrfGuard::disabled()),
        None,
    )
}

fn make_service_with_resolver(resolver: MockTenantResolverClient) -> ControlPlaneServiceImpl {
    ControlPlaneServiceImpl::new(
        Arc::new(InMemoryUpstreamRepo::new()),
        Arc::new(InMemoryRouteRepo::new()),
        Arc::new(resolver),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::empty()),
        Arc::new(SsrfGuard::disabled()),
        None,
    )
}

fn make_service_with_resolver_and_creds(
    resolver: MockTenantResolverClient,
    creds: Vec<(String, String)>,
) -> ControlPlaneServiceImpl {
    ControlPlaneServiceImpl::new(
        Arc::new(InMemoryUpstreamRepo::new()),
        Arc::new(InMemoryRouteRepo::new()),
        Arc::new(resolver),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::with_secrets(creds)),
        Arc::new(SsrfGuard::disabled()),
        None,
    )
}

fn test_ctx(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_tenant_id(tenant_id)
        .subject_id(Uuid::new_v4())
        .build()
        .expect("test security context")
}

/// Hostname-based upstream — alias is auto-derived as `api.openai.com`.
fn make_create_upstream_hostname() -> CreateUpstreamRequest {
    CreateUpstreamRequest {
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
    }
}

/// IP-based upstream — requires an explicit alias.
fn make_create_upstream_ip(alias: &str) -> CreateUpstreamRequest {
    CreateUpstreamRequest {
        id: None,
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: "10.0.0.1".into(),
                port: 443,
            }],
        },
        protocol: HTTP_PROTOCOL_ID.into(),
        alias: Some(alias.into()),
        auth: None,
        headers: None,
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
        enabled: true,
    }
}

/// Build an UpdateUpstreamRequest that mirrors the given upstream (full replacement).
fn make_update_from_upstream(u: &Upstream) -> UpdateUpstreamRequest {
    UpdateUpstreamRequest {
        server: u.server.clone(),
        protocol: u.protocol.clone(),
        alias: Some(u.alias.clone()),
        auth: u.auth.clone(),
        headers: u.headers.clone(),
        plugins: u.plugins.clone(),
        rate_limit: u.rate_limit.clone(),
        cors: u.cors.clone(),
        tags: u.tags.clone(),
        enabled: u.enabled,
    }
}

/// Build an UpdateRouteRequest that mirrors the given route (full replacement).
fn make_update_from_route(r: &Route) -> UpdateRouteRequest {
    UpdateRouteRequest {
        match_rules: r.match_rules.clone(),
        plugins: r.plugins.clone(),
        rate_limit: r.rate_limit.clone(),
        cors: r.cors.clone(),
        tags: r.tags.clone(),
        priority: r.priority,
        enabled: r.enabled,
    }
}

fn make_create_route(upstream_id: Uuid) -> CreateRouteRequest {
    CreateRouteRequest {
        id: None,
        upstream_id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Post],
                path: "/v1/chat/completions".into(),
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
    }
}

#[tokio::test]
async fn upstream_crud_lifecycle() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    // Create (IP-based, explicit alias)
    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();
    assert_eq!(u.alias, "openai");

    // Get
    let fetched = svc.get_upstream(&ctx, u.id).await.unwrap();
    assert_eq!(fetched.id, u.id);

    // Update with same alias (no-op, alias is immutable)
    let mut update_req = make_update_from_upstream(&u);
    update_req.alias = Some("openai".into());
    let updated = svc.update_upstream(&ctx, u.id, update_req).await.unwrap();
    assert_eq!(updated.alias, "openai");
    assert_eq!(updated.id, u.id);

    // Update with different alias — must be rejected.
    let mut update_req2 = make_update_from_upstream(&updated);
    update_req2.alias = Some("openai-v2".into());
    let err = svc
        .update_upstream(&ctx, u.id, update_req2)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));

    // List
    let list = svc
        .list_upstreams(&ctx, &ListQuery::default())
        .await
        .unwrap();
    assert_eq!(list.len(), 1);

    // Delete
    svc.delete_upstream(&ctx, u.id).await.unwrap();
    assert!(svc.get_upstream(&ctx, u.id).await.is_err());
}

#[tokio::test]
async fn alias_auto_generation() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    // Standard port (443) — port omitted in alias.
    let u1 = svc
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    assert_eq!(u1.alias, "api.openai.com");

    // Non-standard port — port included.
    let req = CreateUpstreamRequest {
        id: None,
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: "api.openai.com".into(),
                port: 8443,
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
    let u2 = svc.create_upstream(&ctx, req).await.unwrap();
    assert_eq!(u2.alias, "api.openai.com:8443");
}

#[tokio::test]
async fn alias_rejects_path_traversal() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let err = svc
        .create_upstream(&ctx, make_create_upstream_ip("../../admin"))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn alias_rejects_empty() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let err = svc
        .create_upstream(&ctx, make_create_upstream_ip(""))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn alias_rejects_slashes() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let err = svc
        .create_upstream(&ctx, make_create_upstream_ip("foo/bar"))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn duplicate_alias_conflict() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    svc.create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    let err = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Conflict { .. }));
}

#[tokio::test]
async fn route_create_with_wrong_tenant_upstream() {
    let svc = make_service();
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    let ctx1 = test_ctx(t1);
    let ctx2 = test_ctx(t2);

    let u = svc
        .create_upstream(&ctx1, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    // Try to create route in different tenant referencing t1's upstream.
    let err = svc
        .create_route(&ctx2, make_create_route(u.id))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn alias_resolution_enabled() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    let chain = svc.build_tenant_chain(&ctx).await.unwrap();
    let (resolved, _) = svc
        .resolve_alias(&ctx, &chain, "openai", None, Tags::Load)
        .await
        .unwrap();
    assert_eq!(resolved.id, u.id);
}

#[tokio::test]
async fn alias_resolution_disabled_returns_503() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    // Disable the upstream.
    let mut update_req = make_update_from_upstream(&u);
    update_req.enabled = false;
    svc.update_upstream(&ctx, u.id, update_req).await.unwrap();

    let chain = svc.build_tenant_chain(&ctx).await.unwrap();
    let err = svc
        .resolve_alias(&ctx, &chain, "openai", None, Tags::Load)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::UpstreamDisabled { .. }));
}

#[tokio::test]
async fn alias_resolution_nonexistent_returns_404() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let chain = svc.build_tenant_chain(&ctx).await.unwrap();
    let err = svc
        .resolve_alias(&ctx, &chain, "nonexistent", None, Tags::Load)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::NotFound { .. }));
}

/// A known alias whose routes match neither the method nor the path is a
/// route not-found, through the whole resolution.
#[tokio::test]
async fn resolve_proxy_target_without_a_matching_route_is_route_not_found() {
    let svc = make_service();
    let ctx = test_ctx(Uuid::new_v4());
    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();
    svc.create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap();

    for (method, path) in [("GET", "/v1/chat/completions"), ("POST", "/v2/other")] {
        let err = svc
            .resolve_proxy_target(
                &crate::domain::services::ProxyGrant::for_tests(ctx.clone()),
                "openai",
                method,
                path,
                Tags::Load,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                DomainError::NotFound {
                    entity: "route",
                    ..
                }
            ),
            "{method} {path}: {err:?}"
        );
    }
}

// -- validate_endpoints tests --

#[test]
fn validate_endpoints_rejects_empty() {
    let err = validate_endpoints(&[]).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn validate_endpoints_rejects_mixed_ip_and_hostname() {
    let endpoints = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "10.0.0.1".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "api.example.com".into(),
            port: 443,
        },
    ];
    let err = validate_endpoints(&endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("mixed"),
                "expected mixed error, got: {detail}"
            );
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

#[test]
fn validate_endpoints_rejects_mixed_scheme() {
    let endpoints = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "a.example.com".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Http,
            host: "b.example.com".into(),
            port: 443,
        },
    ];
    let err = validate_endpoints(&endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("scheme"),
                "expected scheme error, got: {detail}"
            );
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

#[test]
fn validate_endpoints_accepts_all_ip() {
    let endpoints = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "10.0.0.1".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "10.0.0.2".into(),
            port: 443,
        },
    ];
    assert!(validate_endpoints(&endpoints).is_ok());
}

#[test]
fn validate_endpoints_accepts_all_hostname() {
    let endpoints = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "a.example.com".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "b.example.com".into(),
            port: 443,
        },
    ];
    assert!(validate_endpoints(&endpoints).is_ok());
}

#[test]
fn validate_endpoints_rejects_mixed_ports() {
    let endpoints = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "a.example.com".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "b.example.com".into(),
            port: 8443,
        },
    ];
    let err = validate_endpoints(&endpoints).unwrap_err();
    assert!(
        err.to_string().contains("port"),
        "expected port error, got: {err}"
    );
}

#[test]
fn validate_endpoints_accepts_single() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    }];
    assert!(validate_endpoints(&endpoints).is_ok());
}

#[test]
fn forbid_ipv6_endpoints_rejects_ipv6() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "::1".into(),
        port: 443,
    }];
    let err = forbid_ipv6_endpoints(&endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("IPv6"),
                "expected IPv6 error, got: {detail}"
            );
            assert!(
                detail.contains("not yet supported"),
                "expected 'not yet supported', got: {detail}"
            );
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

#[test]
fn forbid_ipv6_endpoints_rejects_ipv6_full_address() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "2001:db8::1".into(),
        port: 8443,
    }];
    let err = forbid_ipv6_endpoints(&endpoints).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn forbid_ipv6_endpoints_rejects_bracketed_ipv6() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "[2001:db8::1]".into(),
        port: 8443,
    }];
    let err = forbid_ipv6_endpoints(&endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("IPv6"),
                "expected IPv6 error, got: {detail}"
            );
            assert!(
                detail.contains("not yet supported"),
                "expected 'not yet supported', got: {detail}"
            );
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

// -- validate_hostname (RFC 1123) tests --

#[test]
fn validate_endpoints_rejects_empty_label() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api..openai.com".into(),
        port: 443,
    }];
    let err = validate_endpoints(&endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("empty label"),
                "expected empty label error, got: {detail}"
            );
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

#[test]
fn validate_endpoints_rejects_leading_hyphen() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "-api.openai.com".into(),
        port: 443,
    }];
    let err = validate_endpoints(&endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("must not start or end with '-'"),
                "expected hyphen error, got: {detail}"
            );
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

#[test]
fn validate_endpoints_rejects_trailing_hyphen() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api-.openai.com".into(),
        port: 443,
    }];
    let err = validate_endpoints(&endpoints).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn validate_endpoints_rejects_underscore_in_hostname() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api_v2.openai.com".into(),
        port: 443,
    }];
    let err = validate_endpoints(&endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("invalid characters"),
                "expected invalid chars error, got: {detail}"
            );
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

#[test]
fn validate_endpoints_rejects_overlength_label() {
    let long_label = "a".repeat(64);
    let host = format!("{long_label}.example.com");
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host,
        port: 443,
    }];
    let err = validate_endpoints(&endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("exceeds 63"),
                "expected label length error, got: {detail}"
            );
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

#[test]
fn validate_endpoints_accepts_trailing_dot_fqdn() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com.".into(),
        port: 443,
    }];
    assert!(validate_endpoints(&endpoints).is_ok());
}

#[test]
fn validate_endpoints_accepts_max_length_label() {
    let label = "a".repeat(63);
    let host = format!("{label}.example.com");
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host,
        port: 443,
    }];
    assert!(validate_endpoints(&endpoints).is_ok());
}

#[test]
fn validate_endpoints_rejects_empty_hostname() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "".into(),
        port: 443,
    }];
    let err = validate_endpoints(&endpoints).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn delete_upstream_deletes_its_routes() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();
    let r1 = svc
        .create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap();

    // Add a second route with a different path to avoid overlap.
    let mut req2 = make_create_route(u.id);
    req2.match_rules.http.as_mut().unwrap().methods = vec![HttpMethod::Get];
    let r2 = svc.create_route(&ctx, req2).await.unwrap();

    svc.delete_upstream(&ctx, u.id).await.unwrap();

    for id in [r1.id, r2.id] {
        assert!(matches!(
            svc.get_route(&ctx, id).await,
            Err(DomainError::NotFound { .. })
        ));
    }
    assert!(matches!(
        svc.get_upstream(&ctx, u.id).await,
        Err(DomainError::NotFound { .. })
    ));
}

#[tokio::test]
async fn delete_upstream_without_routes() {
    let svc = make_service();
    let ctx = test_ctx(Uuid::new_v4());

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    svc.delete_upstream(&ctx, u.id).await.unwrap();
    assert!(matches!(
        svc.get_upstream(&ctx, u.id).await,
        Err(DomainError::NotFound { .. })
    ));
}

/// The upstream goes first: when another tenant's delete finds no upstream,
/// the owner's routes are untouched.
#[tokio::test]
async fn delete_upstream_of_another_tenant_keeps_its_routes() {
    let svc = make_service();
    let owner = test_ctx(Uuid::new_v4());
    let intruder = test_ctx(Uuid::new_v4());

    let u = svc
        .create_upstream(&owner, make_create_upstream_ip("openai"))
        .await
        .unwrap();
    let r = svc
        .create_route(&owner, make_create_route(u.id))
        .await
        .unwrap();

    assert!(matches!(
        svc.delete_upstream(&intruder, u.id).await,
        Err(DomainError::NotFound { .. })
    ));
    assert_eq!(svc.get_route(&owner, r.id).await.unwrap(), r);
}

// -- Alias resolution tests --

#[tokio::test]
async fn resolve_alias_walks_tenant_chain_to_ancestor() {
    use crate::domain::model::{AuthConfig, SharingMode};

    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Create upstream in root tenant with inherit sharing (visible to descendants).
    let root_ctx = test_ctx(root);
    let mut req = make_create_upstream_hostname();
    req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let root_upstream = svc.create_upstream(&root_ctx, req).await.unwrap();

    // Child tenant should resolve the alias via tenant chain walk.
    let child_ctx = test_ctx(child);
    let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
    let (resolved, _) = svc
        .resolve_alias(&child_ctx, &chain, "api.openai.com", None, Tags::Load)
        .await
        .unwrap();
    assert_eq!(resolved.id, root_upstream.id);
}

/// ADR-0018: PERS-STORE-006.
#[tokio::test]
async fn resolve_alias_child_shadows_ancestor() {
    use crate::domain::model::{AuthConfig, SharingMode};

    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Create upstream in root with inherit sharing.
    let root_ctx = test_ctx(root);
    let mut req = make_create_upstream_hostname();
    req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    svc.create_upstream(&root_ctx, req).await.unwrap();

    // Create upstream with same host in child tenant (same derived alias shadows root).
    let child_ctx = test_ctx(child);
    let child_upstream = svc
        .create_upstream(&child_ctx, make_create_upstream_hostname())
        .await
        .unwrap();

    // Child resolves to its own upstream (shadow wins).
    let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
    let (resolved, _) = svc
        .resolve_alias(&child_ctx, &chain, "api.openai.com", None, Tags::Load)
        .await
        .unwrap();
    assert_eq!(resolved.id, child_upstream.id);
}

/// Alias shadowing over four levels (root, a, b, leaf), on every store.
/// Root and b share their upstream, and a shares nothing. The closest
/// visible enabled upstream wins and an ancestor that shares nothing is
/// skipped, with its routes. A disabled upstream falls back to the next one
/// up. A tenant's own upstream wins over every ancestor, even unshared.
///
/// ADR-0018: PERS-STORE-006.
async fn alias_shadowing_over_four_levels(backend: Backend) {
    use crate::domain::model::{AuthConfig, SharingMode, UpdateUpstreamRequest};

    let [root, a, b, leaf] = std::array::from_fn(|_| Uuid::new_v4());
    let stores = Stores::new(backend);
    let svc = ControlPlaneServiceImpl::new(
        stores.upstreams,
        stores.routes,
        Arc::new(MockTenantResolverClient::with_hierarchy(
            [root, a, b, leaf].map(TenantId).to_vec(),
        )),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::empty()),
        Arc::new(SsrfGuard::disabled()),
        None,
    );
    let shared = |enabled| CreateUpstreamRequest {
        auth: Some(AuthConfig {
            plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Inherit,
            config: None,
        }),
        enabled,
        ..make_create_upstream_hostname()
    };
    let root_up = svc
        .create_upstream(&test_ctx(root), shared(true))
        .await
        .unwrap();
    let a_up = svc
        .create_upstream(&test_ctx(a), make_create_upstream_hostname())
        .await
        .unwrap();
    let b_up = svc
        .create_upstream(&test_ctx(b), shared(true))
        .await
        .unwrap();
    let root_route = svc
        .create_route(&test_ctx(root), make_create_route(root_up.id))
        .await
        .unwrap();
    // A better match on the unshared upstream, which `leaf` must not see.
    svc.create_route(
        &test_ctx(a),
        CreateRouteRequest {
            priority: 9,
            ..make_create_route(a_up.id)
        },
    )
    .await
    .unwrap();

    // The upstream `tenant` resolves the alias to, and the route it selects.
    let resolve = async |tenant: Uuid| {
        let ctx = test_ctx(tenant);
        let chain = svc.build_tenant_chain(&ctx).await.unwrap();
        let (upstream, route) = svc
            .resolve_alias(
                &ctx,
                &chain,
                "api.openai.com",
                Some(("POST", "/v1/chat/completions")),
                Tags::Skip,
            )
            .await
            .unwrap();
        (upstream.id, route.map(|r| r.id))
    };
    assert_eq!(resolve(leaf).await, (b_up.id, Some(root_route.id)));
    assert_eq!(resolve(b).await, (b_up.id, Some(root_route.id)));
    assert_eq!(resolve(a).await.0, a_up.id, "its own, though unshared");

    // b disabled: leaf and b fall back past a to root.
    let disabled = shared(false);
    svc.update_upstream(
        &test_ctx(b),
        b_up.id,
        UpdateUpstreamRequest {
            server: disabled.server,
            protocol: disabled.protocol,
            alias: disabled.alias,
            auth: disabled.auth,
            headers: disabled.headers,
            plugins: disabled.plugins,
            rate_limit: disabled.rate_limit,
            cors: disabled.cors,
            tags: disabled.tags,
            enabled: disabled.enabled,
        },
    )
    .await
    .unwrap();
    assert_eq!(resolve(leaf).await, (root_up.id, Some(root_route.id)));
    assert_eq!(resolve(b).await, (root_up.id, Some(root_route.id)));

    // leaf's own upstream, unshared, wins over every ancestor.
    let leaf_up = svc
        .create_upstream(&test_ctx(leaf), make_create_upstream_hostname())
        .await
        .unwrap();
    assert_eq!(resolve(leaf).await, (leaf_up.id, Some(root_route.id)));
}

mod alias_shadowing_over_four_levels {
    use super::{Backend, alias_shadowing_over_four_levels as case};

    #[tokio::test]
    async fn in_memory() {
        case(Backend::Memory).await;
    }

    #[tokio::test]
    async fn sqlite() {
        case(Backend::Db(
            crate::infra::storage::testing::shared_sqlite().await,
        ))
        .await;
    }

    #[cfg(feature = "integration")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn postgres() {
        let server = crate::infra::storage::testing::postgres_server().await;
        case(Backend::Db(server.fresh().await)).await;
    }

    #[cfg(feature = "integration")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mysql() {
        let server = crate::infra::storage::testing::mysql_server().await;
        case(Backend::Db(server.fresh().await)).await;
    }
}

#[tokio::test]
async fn resolve_alias_private_ancestor_not_visible() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Create upstream in root with all-private sharing (default).
    let root_ctx = test_ctx(root);
    svc.create_upstream(&root_ctx, make_create_upstream_hostname())
        .await
        .unwrap();

    // Child should NOT see the private upstream → NotFound.
    let child_ctx = test_ctx(child);
    let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
    let err = svc
        .resolve_alias(&child_ctx, &chain, "api.openai.com", None, Tags::Load)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::NotFound { .. }));
}

#[tokio::test]
async fn resolve_alias_disabled_ancestor_falls_through() {
    use crate::domain::model::{AuthConfig, SharingMode};

    let root = Uuid::new_v4();
    let parent = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![
        TenantId(root),
        TenantId(parent),
        TenantId(child),
    ]);
    let svc = make_service_with_resolver(resolver);

    // Create disabled upstream in parent with inherit sharing.
    let parent_ctx = test_ctx(parent);
    let mut req = make_create_upstream_hostname();
    req.enabled = false;
    req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    svc.create_upstream(&parent_ctx, req).await.unwrap();

    // Create enabled upstream in root with inherit sharing.
    let root_ctx = test_ctx(root);
    let mut req2 = make_create_upstream_hostname();
    req2.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let root_upstream = svc.create_upstream(&root_ctx, req2).await.unwrap();

    // Child resolves: parent disabled → falls through to root.
    let child_ctx = test_ctx(child);
    let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
    let (resolved, _) = svc
        .resolve_alias(&child_ctx, &chain, "api.openai.com", None, Tags::Load)
        .await
        .unwrap();
    assert_eq!(resolved.id, root_upstream.id);
}

#[tokio::test]
async fn resolve_alias_all_disabled_returns_upstream_disabled() {
    use crate::domain::model::{AuthConfig, SharingMode};

    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Create disabled upstream in root with inherit sharing.
    let root_ctx = test_ctx(root);
    let mut req = make_create_upstream_hostname();
    req.enabled = false;
    req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    svc.create_upstream(&root_ctx, req).await.unwrap();

    // Child resolves: only disabled match → UpstreamDisabled.
    let child_ctx = test_ctx(child);
    let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
    let err = svc
        .resolve_alias(&child_ctx, &chain, "api.openai.com", None, Tags::Load)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::UpstreamDisabled { .. }));
}

#[tokio::test]
async fn resolve_alias_disabled_child_falls_through_to_ancestor() {
    use crate::domain::model::{AuthConfig, SharingMode};

    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Create enabled upstream in root with inherit sharing.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let root_upstream = svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Create disabled upstream in child with same host (same derived alias).
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.enabled = false;
    child_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    svc.create_upstream(&child_ctx, child_req).await.unwrap();

    // Child resolves: own upstream disabled → falls through to root ancestor.
    let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
    let (resolved, _) = svc
        .resolve_alias(&child_ctx, &chain, "api.openai.com", None, Tags::Load)
        .await
        .unwrap();
    assert_eq!(resolved.id, root_upstream.id);
}

#[tokio::test]
async fn resolve_alias_no_match_in_tenant_chain_returns_not_found() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // No upstreams created anywhere.
    let child_ctx = test_ctx(child);
    let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
    let err = svc
        .resolve_alias(&child_ctx, &chain, "nonexistent", None, Tags::Load)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::NotFound { .. }));
}

// -- Effective config merge tests --

use std::collections::HashMap;

use crate::domain::model::{
    AuthConfig, CorsConfig, CorsHttpMethod, PluginBinding, PluginsConfig, RateLimitAlgorithm,
    RateLimitConfig, RateLimitScope, RateLimitStrategy, SharingMode, SustainedRate, Window,
};

fn make_upstream(
    tenant_id: Uuid,
    alias: &str,
    auth: Option<AuthConfig>,
    rate_limit: Option<RateLimitConfig>,
    plugins: Option<PluginsConfig>,
    tags: Vec<String>,
) -> Upstream {
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
        protocol: "http".into(),
        enabled: true,
        auth,
        headers: None,
        plugins,
        rate_limit,
        cors: None,
        tags,
        managed_by: ManagedBy::Api,
    }
}

fn make_rate_limit(sharing: SharingMode, rate: u32, window: Window) -> RateLimitConfig {
    RateLimitConfig {
        sharing,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate { rate, window },
        burst: None,
        budget: None,
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        shared_pool: None,
        limit_owner_id: None,
    }
}

/// A candidate whose tenant is not in the chain sorts after the chain's,
/// so it never wins over the subject tenant's own upstream.
#[test]
fn select_upstream_ranks_a_tenant_outside_the_chain_last() {
    let (child, parent) = (Uuid::new_v4(), Uuid::new_v4());
    let shared = Some(make_rate_limit(SharingMode::Inherit, 10, Window::Second));
    let outsider = make_upstream(Uuid::new_v4(), "a", None, shared, None, vec![]);
    let own = make_upstream(child, "a", None, None, None, vec![]);

    let (selected, _) =
        super::resolution::select_upstream(&[outsider, own.clone()], child, &[child, parent])
            .expect("selected");
    assert_eq!(selected.id, own.id);
}

#[test]
fn effective_config_single_upstream() {
    let t = Uuid::new_v4();
    let u = make_upstream(t, "openai", None, None, None, vec!["a".into()]);
    let effective = compute_effective_config(std::slice::from_ref(&u), None).unwrap();
    assert_eq!(effective.id, u.id);
    assert_eq!(effective.tags, vec!["a".to_string()]);
}

#[test]
fn effective_config_auth_inherit_descendant_overrides() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_auth = AuthConfig {
        plugin_type: crate::domain::gts_helpers::APIKEY_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    };
    let child_auth = AuthConfig {
        plugin_type: crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    };

    let root = make_upstream(root_id, "openai", Some(root_auth), None, None, vec![]);
    let child = make_upstream(
        child_id,
        "openai",
        Some(child_auth.clone()),
        None,
        None,
        vec![],
    );

    let effective = compute_effective_config(&[root, child], None).unwrap();
    assert_eq!(
        effective.auth.unwrap().plugin_type,
        crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID
    );
}

#[test]
fn effective_config_auth_enforce_ancestor_wins() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_auth = AuthConfig {
        plugin_type: crate::domain::gts_helpers::APIKEY_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Enforce,
        config: None,
    };
    let child_auth = AuthConfig {
        plugin_type: crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    };

    let root = make_upstream(root_id, "openai", Some(root_auth), None, None, vec![]);
    let child = make_upstream(child_id, "openai", Some(child_auth), None, None, vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    // Ancestor enforce wins — apikey stays.
    assert_eq!(
        effective.auth.unwrap().plugin_type,
        crate::domain::gts_helpers::APIKEY_AUTH_PLUGIN_ID
    );
}

#[test]
fn effective_config_rate_limit_min_wins() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_rl = make_rate_limit(SharingMode::Enforce, 100, Window::Minute);
    let child_rl = make_rate_limit(SharingMode::Inherit, 200, Window::Minute);

    let root = make_upstream(root_id, "openai", None, Some(root_rl), None, vec![]);
    let child = make_upstream(child_id, "openai", None, Some(child_rl), None, vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    // min(100/min, 200/min) = 100/min
    assert_eq!(effective.rate_limit.unwrap().sustained.rate, 100);
}

#[test]
fn effective_config_rate_limit_descendant_stricter() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_rl = make_rate_limit(SharingMode::Inherit, 1000, Window::Minute);
    let child_rl = make_rate_limit(SharingMode::Inherit, 50, Window::Minute);

    let root = make_upstream(root_id, "openai", None, Some(root_rl), None, vec![]);
    let child = make_upstream(child_id, "openai", None, Some(child_rl), None, vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    assert_eq!(effective.rate_limit.unwrap().sustained.rate, 50);
}

#[test]
fn effective_config_plugins_concatenation() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_plugins = PluginsConfig {
        sharing: SharingMode::Inherit,
        items: vec![
            PluginBinding {
                plugin_ref: "plugin-a".into(),
                config: HashMap::new(),
            },
            PluginBinding {
                plugin_ref: "plugin-b".into(),
                config: HashMap::new(),
            },
        ],
    };
    let child_plugins = PluginsConfig {
        sharing: SharingMode::Inherit,
        items: vec![
            PluginBinding {
                plugin_ref: "plugin-b".into(),
                config: HashMap::new(),
            },
            PluginBinding {
                plugin_ref: "plugin-c".into(),
                config: HashMap::new(),
            },
        ],
    };

    let root = make_upstream(root_id, "openai", None, None, Some(root_plugins), vec![]);
    let child = make_upstream(child_id, "openai", None, None, Some(child_plugins), vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    let items = effective.plugins.unwrap().items;
    // ancestor + descendant (dedup): [a, b, c]
    assert_eq!(
        items
            .iter()
            .map(|b| b.plugin_ref.as_str())
            .collect::<Vec<_>>(),
        vec!["plugin-a", "plugin-b", "plugin-c"]
    );
}

#[test]
fn effective_config_enforced_plugins_cannot_be_removed() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_plugins = PluginsConfig {
        sharing: SharingMode::Enforce,
        items: vec![PluginBinding {
            plugin_ref: "required-plugin".into(),
            config: HashMap::new(),
        }],
    };
    let child_plugins = PluginsConfig {
        sharing: SharingMode::Enforce,
        items: vec![PluginBinding {
            plugin_ref: "extra-plugin".into(),
            config: HashMap::new(),
        }],
    };

    let root = make_upstream(root_id, "openai", None, None, Some(root_plugins), vec![]);
    let child = make_upstream(child_id, "openai", None, None, Some(child_plugins), vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    let items = effective.plugins.unwrap().items;
    // Enforced plugins remain: required-plugin + extra-plugin.
    assert!(items.iter().any(|b| b.plugin_ref == "required-plugin"));
    assert!(items.iter().any(|b| b.plugin_ref == "extra-plugin"));
}

#[test]
fn effective_config_tags_union() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root = make_upstream(
        root_id,
        "openai",
        None,
        None,
        None,
        vec!["env:prod".into(), "team:platform".into()],
    );
    let child = make_upstream(
        child_id,
        "openai",
        None,
        None,
        None,
        vec!["team:platform".into(), "region:us".into()],
    );

    let effective = compute_effective_config(&[root, child], None).unwrap();
    assert!(effective.tags.contains(&"env:prod".to_string()));
    assert!(effective.tags.contains(&"team:platform".to_string()));
    assert!(effective.tags.contains(&"region:us".to_string()));
    assert_eq!(effective.tags.len(), 3);
}

#[test]
fn effective_config_leaves_the_route_rate_limit_to_the_route_bucket() {
    let t = Uuid::new_v4();
    let upstream_rl = make_rate_limit(SharingMode::Inherit, 100, Window::Minute);
    let u = make_upstream(t, "openai", None, Some(upstream_rl), None, vec![]);

    let route = Route {
        id: Uuid::new_v4(),
        tenant_id: t,
        upstream_id: u.id,
        match_rules: MatchRules {
            http: None,
            grpc: None,
        },
        plugins: None,
        rate_limit: Some(make_rate_limit(SharingMode::Inherit, 50, Window::Minute)),
        cors: None,
        tags: vec![],
        priority: 0,
        enabled: true,
        managed_by: ManagedBy::Api,
    };

    let effective = compute_effective_config(&[u], Some(&route)).unwrap();
    // The proxy checks the route's 50/min on the route's own bucket; merged
    // in here it would reconfigure the upstream bucket per route.
    assert_eq!(effective.rate_limit.unwrap().sustained.rate, 100);
}

#[test]
fn effective_config_three_layer_merge() {
    let root_id = Uuid::new_v4();
    let parent_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root = make_upstream(
        root_id,
        "openai",
        Some(AuthConfig {
            plugin_type: crate::domain::gts_helpers::APIKEY_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Enforce,
            config: None,
        }),
        Some(make_rate_limit(SharingMode::Enforce, 1000, Window::Minute)),
        Some(PluginsConfig {
            sharing: SharingMode::Enforce,
            items: vec![PluginBinding {
                plugin_ref: crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID.into(),
                config: HashMap::new(),
            }],
        }),
        vec!["env:prod".into()],
    );
    let parent = make_upstream(
        parent_id,
        "openai",
        Some(AuthConfig {
            plugin_type: crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Inherit,
            config: None,
        }),
        Some(make_rate_limit(SharingMode::Inherit, 500, Window::Minute)),
        Some(PluginsConfig {
            sharing: SharingMode::Inherit,
            items: vec![PluginBinding {
                plugin_ref: crate::domain::gts_helpers::REQUEST_ID_TRANSFORM_PLUGIN_ID.into(),
                config: HashMap::new(),
            }],
        }),
        vec!["team:partner".into()],
    );
    let child = make_upstream(
        child_id,
        "openai",
        None,
        Some(make_rate_limit(SharingMode::Inherit, 200, Window::Minute)),
        Some(PluginsConfig {
            sharing: SharingMode::Inherit,
            items: vec![PluginBinding {
                plugin_ref: "transform-x".into(),
                config: HashMap::new(),
            }],
        }),
        vec!["region:us".into()],
    );

    let child_id_val = child.id;
    let effective = compute_effective_config(&[root, parent, child], None).unwrap();

    // Auth: root enforced → apikey wins even though parent set oauth2.
    assert_eq!(
        effective.auth.unwrap().plugin_type,
        crate::domain::gts_helpers::APIKEY_AUTH_PLUGIN_ID
    );

    // Rate limit: min(1000, 500, 200) = 200/min, still enforced.
    let rate_limit = effective.rate_limit.unwrap();
    assert_eq!(rate_limit.sustained.rate, 200);
    assert_eq!(rate_limit.sharing, SharingMode::Enforce);

    // Plugins: enforced audit-log + rate-guard + transform-x, still enforced.
    let plugins = effective.plugins.unwrap();
    assert_eq!(plugins.sharing, SharingMode::Enforce);
    let items = plugins.items;
    assert!(
        items
            .iter()
            .any(|b| b.plugin_ref == crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID)
    );
    assert!(
        items
            .iter()
            .any(|b| b.plugin_ref == crate::domain::gts_helpers::REQUEST_ID_TRANSFORM_PLUGIN_ID)
    );
    assert!(items.iter().any(|b| b.plugin_ref == "transform-x"));

    // Tags: union of all three.
    assert!(effective.tags.contains(&"env:prod".to_string()));
    assert!(effective.tags.contains(&"team:partner".to_string()));
    assert!(effective.tags.contains(&"region:us".to_string()));

    // Identity: uses child's id/tenant.
    assert_eq!(effective.id, child_id_val);
    assert_eq!(effective.tenant_id, child_id);
}

// -- CORS effective config merge tests --

fn make_cors(sharing: SharingMode, origins: Vec<&str>) -> CorsConfig {
    CorsConfig {
        sharing,
        enabled: true,
        allowed_origins: origins.into_iter().map(String::from).collect(),
        allowed_methods: vec![CorsHttpMethod::Get, CorsHttpMethod::Post],
        expose_headers: vec![],
        allow_credentials: false,
    }
}

#[test]
fn effective_config_cors_inherit_unions_origins() {
    let t = Uuid::new_v4();
    let mut root = make_upstream(t, "api", None, None, None, vec![]);
    root.cors = Some(make_cors(SharingMode::Inherit, vec!["https://parent.com"]));
    let mut child = make_upstream(t, "api", None, None, None, vec![]);
    child.cors = Some(make_cors(SharingMode::Inherit, vec!["https://child.com"]));

    let effective = compute_effective_config(&[root, child], None).unwrap();
    let cors = effective.cors.unwrap();
    assert!(cors.allowed_origins.contains(&"https://child.com".into()));
    assert!(cors.allowed_origins.contains(&"https://parent.com".into()));
}

#[test]
fn effective_config_cors_private_descendant_skips() {
    // Per inst-merge-3a5: private → skip (do not modify effective).
    // When child has Private CORS, the ancestor's CORS is preserved.
    let t = Uuid::new_v4();
    let mut root = make_upstream(t, "api", None, None, None, vec![]);
    root.cors = Some(make_cors(SharingMode::Inherit, vec!["https://parent.com"]));
    let mut child = make_upstream(t, "api", None, None, None, vec![]);
    child.cors = Some(make_cors(SharingMode::Private, vec!["https://child.com"]));

    let effective = compute_effective_config(&[root, child], None).unwrap();
    let cors = effective.cors.unwrap();
    assert!(
        cors.allowed_origins.contains(&"https://parent.com".into()),
        "Private skip: ancestor CORS should be preserved"
    );
    assert!(
        !cors.allowed_origins.contains(&"https://child.com".into()),
        "Private skip: child origins should NOT appear in effective"
    );
}

#[test]
fn effective_config_cors_enforce_blocks_descendant() {
    let t = Uuid::new_v4();
    let mut root = make_upstream(t, "api", None, None, None, vec![]);
    root.cors = Some(make_cors(SharingMode::Enforce, vec!["https://locked.com"]));
    let mut child = make_upstream(t, "api", None, None, None, vec![]);
    child.cors = Some(make_cors(SharingMode::Inherit, vec!["https://child.com"]));

    let effective = compute_effective_config(&[root, child], None).unwrap();
    let cors = effective.cors.unwrap();
    assert_eq!(cors.allowed_origins, vec!["https://locked.com"]);
}

#[test]
fn effective_config_route_cors_inherit_unions_with_upstream() {
    let t = Uuid::new_v4();
    let mut upstream = make_upstream(t, "api", None, None, None, vec![]);
    upstream.cors = Some(make_cors(
        SharingMode::Inherit,
        vec!["https://upstream.com"],
    ));

    let route = Route {
        id: Uuid::new_v4(),
        tenant_id: t,
        upstream_id: upstream.id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Get],
                path: "/v1".into(),
                query_allowlist: vec![],
                path_suffix_mode: PathSuffixMode::Append,
            }),
            grpc: None,
        },
        plugins: None,
        rate_limit: None,
        cors: Some(make_cors(SharingMode::Inherit, vec!["https://route.com"])),
        tags: vec![],
        priority: 0,
        enabled: true,
        managed_by: ManagedBy::Api,
    };

    let effective =
        compute_effective_config(std::slice::from_ref(&upstream), Some(&route)).unwrap();
    let cors = effective.cors.unwrap();
    assert!(cors.allowed_origins.contains(&"https://route.com".into()));
    assert!(
        cors.allowed_origins
            .contains(&"https://upstream.com".into())
    );
}

#[test]
fn effective_config_cors_private_ancestor_not_inherited_when_absent() {
    // When ancestor CORS is Private and child has no CORS, effective should be None.
    let (root_t, child_t) = (Uuid::new_v4(), Uuid::new_v4());
    let mut root = make_upstream(root_t, "api", None, None, None, vec![]);
    root.cors = Some(make_cors(SharingMode::Private, vec!["https://root.com"]));
    let child = make_upstream(child_t, "api", None, None, None, vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    assert!(
        effective.cors.is_none(),
        "Private ancestor CORS must not propagate to descendants"
    );
}

#[test]
fn effective_config_cors_inherit_skips_private_ancestor_origins() {
    // When ancestor CORS is Private, its origins should not be unioned into Inherit descendant.
    let (root_t, child_t) = (Uuid::new_v4(), Uuid::new_v4());
    let mut root = make_upstream(root_t, "api", None, None, None, vec![]);
    root.cors = Some(make_cors(SharingMode::Private, vec!["https://root.com"]));
    let mut child = make_upstream(child_t, "api", None, None, None, vec![]);
    child.cors = Some(make_cors(SharingMode::Inherit, vec!["https://child.com"]));

    let effective = compute_effective_config(&[root, child], None).unwrap();
    let cors = effective.cors.unwrap();
    assert!(cors.allowed_origins.contains(&"https://child.com".into()));
    assert!(
        !cors.allowed_origins.contains(&"https://root.com".into()),
        "Private ancestor origins must not be unioned"
    );
}

#[test]
fn effective_config_cors_enforce_descendant_keeps_ancestor() {
    // Per inst-merge-3a5: enforce → use ancestor CORS (keep effective unchanged).
    let t = Uuid::new_v4();
    let mut root = make_upstream(t, "api", None, None, None, vec![]);
    root.cors = Some(make_cors(
        SharingMode::Inherit,
        vec!["https://ancestor.com"],
    ));
    let mut child = make_upstream(t, "api", None, None, None, vec![]);
    child.cors = Some(make_cors(SharingMode::Enforce, vec!["https://child.com"]));

    let effective = compute_effective_config(&[root, child], None).unwrap();
    let cors = effective.cors.unwrap();
    assert_eq!(
        cors.allowed_origins,
        vec!["https://ancestor.com"],
        "Enforce descendant should keep ancestor CORS unchanged"
    );
}

#[test]
fn effective_config_cors_merge_rejects_invalid_union() {
    // Unioning origins can produce an invalid config (credentials + wildcard).
    let t = Uuid::new_v4();
    let mut root = make_upstream(t, "api", None, None, None, vec![]);
    root.cors = Some(CorsConfig {
        sharing: SharingMode::Inherit,
        enabled: true,
        allowed_origins: vec!["*".into()],
        allowed_methods: vec![CorsHttpMethod::Get],
        expose_headers: vec![],
        allow_credentials: false,
    });
    let mut child = make_upstream(t, "api", None, None, None, vec![]);
    child.cors = Some(CorsConfig {
        sharing: SharingMode::Inherit,
        enabled: true,
        allowed_origins: vec!["https://child.com".into()],
        allowed_methods: vec![CorsHttpMethod::Get],
        expose_headers: vec![],
        allow_credentials: true,
    });

    let result = compute_effective_config(&[root, child], None);
    assert!(
        result.is_err(),
        "Merged CORS with credentials + wildcard must be rejected"
    );
}

#[test]
fn effective_config_route_cors_merge_rejects_invalid_union() {
    let t = Uuid::new_v4();
    let mut upstream = make_upstream(t, "api", None, None, None, vec![]);
    upstream.cors = Some(CorsConfig {
        sharing: SharingMode::Inherit,
        enabled: true,
        allowed_origins: vec!["*".into()],
        allowed_methods: vec![CorsHttpMethod::Get],
        expose_headers: vec![],
        allow_credentials: false,
    });

    let route = Route {
        id: Uuid::new_v4(),
        tenant_id: t,
        upstream_id: upstream.id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Get],
                path: "/v1".into(),
                query_allowlist: vec![],
                path_suffix_mode: PathSuffixMode::Append,
            }),
            grpc: None,
        },
        plugins: None,
        rate_limit: None,
        cors: Some(CorsConfig {
            sharing: SharingMode::Inherit,
            enabled: true,
            allowed_origins: vec!["https://route.com".into()],
            allowed_methods: vec![CorsHttpMethod::Get],
            expose_headers: vec![],
            allow_credentials: true,
        }),
        tags: vec![],
        priority: 0,
        enabled: true,
        managed_by: ManagedBy::Api,
    };

    let result = compute_effective_config(std::slice::from_ref(&upstream), Some(&route));
    assert!(
        result.is_err(),
        "Route CORS merge with credentials + wildcard must be rejected"
    );
}

// -- Private stays with its owner; enforce is sticky (#4975) --

fn noop_auth(sharing: SharingMode) -> AuthConfig {
    AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing,
        config: None,
    }
}

/// `items` are `(plugin_ref, marker)`; the marker goes to the binding's
/// config under `"by"`, to tell which level's binding won.
fn plugins_of(sharing: SharingMode, items: &[(&str, &str)]) -> PluginsConfig {
    PluginsConfig {
        sharing,
        items: items
            .iter()
            .map(|(plugin_ref, by)| PluginBinding {
                plugin_ref: (*plugin_ref).into(),
                config: HashMap::from([("by".to_owned(), (*by).to_owned())]),
            })
            .collect(),
    }
}

fn plugin_refs(plugins: Option<&PluginsConfig>) -> Vec<(&str, &str)> {
    plugins
        .into_iter()
        .flat_map(|p| &p.items)
        .map(|b| {
            (
                b.plugin_ref.as_str(),
                b.config.get("by").map_or("", String::as_str),
            )
        })
        .collect()
}

fn route_on(upstream: &Upstream, plugins: Option<PluginsConfig>) -> Route {
    Route {
        id: Uuid::new_v4(),
        tenant_id: upstream.tenant_id,
        upstream_id: upstream.id,
        match_rules: MatchRules {
            http: None,
            grpc: None,
        },
        plugins,
        rate_limit: None,
        cors: None,
        tags: vec![],
        priority: 0,
        enabled: true,
        managed_by: ManagedBy::Api,
    }
}

const RH: &str = crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID;
const RID: &str = crate::domain::gts_helpers::REQUEST_ID_TRANSFORM_PLUGIN_ID;
const CHAT_PATH: &str = "/v1/chat/completions";

/// A child with no upstream of its own resolves to the ancestor's, and gets
/// only what the ancestor shares: its private auth is not sent for the
/// child, through the proxy's resolution and the SDK's.
#[tokio::test]
async fn a_private_ancestor_auth_never_reaches_a_child_without_an_upstream() {
    let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    let root_ctx = test_ctx(root);
    let mut req = make_create_upstream_hostname();
    req.auth = Some(noop_auth(SharingMode::Private));
    req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 100, Window::Minute));
    let root_up = svc.create_upstream(&root_ctx, req).await.unwrap();
    svc.create_route(&root_ctx, make_create_route(root_up.id))
        .await
        .unwrap();

    let child_ctx = test_ctx(child);
    let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
    for tags in [Tags::Skip, Tags::Load] {
        let (effective, _) = svc
            .resolve_alias(
                &child_ctx,
                &chain,
                &root_up.alias,
                Some(("POST", CHAT_PATH)),
                tags,
            )
            .await
            .unwrap();
        assert_eq!(effective.id, root_up.id);
        assert!(effective.auth.is_none(), "{:?}", effective.auth);
        assert_eq!(effective.rate_limit.map(|r| r.sustained.rate), Some(100));
    }
    let (effective, _) = svc
        .resolve_proxy_target_for_client(&child_ctx, &root_up.alias, "POST", CHAT_PATH)
        .await
        .unwrap();
    assert!(effective.auth.is_none(), "{:?}", effective.auth);

    // The owner keeps it.
    let root_chain = svc.build_tenant_chain(&root_ctx).await.unwrap();
    let (own, _) = svc
        .resolve_alias(&root_ctx, &root_chain, &root_up.alias, None, Tags::Load)
        .await
        .unwrap();
    assert!(own.auth.is_some());
}

/// An ancestor's private limit, plugins and CORS take no part in a child's
/// merge: neither when the child sets the field with `inherit` (no `min`
/// against the private limit, no concatenation) nor when it leaves it out.
#[tokio::test]
async fn private_ancestor_limit_plugins_and_cors_stay_out_of_a_childs_merge() {
    let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    let mut req = make_create_upstream_hostname();
    req.auth = Some(noop_auth(SharingMode::Inherit));
    req.rate_limit = Some(make_rate_limit(SharingMode::Private, 10, Window::Minute));
    req.plugins = Some(plugins_of(SharingMode::Private, &[(RH, "root")]));
    req.cors = Some(make_cors(SharingMode::Private, vec!["https://root.com"]));
    svc.create_upstream(&test_ctx(root), req).await.unwrap();

    let child_ctx = test_ctx(child);
    let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();

    // The child binds without limit, plugins or CORS of its own.
    let child_up = svc
        .create_upstream(&child_ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    let (effective, _) = svc
        .resolve_alias(&child_ctx, &chain, &child_up.alias, None, Tags::Load)
        .await
        .unwrap();
    assert!(effective.auth.is_some(), "the shared auth applies");
    assert!(effective.rate_limit.is_none(), "{:?}", effective.rate_limit);
    assert!(effective.plugins.is_none(), "{:?}", effective.plugins);
    assert!(effective.cors.is_none(), "{:?}", effective.cors);

    // Its own `inherit` limit and plugins are not merged with the private ones.
    let mut update = make_update_from_upstream(&child_up);
    update.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 1000, Window::Minute));
    update.plugins = Some(plugins_of(SharingMode::Inherit, &[(RID, "child")]));
    update.cors = Some(make_cors(SharingMode::Inherit, vec!["https://child.com"]));
    svc.update_upstream(&child_ctx, child_up.id, update)
        .await
        .unwrap();
    let (effective, _) = svc
        .resolve_alias(&child_ctx, &chain, &child_up.alias, None, Tags::Load)
        .await
        .unwrap();
    assert_eq!(effective.rate_limit.map(|r| r.sustained.rate), Some(1000));
    assert_eq!(
        plugin_refs(effective.plugins.as_ref()),
        vec![(RID, "child")]
    );
    assert_eq!(
        effective.cors.map(|c| c.allowed_origins),
        Some(vec!["https://child.com".to_owned()])
    );
}

/// An ancestor's route serves its descendants with its shared settings only:
/// its private plugins and limit are neither merged nor returned.
#[tokio::test]
async fn an_ancestor_routes_private_plugins_and_limit_stay_with_its_owner() {
    let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    let root_ctx = test_ctx(root);
    let mut req = make_create_upstream_hostname();
    req.auth = Some(noop_auth(SharingMode::Inherit));
    let root_up = svc.create_upstream(&root_ctx, req).await.unwrap();
    let mut route_req = make_create_route(root_up.id);
    route_req.plugins = Some(plugins_of(SharingMode::Private, &[(RH, "root-route")]));
    route_req.rate_limit = Some(make_rate_limit(SharingMode::Private, 1, Window::Minute));
    svc.create_route(&root_ctx, route_req).await.unwrap();

    let root_chain = svc.build_tenant_chain(&root_ctx).await.unwrap();
    let (own, own_route) = svc
        .resolve_alias(
            &root_ctx,
            &root_chain,
            &root_up.alias,
            Some(("POST", CHAT_PATH)),
            Tags::Load,
        )
        .await
        .unwrap();
    assert_eq!(plugin_refs(own.plugins.as_ref()), vec![(RH, "root-route")]);
    assert!(
        own_route.unwrap().rate_limit.is_some(),
        "the owner keeps it"
    );

    let child_ctx = test_ctx(child);
    let chain = svc.build_tenant_chain(&child_ctx).await.unwrap();
    let check = |(effective, route): (Upstream, Option<Route>)| {
        let route = route.unwrap();
        assert!(effective.plugins.is_none(), "{:?}", effective.plugins);
        assert!(route.plugins.is_none(), "{:?}", route.plugins);
        assert!(route.rate_limit.is_none(), "{:?}", route.rate_limit);
    };

    // Without an upstream of its own, and with one (the merge path).
    check(
        svc.resolve_alias(
            &child_ctx,
            &chain,
            &root_up.alias,
            Some(("POST", CHAT_PATH)),
            Tags::Skip,
        )
        .await
        .unwrap(),
    );
    svc.create_upstream(&child_ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    check(
        svc.resolve_alias(
            &child_ctx,
            &chain,
            &root_up.alias,
            Some(("POST", CHAT_PATH)),
            Tags::Skip,
        )
        .await
        .unwrap(),
    );
}

/// `min` takes the winning level's whole config; the limit stays enforced
/// all the same, so a private leaf below an `inherit` middle level cannot
/// escape it.
#[test]
fn an_enforced_limit_stays_enforced_down_a_chain() {
    let root = make_upstream(
        Uuid::new_v4(),
        "a",
        None,
        Some(make_rate_limit(SharingMode::Enforce, 1000, Window::Minute)),
        None,
        vec![],
    );
    let mid = make_upstream(
        Uuid::new_v4(),
        "a",
        None,
        Some(make_rate_limit(SharingMode::Inherit, 500, Window::Minute)),
        None,
        vec![],
    );
    let leaf = make_upstream(
        Uuid::new_v4(),
        "a",
        None,
        Some(make_rate_limit(SharingMode::Private, 9999, Window::Minute)),
        None,
        vec![],
    );

    let effective = compute_effective_config(&[root.clone(), mid, leaf], None).unwrap();
    let rate_limit = effective.rate_limit.unwrap();
    assert_eq!(rate_limit.sustained.rate, 500);
    assert_eq!(rate_limit.sharing, SharingMode::Enforce);

    // A route limit has its own bucket and is not merged in: the upstream's
    // enforced limit is unchanged by it.
    let mut route = route_on(&root, None);
    route.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 100, Window::Minute));
    let effective = compute_effective_config(&[root], Some(&route)).unwrap();
    let rate_limit = effective.rate_limit.unwrap();
    assert_eq!(rate_limit.sustained.rate, 1000);
    assert_eq!(rate_limit.sharing, SharingMode::Enforce);
}

/// An enforced plugin keeps its owner's config: a lower level or a route
/// naming the same `plugin_ref` adds nothing and replaces nothing.
#[test]
fn an_enforced_plugin_cannot_be_replaced_by_the_same_ref() {
    let root = make_upstream(
        Uuid::new_v4(),
        "a",
        None,
        None,
        Some(plugins_of(SharingMode::Enforce, &[(RH, "root")])),
        vec![],
    );
    let child = make_upstream(
        Uuid::new_v4(),
        "a",
        None,
        None,
        Some(plugins_of(
            SharingMode::Inherit,
            &[(RH, "child"), (RID, "child")],
        )),
        vec![],
    );
    let route = route_on(
        &child,
        Some(plugins_of(SharingMode::Inherit, &[(RH, "route")])),
    );

    let effective = compute_effective_config(&[root.clone(), child.clone()], None).unwrap();
    assert_eq!(
        plugin_refs(effective.plugins.as_ref()),
        vec![(RH, "root"), (RID, "child")]
    );
    let effective = compute_effective_config(&[root, child], Some(&route)).unwrap();
    assert_eq!(
        plugin_refs(effective.plugins.as_ref()),
        vec![(RH, "root"), (RID, "child")]
    );
    assert_eq!(effective.plugins.unwrap().sharing, SharingMode::Enforce);
}

/// A private level drops what it inherits, except the enforced items, also
/// when an `inherit` level between them made the effective plugins
/// non-enforced before.
#[test]
fn an_enforced_plugin_survives_a_private_level_below_an_inherit_one() {
    let root = make_upstream(
        Uuid::new_v4(),
        "a",
        None,
        None,
        Some(plugins_of(SharingMode::Enforce, &[(RH, "root")])),
        vec![],
    );
    let mid = make_upstream(
        Uuid::new_v4(),
        "a",
        None,
        None,
        Some(plugins_of(SharingMode::Inherit, &[(RID, "mid")])),
        vec![],
    );
    let leaf = make_upstream(
        Uuid::new_v4(),
        "a",
        None,
        None,
        Some(plugins_of(SharingMode::Private, &[("custom", "leaf")])),
        vec![],
    );

    let effective = compute_effective_config(&[root, mid, leaf], None).unwrap();
    let plugins = effective.plugins.unwrap();
    assert_eq!(
        plugin_refs(Some(&plugins)),
        vec![(RH, "root"), ("custom", "leaf")]
    );
    assert_eq!(plugins.sharing, SharingMode::Enforce);
}

/// A private route keeps the enforced upstream plugins and appends its own;
/// it still replaces the ones nobody enforces.
#[test]
fn a_private_route_keeps_enforced_upstream_plugins() {
    let t = Uuid::new_v4();
    let enforced = make_upstream(
        t,
        "a",
        None,
        None,
        Some(plugins_of(SharingMode::Enforce, &[(RH, "upstream")])),
        vec![],
    );
    let route = route_on(
        &enforced,
        Some(plugins_of(SharingMode::Private, &[(RID, "route")])),
    );
    let effective =
        compute_effective_config(std::slice::from_ref(&enforced), Some(&route)).unwrap();
    let plugins = effective.plugins.unwrap();
    assert_eq!(
        plugin_refs(Some(&plugins)),
        vec![(RH, "upstream"), (RID, "route")]
    );
    assert_eq!(plugins.sharing, SharingMode::Enforce);

    let inherited = make_upstream(
        t,
        "a",
        None,
        None,
        Some(plugins_of(SharingMode::Inherit, &[(RH, "upstream")])),
        vec![],
    );
    let route = route_on(
        &inherited,
        Some(plugins_of(SharingMode::Private, &[(RID, "route")])),
    );
    let effective =
        compute_effective_config(std::slice::from_ref(&inherited), Some(&route)).unwrap();
    assert_eq!(
        plugin_refs(effective.plugins.as_ref()),
        vec![(RID, "route")]
    );
}

/// A child's route shadowing the parent's for the same request keeps the
/// parent route's enforced plugins (not replaced by the same `plugin_ref`)
/// and its enforced limit. The limit is returned on its own, for the parent
/// route's bucket: the effective (upstream) limit is the one the upstreams
/// give, whatever route matched. What the parent route only shares with
/// `inherit` does not apply.
#[test]
fn a_shadowed_ancestor_route_applies_only_its_enforced_settings() {
    use crate::domain::model::ShadowedRouteLimit;

    let root = make_upstream(
        Uuid::new_v4(),
        "a",
        None,
        Some(make_rate_limit(SharingMode::Inherit, 1000, Window::Minute)),
        None,
        vec![],
    );
    let child = make_upstream(Uuid::new_v4(), "a", None, None, None, vec![]);
    let chain = [root.clone(), child.clone()];
    let mut child_route = route_on(
        &child,
        Some(plugins_of(
            SharingMode::Inherit,
            &[(RH, "child-route"), (RID, "child-route")],
        )),
    );
    child_route.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 100, Window::Minute));
    let merge = |shadowed: &[Route]| {
        super::merge::compute_effective_config_reporting(
            &chain,
            Some(&child_route),
            shadowed,
            &mut |_| {},
            &mut |_| {},
        )
        .unwrap()
    };
    let upstream_limit = merge::effective_rate_limit(&chain);
    assert_eq!(
        upstream_limit.as_ref().map(|r| r.sustained.rate),
        Some(1000)
    );

    let mut root_route = route_on(
        &root,
        Some(plugins_of(SharingMode::Enforce, &[(RH, "root-route")])),
    );
    let enforced = make_rate_limit(SharingMode::Enforce, 10, Window::Minute);
    root_route.rate_limit = Some(enforced.clone());
    let effective = merge(std::slice::from_ref(&root_route));
    let plugins = effective.plugins.unwrap();
    assert_eq!(
        plugin_refs(Some(&plugins)),
        vec![(RH, "root-route"), (RID, "child-route")]
    );
    assert_eq!(plugins.sharing, SharingMode::Enforce);
    assert_eq!(effective.rate_limit, upstream_limit);
    assert_eq!(
        merge::shadowed_route_limits(std::slice::from_ref(&root_route)),
        vec![ShadowedRouteLimit {
            route_id: root_route.id,
            rate_limit: enforced,
        }]
    );

    root_route.plugins = Some(plugins_of(
        SharingMode::Inherit,
        &[("custom", "root-route")],
    ));
    root_route.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 1, Window::Minute));
    let effective = merge(std::slice::from_ref(&root_route));
    assert_eq!(
        plugin_refs(effective.plugins.as_ref()),
        vec![(RH, "child-route"), (RID, "child-route")]
    );
    assert_eq!(effective.rate_limit, upstream_limit);
    assert!(merge::shadowed_route_limits(std::slice::from_ref(&root_route)).is_empty());
}

/// An ancestor route's enforced plugin applies at its owner's level: a
/// descendant enforcing the same `plugin_ref` on its own upstream does not
/// pin its config first, whether the descendant's route shadows the
/// ancestor's or the ancestor's route is the one matched.
#[test]
fn an_ancestor_routes_enforced_plugin_beats_a_descendants_enforce() {
    let root = make_upstream(Uuid::new_v4(), "a", None, None, None, vec![]);
    let child = make_upstream(
        Uuid::new_v4(),
        "a",
        None,
        None,
        Some(plugins_of(
            SharingMode::Enforce,
            &[(RH, "child"), (RID, "child")],
        )),
        vec![],
    );
    let root_route = route_on(
        &root,
        Some(plugins_of(SharingMode::Enforce, &[(RH, "root-route")])),
    );
    let child_route = route_on(
        &child,
        Some(plugins_of(SharingMode::Inherit, &[(RH, "child-route")])),
    );
    let chain = [root, child];

    let shadowing = super::merge::compute_effective_config_reporting(
        &chain,
        Some(&child_route),
        std::slice::from_ref(&root_route),
        &mut |_| {},
        &mut |_| {},
    )
    .unwrap();
    let matched = compute_effective_config(&chain, Some(&root_route)).unwrap();
    for (name, effective) in [("shadowing", shadowing), ("matched", matched)] {
        let plugins = effective.plugins.unwrap();
        assert_eq!(
            plugin_refs(Some(&plugins)),
            vec![(RH, "root-route"), (RID, "child")],
            "{name}"
        );
        assert_eq!(plugins.sharing, SharingMode::Enforce, "{name}");
    }
}

/// For a `plugin_ref` enforced at several levels, the level closest to the
/// root keeps its config. Levels run root-first; at each tenant its
/// upstream, then its route. Non-enforcing levels above do not count.
#[test]
fn the_enforcing_level_closest_to_the_root_wins() {
    let upstream = |plugins| make_upstream(Uuid::new_v4(), "a", None, None, plugins, vec![]);
    let enforce = |by| Some(plugins_of(SharingMode::Enforce, &[(RH, by)]));
    let merge = |chain: &[Upstream; 3], routes: [Option<PluginsConfig>; 3]| {
        let [root_route, mid_route, leaf_route] = routes;
        let shadowed: Vec<Route> = [(&chain[0], root_route), (&chain[1], mid_route)]
            .into_iter()
            .filter_map(|(up, plugins)| plugins.map(|p| route_on(up, Some(p))))
            .collect();
        let route = route_on(&chain[2], leaf_route);
        let effective = super::merge::compute_effective_config_reporting(
            chain,
            Some(&route),
            &shadowed,
            &mut |_| {},
            &mut |_| {},
        )
        .unwrap();
        plugin_refs(effective.plugins.as_ref())
            .into_iter()
            .map(|(r, by)| (r.to_owned(), by.to_owned()))
            .collect::<Vec<_>>()
    };
    let wins = |by: &str| vec![(RH.to_owned(), by.to_owned())];
    let all_routes = || {
        [
            enforce("root-route"),
            enforce("mid-route"),
            enforce("leaf-route"),
        ]
    };

    let chain = [
        upstream(Some(plugins_of(SharingMode::Inherit, &[(RH, "root")]))),
        upstream(enforce("mid")),
        upstream(enforce("leaf")),
    ];
    assert_eq!(merge(&chain, all_routes()), wins("root-route"));
    assert_eq!(
        merge(&chain, [None, enforce("mid-route"), enforce("leaf-route")]),
        wins("mid")
    );

    let chain = [
        upstream(enforce("root")),
        upstream(enforce("mid")),
        upstream(enforce("leaf")),
    ];
    assert_eq!(merge(&chain, all_routes()), wins("root"));

    let chain = [upstream(None), upstream(None), upstream(enforce("leaf"))];
    assert_eq!(
        merge(&chain, [None, enforce("mid-route"), enforce("leaf-route")]),
        wins("mid-route")
    );
}

/// Resolution finds the routes a child's route shadows, through the proxy
/// configuration cache and the repositories alike: the parent's enforced
/// route settings apply on the child's path (its rate limit returned apart,
/// for the parent route's bucket), its `inherit` ones do not, and an encoded
/// separator that would select the parent's enforced route is refused.
async fn a_childs_route_does_not_lift_a_shadowed_enforced_route(backend: Backend) {
    let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
    let stores = Stores::new(backend);
    let (cached, uncached, _) = cached_and_uncached(&stores, &[root, child]);
    let on_path = |upstream_id, path: &str, plugins, rate_limit| {
        let mut req = make_create_route(upstream_id);
        if let Some(http) = req.match_rules.http.as_mut() {
            http.path = path.into();
        }
        req.plugins = Some(plugins);
        req.rate_limit = Some(rate_limit);
        req
    };

    let root_ctx = test_ctx(root);
    let mut req = make_create_upstream_hostname();
    req.auth = Some(noop_auth(SharingMode::Inherit));
    let root_up = uncached.create_upstream(&root_ctx, req).await.unwrap();
    let mut root_routes = Vec::new();
    for (path, sharing) in [
        (CHAT_PATH, SharingMode::Enforce),
        ("/v1/embeddings", SharingMode::Inherit),
        ("/v2/admin", SharingMode::Enforce),
    ] {
        let route = on_path(
            root_up.id,
            path,
            plugins_of(sharing, &[(RH, "root-route")]),
            make_rate_limit(sharing, 10, Window::Minute),
        );
        root_routes.push(uncached.create_route(&root_ctx, route).await.unwrap());
    }

    let child_ctx = test_ctx(child);
    let child_up = uncached
        .create_upstream(&child_ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    let mut child_routes = Vec::new();
    for path in [CHAT_PATH, "/v1/embeddings", "/v2"] {
        let route = on_path(
            child_up.id,
            path,
            plugins_of(SharingMode::Inherit, &[(RID, "child-route")]),
            make_rate_limit(SharingMode::Inherit, 100, Window::Minute),
        );
        child_routes.push(uncached.create_route(&child_ctx, route).await.unwrap().id);
    }

    let grant = crate::domain::services::ProxyGrant::for_tests(child_ctx.clone());
    for (name, svc) in [("cached", &cached), ("uncached", &uncached)] {
        for tags in [Tags::Skip, Tags::Load] {
            let resolve = async |path| {
                svc.resolve_proxy_target(&grant, &root_up.alias, "POST", path, tags)
                    .await
            };

            let target = resolve(CHAT_PATH).await.unwrap();
            assert_eq!(target.route.id, child_routes[0], "{name}");
            assert_eq!(
                plugin_refs(target.upstream.plugins.as_ref()),
                vec![(RH, "root-route"), (RID, "child-route")],
                "{name} {tags:?}"
            );
            // Neither the child's route limit nor the parent route's
            // enforced one is merged into the upstream's.
            assert_eq!(target.upstream.rate_limit, None, "{name} {tags:?}");
            let shadowed: Vec<_> = target
                .shadowed_route_limits
                .iter()
                .map(|s| {
                    (
                        s.route_id,
                        s.rate_limit.sustained.rate,
                        s.rate_limit.sharing,
                    )
                })
                .collect();
            assert_eq!(
                shadowed,
                [(root_routes[0].id, 10, SharingMode::Enforce)],
                "{name} {tags:?}"
            );

            let target = resolve("/v1/embeddings").await.unwrap();
            assert_eq!(target.route.id, child_routes[1], "{name}");
            assert_eq!(
                plugin_refs(target.upstream.plugins.as_ref()),
                vec![(RID, "child-route")],
                "{name} {tags:?}"
            );
            assert_eq!(target.upstream.rate_limit, None, "{name} {tags:?}");
            assert!(target.shadowed_route_limits.is_empty(), "{name} {tags:?}");

            let err = resolve("/v2%2Fadmin").await.unwrap_err();
            assert!(
                matches!(&err, DomainError::Validation { reason, instance, .. }
                    if *reason == oagw_sdk::field::INVALID_PROXY_PATH
                        && instance == "/v2%2Fadmin"),
                "{name} {tags:?}: {err:?}"
            );
            let target = resolve("/v2/x").await.unwrap();
            assert_eq!(target.route.id, child_routes[2], "{name}");
            assert!(target.shadowed_route_limits.is_empty(), "{name} {tags:?}");
        }
    }
}

mod a_childs_route_does_not_lift_a_shadowed_enforced_route {
    use super::{Backend, a_childs_route_does_not_lift_a_shadowed_enforced_route as case};

    #[tokio::test]
    async fn in_memory() {
        case(Backend::Memory).await;
    }

    #[tokio::test]
    async fn sqlite() {
        case(Backend::Db(
            crate::infra::storage::testing::shared_sqlite().await,
        ))
        .await;
    }
}

/// A child that enforces the parent route's guard `plugin_ref` on its own
/// upstream binding does not replace the parent route's config, through the
/// proxy configuration cache and the repositories alike: neither with a
/// route of its own on the path (shadowing) nor without one (the parent's
/// route matched).
async fn a_childs_enforced_binding_does_not_replace_an_ancestor_routes_guard(backend: Backend) {
    let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
    let stores = Stores::new(backend);
    let (cached, uncached, _) = cached_and_uncached(&stores, &[root, child]);
    let on_path = |upstream_id, path: &str, plugins| {
        let mut req = make_create_route(upstream_id);
        if let Some(http) = req.match_rules.http.as_mut() {
            http.path = path.into();
        }
        req.plugins = Some(plugins);
        req
    };

    let root_ctx = test_ctx(root);
    let mut req = make_create_upstream_hostname();
    req.auth = Some(noop_auth(SharingMode::Inherit));
    let root_up = uncached.create_upstream(&root_ctx, req).await.unwrap();
    for path in [CHAT_PATH, "/v1/embeddings"] {
        uncached
            .create_route(
                &root_ctx,
                on_path(
                    root_up.id,
                    path,
                    plugins_of(SharingMode::Enforce, &[(RH, "root-route")]),
                ),
            )
            .await
            .unwrap();
    }

    let child_ctx = test_ctx(child);
    let mut binding = make_create_upstream_hostname();
    binding.plugins = Some(plugins_of(SharingMode::Enforce, &[(RH, "child")]));
    let child_up = uncached.create_upstream(&child_ctx, binding).await.unwrap();
    let child_route = uncached
        .create_route(
            &child_ctx,
            on_path(
                child_up.id,
                CHAT_PATH,
                plugins_of(SharingMode::Inherit, &[(RID, "child-route")]),
            ),
        )
        .await
        .unwrap();

    let chain = uncached.build_tenant_chain(&child_ctx).await.unwrap();
    for (name, svc) in [("cached", &cached), ("uncached", &uncached)] {
        for tags in [Tags::Skip, Tags::Load] {
            let resolve = async |path| {
                svc.resolve_alias(
                    &child_ctx,
                    &chain,
                    &root_up.alias,
                    Some(("POST", path)),
                    tags,
                )
                .await
                .unwrap()
            };

            let (effective, route) = resolve(CHAT_PATH).await;
            assert_eq!(route.map(|r| r.id), Some(child_route.id), "{name}");
            assert_eq!(
                plugin_refs(effective.plugins.as_ref()),
                vec![(RH, "root-route"), (RID, "child-route")],
                "{name} {tags:?}"
            );

            let (effective, route) = resolve("/v1/embeddings").await;
            assert_eq!(
                route.map(|r| r.upstream_id),
                Some(root_up.id),
                "{name} {tags:?}"
            );
            assert_eq!(
                plugin_refs(effective.plugins.as_ref()),
                vec![(RH, "root-route")],
                "{name} {tags:?}"
            );
        }
    }
}

mod a_childs_enforced_binding_does_not_replace_an_ancestor_routes_guard {
    use super::{
        Backend, a_childs_enforced_binding_does_not_replace_an_ancestor_routes_guard as case,
    };

    #[tokio::test]
    async fn in_memory() {
        case(Backend::Memory).await;
    }

    #[tokio::test]
    async fn sqlite() {
        case(Backend::Db(
            crate::infra::storage::testing::shared_sqlite().await,
        ))
        .await;
    }
}

/// An enforcing grand-ancestor refuses a leaf's auth or CORS override
/// although the closest ancestor shares the field with `inherit`. A rate limit
/// is refused only under an enforcing closest ancestor: under a farther one
/// the binding is accepted and the merge caps it.
#[tokio::test]
async fn overrides_under_an_enforcing_grand_ancestor() {
    let (root, mid, leaf) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let resolver = MockTenantResolverClient::with_hierarchy(vec![
        TenantId(root),
        TenantId(mid),
        TenantId(leaf),
    ]);
    let svc = make_service_with_resolver(resolver);

    // The middle tenant binds first, so the root's enforce does not refuse it.
    let mut mid_req = make_create_upstream_ip("svc");
    mid_req.auth = Some(noop_auth(SharingMode::Inherit));
    mid_req.rate_limit = Some(make_rate_limit(SharingMode::Private, 50, Window::Minute));
    mid_req.cors = Some(make_cors(SharingMode::Inherit, vec!["https://mid.com"]));
    svc.create_upstream(&test_ctx(mid), mid_req).await.unwrap();
    let mut root_req = make_create_upstream_ip("svc");
    root_req.auth = Some(noop_auth(SharingMode::Enforce));
    root_req.rate_limit = Some(make_rate_limit(SharingMode::Enforce, 100, Window::Minute));
    root_req.cors = Some(make_cors(SharingMode::Enforce, vec!["https://root.com"]));
    svc.create_upstream(&test_ctx(root), root_req)
        .await
        .unwrap();

    let leaf_ctx = test_ctx(leaf);
    let (mut auth, mut cors) = (
        make_create_upstream_ip("svc"),
        make_create_upstream_ip("svc"),
    );
    auth.auth = Some(noop_auth(SharingMode::Private));
    cors.cors = Some(make_cors(SharingMode::Private, vec!["https://leaf.com"]));
    for (field, req) in [("auth", auth), ("cors", cors)] {
        let err = svc.create_upstream(&leaf_ctx, req).await.unwrap_err();
        assert!(
            matches!(&err, DomainError::Validation { detail, .. }
            if detail == &format!(
                "cannot override {field}: ancestor upstream has sharing mode 'enforce'"
            )),
            "{field}: {err:?}"
        );
    }

    let mut rate_limit = make_create_upstream_ip("svc");
    rate_limit.rate_limit = Some(make_rate_limit(SharingMode::Private, 9999, Window::Minute));
    let leaf_up = svc.create_upstream(&leaf_ctx, rate_limit).await.unwrap();
    let chain = svc.build_tenant_chain(&leaf_ctx).await.unwrap();
    let (effective, _) = svc
        .resolve_alias(&leaf_ctx, &chain, &leaf_up.alias, None, Tags::Load)
        .await
        .unwrap();
    let rate_limit = effective.rate_limit.unwrap();
    assert_eq!(
        (rate_limit.sustained.rate, rate_limit.sharing),
        (100, SharingMode::Enforce)
    );
}

/// Regression: a leaf allocates from its `inherit` parent's allocated budget
/// under a root that enforces its limit (the budget check needs an explicit
/// rate limit, so the leaf has to set one). Create and update are accepted,
/// the budget still applies, and the root's enforce stays on the result.
#[tokio::test]
async fn allocated_budget_child_binds_under_an_enforcing_grand_ancestor() {
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let (root, mid, leaf) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let resolver = MockTenantResolverClient::with_hierarchy(vec![
        TenantId(root),
        TenantId(mid),
        TenantId(leaf),
    ]);
    let svc = make_service_with_resolver(resolver);

    // The middle tenant binds first, so the root's enforce does not refuse it.
    let mut mid_req = make_create_upstream_ip("svc");
    let mut mid_limit = make_rate_limit(SharingMode::Inherit, 500, Window::Minute);
    mid_limit.budget = Some(BudgetConfig {
        mode: BudgetMode::Allocated,
        total: Some(500),
        overcommit_ratio: None,
    });
    mid_req.rate_limit = Some(mid_limit);
    svc.create_upstream(&test_ctx(mid), mid_req).await.unwrap();
    let mut root_req = make_create_upstream_ip("svc");
    root_req.rate_limit = Some(make_rate_limit(SharingMode::Enforce, 1000, Window::Minute));
    svc.create_upstream(&test_ctx(root), root_req)
        .await
        .unwrap();

    let leaf_ctx = test_ctx(leaf);
    let effective_limit = async || {
        let chain = svc.build_tenant_chain(&leaf_ctx).await.unwrap();
        let (effective, _) = svc
            .resolve_alias(&leaf_ctx, &chain, "svc", None, Tags::Load)
            .await
            .unwrap();
        let rate_limit = effective.rate_limit.unwrap();
        (rate_limit.sustained.rate, rate_limit.sharing)
    };

    let mut req = make_create_upstream_ip("svc");
    req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 100, Window::Minute));
    let leaf_up = svc.create_upstream(&leaf_ctx, req).await.unwrap();
    assert_eq!(effective_limit().await, (100, SharingMode::Enforce));

    let mut update = make_update_from_upstream(&leaf_up);
    update.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 200, Window::Minute));
    svc.update_upstream(&leaf_ctx, leaf_up.id, update)
        .await
        .unwrap();
    assert_eq!(effective_limit().await, (200, SharingMode::Enforce));

    let mut update = make_update_from_upstream(&leaf_up);
    update.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 600, Window::Minute));
    let err = svc
        .update_upstream(&leaf_ctx, leaf_up.id, update)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::Validation { detail, .. }
            if detail.starts_with("budget allocation exceeded")),
        "{err:?}"
    );
    assert_eq!(effective_limit().await, (200, SharingMode::Enforce));
}

// -- Ancestor bind validation tests --

#[tokio::test]
async fn bind_rejects_cors_override_on_enforce() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root upstream with enforced CORS.
    let root_ctx = test_ctx(root);
    let mut req = make_create_upstream_hostname();
    req.cors = Some(CorsConfig {
        sharing: SharingMode::Enforce,
        enabled: true,
        allowed_origins: vec!["https://locked.com".into()],
        allowed_methods: vec![CorsHttpMethod::Get],
        expose_headers: vec![],
        allow_credentials: false,
    });
    svc.create_upstream(&root_ctx, req).await.unwrap();

    // Child attempts to override CORS → should fail.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.cors = Some(make_cors(SharingMode::Inherit, vec!["https://child.com"]));
    let result = svc.create_upstream(&child_ctx, child_req).await;
    assert!(
        result.is_err(),
        "CORS override on enforce should be rejected"
    );
}

#[tokio::test]
async fn bind_allows_cors_override_on_private() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root upstream with private CORS but shared auth (so it's visible to descendants).
    let root_ctx = test_ctx(root);
    let mut req = make_create_upstream_hostname();
    req.cors = Some(make_cors(SharingMode::Private, vec!["https://private.com"]));
    req.auth = Some(AuthConfig {
        sharing: SharingMode::Inherit,
        plugin_type: crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID.into(),
        config: None,
    });
    svc.create_upstream(&root_ctx, req).await.unwrap();

    // Child overrides CORS → allowed (Private means "not shared", descendant can shadow).
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.cors = Some(make_cors(SharingMode::Inherit, vec!["https://child.com"]));
    let result = svc.create_upstream(&child_ctx, child_req).await;
    assert!(
        result.is_ok(),
        "CORS override on private should be allowed (shadow): {result:?}"
    );
}

#[tokio::test]
async fn bind_rejects_auth_override_on_enforce() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Create upstream in root with auth sharing = enforce.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Enforce,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child tries to create upstream with same host (same derived alias) AND auth override.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let err = svc
        .create_upstream(&child_ctx, child_req)
        .await
        .unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("enforce"),
                "expected enforce error, got: {detail}"
            );
        }
        _ => panic!("expected Validation error, got: {err:?}"),
    }
}

#[tokio::test]
async fn bind_allows_rate_limit_override_on_private() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Create upstream in root with rate_limit sharing = private.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.rate_limit = Some(make_rate_limit(SharingMode::Private, 100, Window::Minute));
    // Need at least one non-private field so root upstream is visible.
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child overrides rate_limit → allowed (Private means "not shared", descendant can shadow).
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 50, Window::Minute));
    let result = svc.create_upstream(&child_ctx, child_req).await;
    assert!(
        result.is_ok(),
        "rate_limit override on private should be allowed (shadow): {result:?}"
    );
}

#[tokio::test]
async fn bind_allows_inherit_override_with_permissions() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Create upstream in root with inherit sharing.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child creates upstream with same host (same derived alias) and overrides auth.
    // With allow-all enforcer, bind + override_auth permissions pass.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let child_upstream = svc.create_upstream(&child_ctx, child_req).await.unwrap();
    assert_eq!(child_upstream.alias, "api.openai.com");
    assert_eq!(
        child_upstream.auth.unwrap().plugin_type,
        crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID
    );
}

#[tokio::test]
async fn bind_no_ancestor_match_creates_normally() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // No upstream in root. Child creates fresh upstream — no permission checks needed.
    let child_ctx = test_ctx(child);
    let child_upstream = svc
        .create_upstream(&child_ctx, make_create_upstream_ip("fresh-alias"))
        .await
        .unwrap();
    assert_eq!(child_upstream.alias, "fresh-alias");
}

// -- Secret ref validation tests --

fn auth_with_secret_ref(secret_ref: &str) -> AuthConfig {
    let mut config = std::collections::HashMap::new();
    config.insert("header".into(), "authorization".into());
    config.insert("secret_ref".into(), secret_ref.into());
    AuthConfig {
        plugin_type: crate::domain::gts_helpers::APIKEY_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: Some(config),
    }
}

#[tokio::test]
async fn bind_rejects_inaccessible_secret_ref() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    // No secrets in credstore.
    let svc = make_service_with_resolver_and_creds(resolver, vec![]);

    // Root upstream with auth inherit.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child tries to bind with a secret_ref the credstore doesn't have.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.auth = Some(auth_with_secret_ref("cred://missing-key"));
    let err = svc
        .create_upstream(&child_ctx, child_req)
        .await
        .unwrap_err();
    match err {
        // A state precondition (retryable by provisioning callers), not a
        // malformed-argument validation failure.
        DomainError::SecretRefNotAccessible { detail, .. } => {
            assert!(
                detail.contains("not accessible"),
                "expected 'not accessible' error, got: {detail}"
            );
            // The ref is a credential name: keep it out of errors (F0008).
            assert!(!detail.contains("missing-key"), "{detail}");
        }
        _ => panic!("expected SecretRefNotAccessible error, got: {err:?}"),
    }
}

/// A client implementation that reports the not-found surface as an error
/// (`Err(CredStoreError::NotFound)`) instead of `Ok(None)` — e.g. a resolved
/// metadata row whose value is still being provisioned — must classify the
/// same way: a retryable state precondition, not a misconfiguration.
#[tokio::test]
async fn bind_maps_not_found_error_to_secret_ref_not_accessible() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = ControlPlaneServiceImpl::new(
        Arc::new(InMemoryUpstreamRepo::new()),
        Arc::new(InMemoryRouteRepo::new()),
        Arc::new(resolver),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::erroring_not_found()),
        Arc::new(SsrfGuard::disabled()),
        None,
    );

    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.auth = Some(auth_with_secret_ref("cred://missing-key"));
    let err = svc
        .create_upstream(&child_ctx, child_req)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::SecretRefNotAccessible { .. }),
        "expected SecretRefNotAccessible, got: {err:?}"
    );
}

/// A credential-store outage is the gateway's problem, not the request's:
/// `Internal` (500), so the registry reconcile treats it as fatal instead of
/// skipping every upstream with a `secret_ref`. Any other credential-store
/// refusal stays a validation error (400).
///
/// No path writes the `secret_ref` to a log or an error (F0008), also when
/// the credential store's error quotes it.
#[tokio::test]
#[tracing_test::traced_test]
async fn secret_ref_check_maps_a_credstore_outage_to_internal() {
    use credstore_sdk::CredStoreError;
    type MakeError = fn() -> CredStoreError;

    let cases: [(MakeError, bool); 5] = [
        (
            || CredStoreError::Internal("store failed reading my-key".into()),
            true,
        ),
        (
            || CredStoreError::ServiceUnavailable {
                detail: "my-key: backend down".into(),
                retry_after: None,
            },
            true,
        ),
        (|| CredStoreError::NoPluginAvailable, true),
        (|| CredStoreError::AccessDenied, false),
        (
            || CredStoreError::InvalidSecretRef {
                reason: "my-key is malformed".into(),
            },
            false,
        ),
    ];
    for (error, outage) in cases {
        let svc = ControlPlaneServiceImpl::new(
            Arc::new(InMemoryUpstreamRepo::new()),
            Arc::new(InMemoryRouteRepo::new()),
            Arc::new(MockTenantResolverClient::single_tenant()),
            allow_all_enforcer(),
            Arc::new(crate::domain::test_support::ErroringCredStore(error)),
            Arc::new(SsrfGuard::disabled()),
            None,
        );
        let mut req = make_create_upstream_hostname();
        req.auth = Some(auth_with_secret_ref("cred://my-key"));
        let err = svc
            .create_upstream(&test_ctx(Uuid::new_v4()), req)
            .await
            .unwrap_err();
        if outage {
            assert!(matches!(err, DomainError::Internal { .. }), "{err:?}");
        } else {
            match &err {
                DomainError::Validation { field, reason, .. } => {
                    assert_eq!(*field, "auth.config.secret_ref");
                    assert_eq!(*reason, oagw_sdk::field::VALIDATION);
                }
                other => panic!("expected a validation error, got {other:?}"),
            }
        }
        let text = match &err {
            DomainError::Internal { message } => message,
            DomainError::Validation { detail, .. } => detail,
            other => panic!("unexpected {other:?}"),
        };
        assert!(!text.contains("my-key"), "{text}");
    }
    assert!(logs_contain("cred_store unavailable"));
    assert!(!logs_contain("my-key"));
}

/// A malformed `secret_ref` is a pure check: rejected before the credential
/// store is asked.
#[tokio::test]
async fn malformed_secret_ref_is_rejected_before_the_credstore_is_asked() {
    let svc = ControlPlaneServiceImpl::new(
        Arc::new(InMemoryUpstreamRepo::new()),
        Arc::new(InMemoryRouteRepo::new()),
        Arc::new(MockTenantResolverClient::single_tenant()),
        allow_all_enforcer(),
        Arc::new(crate::domain::test_support::ErroringCredStore(|| {
            credstore_sdk::CredStoreError::NoPluginAvailable
        })),
        Arc::new(SsrfGuard::disabled()),
        None,
    );
    let mut req = make_create_upstream_hostname();
    req.auth = Some(auth_with_secret_ref("cred://has spaces/and..dots"));
    let err = svc
        .create_upstream(&test_ctx(Uuid::new_v4()), req)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::Validation { detail, .. }
            if detail.contains("invalid credential reference") && !detail.contains("has spaces")),
        "{err:?}"
    );
}

#[tokio::test]
async fn bind_allows_accessible_secret_ref() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver_and_creds(
        resolver,
        vec![("my-key".into(), "secret-value".into())],
    );

    // Root upstream with auth inherit.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child binds with accessible secret_ref.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.auth = Some(auth_with_secret_ref("cred://my-key"));
    let child_upstream = svc.create_upstream(&child_ctx, child_req).await.unwrap();
    assert_eq!(child_upstream.alias, "api.openai.com");
}

#[tokio::test]
async fn bind_allows_plugins_override_on_enforce() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root upstream with enforced plugins.
    let root_ctx = test_ctx(root);
    let mut req = make_create_upstream_hostname();
    req.plugins = Some(PluginsConfig {
        sharing: SharingMode::Enforce,
        items: vec![PluginBinding {
            plugin_ref: crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID.into(),
            config: HashMap::new(),
        }],
    });
    svc.create_upstream(&root_ctx, req).await.unwrap();

    // Child adds a new plugin → allowed (Enforce protects existing items at merge time,
    // but doesn't block adding new ones at bind time).
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.plugins = Some(PluginsConfig {
        sharing: SharingMode::Inherit,
        items: vec![PluginBinding {
            plugin_ref: crate::domain::gts_helpers::REQUEST_ID_TRANSFORM_PLUGIN_ID.into(),
            config: HashMap::new(),
        }],
    });
    let result = svc.create_upstream(&child_ctx, child_req).await;
    assert!(
        result.is_ok(),
        "plugins override on enforce should be allowed (add new): {result:?}"
    );
}

// -- Update upstream bind validation tests --

#[tokio::test]
async fn update_rejects_auth_override_on_enforce() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root upstream with auth enforce.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Enforce,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child creates upstream with same host (same derived alias, no auth override on create).
    let child_ctx = test_ctx(child);
    let child_upstream = svc
        .create_upstream(&child_ctx, make_create_upstream_hostname())
        .await
        .unwrap();

    // Child tries to update auth — should fail because ancestor is enforce.
    let mut update_req = make_update_from_upstream(&child_upstream);
    update_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let err = svc
        .update_upstream(&child_ctx, child_upstream.id, update_req)
        .await
        .unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("enforce"),
                "expected enforce error, got: {detail}"
            );
        }
        _ => panic!("expected Validation error, got: {err:?}"),
    }
}

#[tokio::test]
async fn create_alias_matching_ancestor_requires_bind() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root upstream with inherit sharing (IP-based for explicit alias control).
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_ip("openai");
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child creates upstream with same alias — with allow-all enforcer this passes
    // (ancestor bind validation is triggered at creation time).
    let child_ctx = test_ctx(child);
    let child_upstream = svc
        .create_upstream(&child_ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();
    assert_eq!(child_upstream.alias, "openai");
}

#[tokio::test]
async fn create_alias_matching_ancestor_enforce_rejects_incompatible_auth() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root upstream with auth enforce (IP-based for explicit alias control).
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_ip("openai");
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Enforce,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child creates upstream with same alias but incompatible auth — must fail
    // because the ancestor's enforce mode forbids a different auth plugin type.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_ip("openai");
    child_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let err = svc
        .create_upstream(&child_ctx, child_req)
        .await
        .unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("enforce"),
                "expected enforce error, got: {detail}"
            );
        }
        _ => panic!("expected Validation error, got: {err:?}"),
    }
}

#[tokio::test]
async fn update_no_ancestor_match_succeeds() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Child creates upstream (IP-based for explicit alias).
    let child_ctx = test_ctx(child);
    let child_upstream = svc
        .create_upstream(&child_ctx, make_create_upstream_ip("my-svc"))
        .await
        .unwrap();

    // Update auth — no ancestor match, should succeed without permission checks.
    let mut update_req = make_update_from_upstream(&child_upstream);
    update_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let updated = svc
        .update_upstream(&child_ctx, child_upstream.id, update_req)
        .await
        .unwrap();
    assert_eq!(
        updated.auth.unwrap().plugin_type,
        crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID
    );
}

/// A full-replacement update is checked against the ancestor whatever it
/// leaves out: a limit under an enforcing ancestor is refused, and removing
/// it is accepted.
#[tokio::test]
async fn update_removing_rate_limit_still_validates_against_ancestor() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root upstream with rate_limit enforce.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.rate_limit = Some(make_rate_limit(SharingMode::Enforce, 1000, Window::Minute));
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // A child limit under the enforced one is refused on create.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 500, Window::Minute));
    let err = svc
        .create_upstream(&child_ctx, child_req)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::Validation { detail, .. } if detail.contains("rate_limit")),
        "{err:?}"
    );

    // Bound without a limit of its own.
    let child_upstream = svc
        .create_upstream(&child_ctx, make_create_upstream_hostname())
        .await
        .unwrap();

    // Setting one on update is refused the same way.
    let mut update_req = make_update_from_upstream(&child_upstream);
    update_req.rate_limit = Some(make_rate_limit(SharingMode::Private, 9999, Window::Minute));
    let err = svc
        .update_upstream(&child_ctx, child_upstream.id, update_req)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::Validation { detail, .. } if detail.contains("rate_limit")),
        "{err:?}"
    );

    // Leaving it out passes and stores none.
    let mut update_req = make_update_from_upstream(&child_upstream);
    update_req.rate_limit = None;
    let updated = svc
        .update_upstream(&child_ctx, child_upstream.id, update_req)
        .await
        .unwrap();
    assert!(updated.rate_limit.is_none());
}

#[tokio::test]
async fn update_with_no_overrides_still_validates_ancestor_auth_enforce() {
    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root upstream with auth enforce.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Enforce,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child creates upstream (no auth override on create — succeeds).
    let child_ctx = test_ctx(child);
    let child_upstream = svc
        .create_upstream(&child_ctx, make_create_upstream_hostname())
        .await
        .unwrap();

    // Update with auth set (override) — must fail due to ancestor enforce.
    // This verifies bind validation runs even when other fields are None.
    let mut update_req = make_update_from_upstream(&child_upstream);
    update_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    update_req.rate_limit = None;
    update_req.plugins = None;
    update_req.cors = None;
    let err = svc
        .update_upstream(&child_ctx, child_upstream.id, update_req)
        .await
        .unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("enforce"),
                "expected enforce error, got: {detail}"
            );
        }
        _ => panic!("expected Validation error, got: {err:?}"),
    }
}

// -- resolve_proxy_target tests --

#[tokio::test]
async fn proxy_target_resolves_route_from_ancestor_upstream() {
    use crate::domain::model::{AuthConfig, SharingMode};

    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root creates upstream with auth inherit (hostname-based, derived alias).
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let root_upstream = svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Root creates a route on that upstream.
    let route_req = CreateRouteRequest {
        id: None,
        upstream_id: root_upstream.id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                path: "/v1/chat".into(),
                methods: vec![HttpMethod::Post],
                query_allowlist: vec![],
                path_suffix_mode: PathSuffixMode::default(),
            }),
            grpc: None,
        },
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
        priority: 0,
        enabled: true,
    };
    let root_route = svc.create_route(&root_ctx, route_req).await.unwrap();

    // Child creates upstream with same host (same derived alias, bind to ancestor).
    let child_ctx = test_ctx(child);
    let _child_upstream = svc
        .create_upstream(&child_ctx, make_create_upstream_hostname())
        .await
        .unwrap();

    // Child resolves proxy target — should find the route defined on
    // the root's upstream ID, not the child's.
    let ProxyTarget {
        upstream: effective,
        route,
        ..
    } = svc
        .resolve_proxy_target(
            &crate::domain::services::ProxyGrant::for_tests(child_ctx.clone()),
            "api.openai.com",
            "POST",
            "/v1/chat",
            Tags::Load,
        )
        .await
        .unwrap();

    assert_eq!(route.id, root_route.id);
    assert_eq!(effective.alias, "api.openai.com");
}

#[tokio::test]
async fn proxy_target_prefers_child_route_over_ancestor() {
    use crate::domain::model::{AuthConfig, SharingMode};

    let root = Uuid::new_v4();
    let child = Uuid::new_v4();
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root creates upstream with auth inherit (hostname-based, derived alias).
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let root_upstream = svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Root creates a route.
    let root_route_req = CreateRouteRequest {
        id: None,
        upstream_id: root_upstream.id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                path: "/v1/chat".into(),
                methods: vec![HttpMethod::Post],
                query_allowlist: vec![],
                path_suffix_mode: PathSuffixMode::default(),
            }),
            grpc: None,
        },
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
        priority: 0,
        enabled: true,
    };
    svc.create_route(&root_ctx, root_route_req).await.unwrap();

    // Child creates upstream with same host (same derived alias).
    let child_ctx = test_ctx(child);
    let child_upstream = svc
        .create_upstream(&child_ctx, make_create_upstream_hostname())
        .await
        .unwrap();

    // Child creates its own route on its own upstream.
    let child_route_req = CreateRouteRequest {
        id: None,
        upstream_id: child_upstream.id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                path: "/v1/chat".into(),
                methods: vec![HttpMethod::Post],
                query_allowlist: vec![],
                path_suffix_mode: PathSuffixMode::default(),
            }),
            grpc: None,
        },
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
        priority: 0,
        enabled: true,
    };
    let child_route = svc.create_route(&child_ctx, child_route_req).await.unwrap();

    // Child resolves — should prefer its own route (child upstream ID checked first).
    let ProxyTarget { route, .. } = svc
        .resolve_proxy_target(
            &crate::domain::services::ProxyGrant::for_tests(child_ctx.clone()),
            "api.openai.com",
            "POST",
            "/v1/chat",
            Tags::Load,
        )
        .await
        .unwrap();

    assert_eq!(route.id, child_route.id);
}

// -- Private sharing (no enforce ancestor) tests --

#[test]
fn merge_auth_private_replaces_when_ancestor_not_enforced() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_auth = AuthConfig {
        plugin_type: crate::domain::gts_helpers::APIKEY_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    };
    let child_auth = AuthConfig {
        plugin_type: crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Private,
        config: None,
    };

    let root = make_upstream(root_id, "openai", Some(root_auth), None, None, vec![]);
    let child = make_upstream(child_id, "openai", Some(child_auth), None, None, vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    // Ancestor is Inherit (not Enforce) — Private descendant replaces.
    let auth = effective.auth.unwrap();
    assert_eq!(
        auth.plugin_type,
        crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID
    );
    assert_eq!(auth.sharing, SharingMode::Private);
}

#[test]
fn route_private_plugins_replace_upstream() {
    let t = Uuid::new_v4();
    let upstream_plugins = PluginsConfig {
        sharing: SharingMode::Inherit,
        items: vec![PluginBinding {
            plugin_ref: "upstream-plugin".into(),
            config: HashMap::new(),
        }],
    };
    let u = make_upstream(t, "openai", None, None, Some(upstream_plugins), vec![]);

    let route = Route {
        id: Uuid::new_v4(),
        tenant_id: t,
        upstream_id: u.id,
        match_rules: MatchRules {
            http: None,
            grpc: None,
        },
        plugins: Some(PluginsConfig {
            sharing: SharingMode::Private,
            items: vec![PluginBinding {
                plugin_ref: "route-plugin".into(),
                config: HashMap::new(),
            }],
        }),
        rate_limit: None,
        cors: None,
        tags: vec![],
        priority: 0,
        enabled: true,
        managed_by: ManagedBy::Api,
    };

    let effective = compute_effective_config(&[u], Some(&route)).unwrap();
    let items = effective.plugins.unwrap().items;
    // Route plugins with Private sharing replace upstream's (local-only).
    assert_eq!(
        items
            .iter()
            .map(|b| b.plugin_ref.as_str())
            .collect::<Vec<_>>(),
        vec!["route-plugin"]
    );
}

#[test]
fn route_private_rate_limit_is_skipped() {
    let t = Uuid::new_v4();
    let upstream_rl = make_rate_limit(SharingMode::Inherit, 100, Window::Minute);
    let u = make_upstream(t, "openai", None, Some(upstream_rl), None, vec![]);

    let route = Route {
        id: Uuid::new_v4(),
        tenant_id: t,
        upstream_id: u.id,
        match_rules: MatchRules {
            http: None,
            grpc: None,
        },
        plugins: None,
        rate_limit: Some(make_rate_limit(SharingMode::Private, 10, Window::Minute)),
        cors: None,
        tags: vec![],
        priority: 0,
        enabled: true,
        managed_by: ManagedBy::Api,
    };

    let effective = compute_effective_config(&[u], Some(&route)).unwrap();
    // Route rate_limit with Private sharing is skipped — upstream rate stays.
    assert_eq!(effective.rate_limit.unwrap().sustained.rate, 100);
}

// -- Defense-in-depth: enforce vs private merge tests --

#[test]
fn merge_rate_limit_private_cannot_bypass_enforced_ancestor() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_rl = make_rate_limit(SharingMode::Enforce, 100, Window::Minute);
    let child_rl = make_rate_limit(SharingMode::Private, 9999, Window::Minute);

    let root = make_upstream(root_id, "openai", None, Some(root_rl), None, vec![]);
    let child = make_upstream(child_id, "openai", None, Some(child_rl), None, vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    // Enforced ancestor rate (100/min) must still constrain even though
    // descendant declared Private with a much higher rate.
    assert_eq!(effective.rate_limit.unwrap().sustained.rate, 100);
}

// -- limit_owner_id: whose limit the effective one is --

fn limit_owner(chain: &[Upstream], route: Option<&Route>) -> Option<Uuid> {
    compute_effective_config(chain, route)
        .unwrap()
        .rate_limit
        .unwrap()
        .limit_owner_id
}

fn rate_limited(sharing: SharingMode, rate: u32) -> Upstream {
    let rl = make_rate_limit(sharing, rate, Window::Minute);
    make_upstream(Uuid::new_v4(), "openai", None, Some(rl), None, vec![])
}

#[test]
fn limit_owner_is_the_level_that_sets_the_limit() {
    let root = rate_limited(SharingMode::Inherit, 1000);
    let parent = rate_limited(SharingMode::Private, 1);
    assert_eq!(
        limit_owner(std::slice::from_ref(&root), None),
        Some(root.id)
    );
    assert_eq!(
        limit_owner(&[root, parent.clone()], None),
        Some(parent.id),
        "a private limit replaces the inherited one"
    );
}

#[test]
fn limit_owner_stays_the_ancestor_when_the_limit_is_inherited_unchanged() {
    let root = rate_limited(SharingMode::Inherit, 1000);
    let no_limit = make_upstream(Uuid::new_v4(), "openai", None, None, None, vec![]);
    assert_eq!(
        limit_owner(&[root.clone(), no_limit.clone()], None),
        Some(root.id)
    );

    // The review case: the child resolves the parent's upstream without the
    // parent's private limit, so the limit in effect is the root's.
    let parent = rate_limited(SharingMode::Private, 1);
    let parent_seen_by_child = super::merge::shared_upstream_view(parent.clone());
    assert_eq!(
        limit_owner(&[root.clone(), parent_seen_by_child], None),
        Some(root.id)
    );
}

#[test]
fn limit_owner_is_the_winner_of_the_min() {
    let loose_root = rate_limited(SharingMode::Inherit, 100);
    let strict_child = rate_limited(SharingMode::Inherit, 10);
    assert_eq!(
        limit_owner(&[loose_root, strict_child.clone()], None),
        Some(strict_child.id)
    );

    let strict_root = rate_limited(SharingMode::Inherit, 10);
    let loose_child = rate_limited(SharingMode::Inherit, 100);
    assert_eq!(
        limit_owner(&[strict_root.clone(), loose_child], None),
        Some(strict_root.id)
    );

    // Under an enforced ancestor a private limit is min-merged too.
    let enforcing_root = rate_limited(SharingMode::Enforce, 100);
    let loose_private = rate_limited(SharingMode::Private, 9999);
    let strict_private = rate_limited(SharingMode::Private, 10);
    assert_eq!(
        limit_owner(&[enforcing_root.clone(), loose_private], None),
        Some(enforcing_root.id)
    );
    assert_eq!(
        limit_owner(&[enforcing_root, strict_private.clone()], None),
        Some(strict_private.id)
    );
}

/// A route's limit has its own bucket and is not merged in, so it changes
/// neither the upstream limit nor its owner.
#[test]
fn a_route_limit_does_not_change_the_limit_owner() {
    let root = rate_limited(SharingMode::Inherit, 1000);
    let child = make_upstream(Uuid::new_v4(), "openai", None, None, None, vec![]);
    let strict = |upstream: &Upstream| {
        let mut r = route_on(upstream, None);
        r.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 1, Window::Minute));
        r
    };

    assert_eq!(
        limit_owner(&[root.clone(), child.clone()], Some(&strict(&root))),
        Some(root.id)
    );
    // No upstream-level limit: none applies to the upstream bucket.
    let effective =
        compute_effective_config(std::slice::from_ref(&child), Some(&strict(&child))).unwrap();
    assert_eq!(effective.rate_limit, None);
}

#[test]
fn merge_auth_private_cannot_bypass_enforced_ancestor() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_auth = AuthConfig {
        plugin_type: crate::domain::gts_helpers::APIKEY_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Enforce,
        config: None,
    };
    let child_auth = AuthConfig {
        plugin_type: crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Private,
        config: None,
    };

    let root = make_upstream(root_id, "openai", Some(root_auth), None, None, vec![]);
    let child = make_upstream(child_id, "openai", Some(child_auth), None, None, vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    // Enforced ancestor auth (apikey) must survive even though
    // descendant declared Private with oauth2.
    assert_eq!(
        effective.auth.unwrap().plugin_type,
        crate::domain::gts_helpers::APIKEY_AUTH_PLUGIN_ID
    );
}

#[test]
fn merge_plugins_private_cannot_drop_enforced_ancestor_plugins() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_plugins = PluginsConfig {
        sharing: SharingMode::Enforce,
        items: vec![PluginBinding {
            plugin_ref: crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID.into(),
            config: HashMap::new(),
        }],
    };
    let child_plugins = PluginsConfig {
        sharing: SharingMode::Private,
        items: vec![PluginBinding {
            plugin_ref: "my-plugin".into(),
            config: HashMap::new(),
        }],
    };

    let root = make_upstream(root_id, "openai", None, None, Some(root_plugins), vec![]);
    let child = make_upstream(child_id, "openai", None, None, Some(child_plugins), vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    let items = effective.plugins.unwrap().items;
    // Enforced crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID must survive even though descendant set Private.
    assert!(
        items
            .iter()
            .any(|b| b.plugin_ref == crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID)
    );
    assert!(items.iter().any(|b| b.plugin_ref == "my-plugin"));
}

#[test]
fn merge_plugins_enforced_ancestor_config_wins_on_ref_conflict() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_plugins = PluginsConfig {
        sharing: SharingMode::Enforce,
        items: vec![PluginBinding {
            plugin_ref: crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID.into(),
            config: HashMap::from([("level".into(), "full".into())]),
        }],
    };
    let child_plugins = PluginsConfig {
        sharing: SharingMode::Private,
        items: vec![PluginBinding {
            plugin_ref: crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID.into(),
            config: HashMap::from([("level".into(), "minimal".into())]),
        }],
    };

    let root = make_upstream(root_id, "openai", None, None, Some(root_plugins), vec![]);
    let child = make_upstream(child_id, "openai", None, None, Some(child_plugins), vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    let items = effective.plugins.unwrap().items;
    // Should have exactly one audit-log, with ancestor (enforced) config.
    let audit = items
        .iter()
        .filter(|b| b.plugin_ref == crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID)
        .collect::<Vec<_>>();
    assert_eq!(audit.len(), 1, "should dedup by plugin_ref");
    assert_eq!(
        audit[0].config.get("level").unwrap(),
        "full",
        "enforced ancestor config wins"
    );
}

#[test]
fn merge_plugins_inherit_descendant_config_wins_on_ref_conflict() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_plugins = PluginsConfig {
        sharing: SharingMode::Inherit,
        items: vec![PluginBinding {
            plugin_ref: crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID.into(),
            config: HashMap::from([("level".into(), "full".into())]),
        }],
    };
    let child_plugins = PluginsConfig {
        sharing: SharingMode::Inherit,
        items: vec![PluginBinding {
            plugin_ref: crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID.into(),
            config: HashMap::from([("level".into(), "minimal".into())]),
        }],
    };

    let root = make_upstream(root_id, "openai", None, None, Some(root_plugins), vec![]);
    let child = make_upstream(child_id, "openai", None, None, Some(child_plugins), vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    let items = effective.plugins.unwrap().items;
    let audit = items
        .iter()
        .filter(|b| b.plugin_ref == crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID)
        .collect::<Vec<_>>();
    assert_eq!(audit.len(), 1, "should dedup by plugin_ref");
    assert_eq!(
        audit[0].config.get("level").unwrap(),
        "minimal",
        "descendant config wins for inherit"
    );
}

// -- Alias enforcement tests --

#[test]
fn compute_derived_alias_single_hostname_standard_port() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    }];
    assert_eq!(compute_derived_alias(&eps), Some("api.openai.com".into()));
}

#[test]
fn compute_derived_alias_single_hostname_non_standard_port() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 8443,
    }];
    assert_eq!(
        compute_derived_alias(&eps),
        Some("api.openai.com:8443".into())
    );
}

#[test]
fn compute_derived_alias_http_standard_port() {
    let eps = vec![Endpoint {
        scheme: Scheme::Http,
        host: "api.example.com".into(),
        port: 80,
    }];
    assert_eq!(compute_derived_alias(&eps), Some("api.example.com".into()));
}

#[test]
fn compute_derived_alias_grpc_standard_port() {
    let eps = vec![Endpoint {
        scheme: Scheme::Grpc,
        host: "grpc.example.com".into(),
        port: 443,
    }];
    assert_eq!(compute_derived_alias(&eps), Some("grpc.example.com".into()));
}

#[test]
fn compute_derived_alias_multi_host_common_suffix() {
    let eps = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "us.vendor.com".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "eu.vendor.com".into(),
            port: 443,
        },
    ];
    assert_eq!(compute_derived_alias(&eps), Some("vendor.com".into()));
}

#[test]
fn compute_derived_alias_multi_host_common_suffix_non_standard_port() {
    let eps = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "us.vendor.com".into(),
            port: 8443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "eu.vendor.com".into(),
            port: 8443,
        },
    ];
    assert_eq!(compute_derived_alias(&eps), Some("vendor.com:8443".into()));
}

#[test]
fn compute_derived_alias_multi_host_deeper_common_suffix() {
    let eps = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "a.b.vendor.com".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "c.b.vendor.com".into(),
            port: 443,
        },
    ];
    assert_eq!(compute_derived_alias(&eps), Some("b.vendor.com".into()));
}

#[test]
fn compute_derived_alias_multi_host_no_common_suffix() {
    let eps = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "us.foo.com".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "eu.bar.com".into(),
            port: 443,
        },
    ];
    // Only 1 common label ("com") — minimum is 2.
    assert_eq!(compute_derived_alias(&eps), None);
}

#[test]
fn compute_derived_alias_multi_host_identical_treated_as_single() {
    let eps = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "api.vendor.com".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "api.vendor.com".into(),
            port: 443,
        },
    ];
    assert_eq!(compute_derived_alias(&eps), Some("api.vendor.com".into()));
}

#[test]
fn compute_derived_alias_ip_returns_none() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.1".into(),
        port: 443,
    }];
    assert_eq!(compute_derived_alias(&eps), None);
}

#[test]
fn compute_derived_alias_normalizes_case() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "Api.OpenAI.COM".into(),
        port: 443,
    }];
    assert_eq!(compute_derived_alias(&eps), Some("api.openai.com".into()));
}

#[test]
fn compute_derived_alias_strips_trailing_dot() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.example.com.".into(),
        port: 443,
    }];
    assert_eq!(compute_derived_alias(&eps), Some("api.example.com".into()));
}

#[test]
fn normalize_alias_lowercases() {
    assert_eq!(normalize_alias("My-Service"), "my-service");
}

#[test]
fn normalize_alias_strips_trailing_dot() {
    assert_eq!(normalize_alias("api.example.com."), "api.example.com");
}

#[test]
fn normalize_alias_strips_multiple_trailing_dots() {
    assert_eq!(normalize_alias("svc.."), "svc");
    assert_eq!(normalize_alias("svc..."), "svc");
}

#[test]
fn normalized_host_strips_multiple_trailing_dots() {
    let ep = Endpoint {
        scheme: Scheme::Https,
        host: "Api.Example.COM..".into(),
        port: 443,
    };
    assert_eq!(ep.normalized_host(), "api.example.com");
}

#[test]
fn enforce_alias_create_hostname_rejects_user_alias() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    }];
    let err = enforce_alias_create_with("custom-alias", &eps).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn enforce_alias_create_hostname_tolerates_exact_match() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    }];
    let alias = enforce_alias_create_with("api.openai.com", &eps).unwrap();
    assert_eq!(alias, "api.openai.com");
}

#[test]
fn enforce_alias_create_hostname_auto_derives() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    }];
    let alias = enforce_alias_create_derived(&eps).unwrap();
    assert_eq!(alias, "api.openai.com");
}

#[test]
fn enforce_alias_create_ip_requires_explicit() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.1".into(),
        port: 443,
    }];
    let err = enforce_alias_create_derived(&eps).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn enforce_alias_create_ip_accepts_explicit() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.1".into(),
        port: 443,
    }];
    let alias = enforce_alias_create_with("my-backend", &eps).unwrap();
    assert_eq!(alias, "my-backend");
}

#[test]
fn enforce_alias_update_hostname_to_hostname_different_alias_rejected() {
    let old_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "old.vendor.com".into(),
        port: 443,
    }];
    let new_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "new.vendor.com".into(),
        port: 443,
    }];
    let err = enforce_alias_update_derived(&new_eps, "old.vendor.com", &old_eps).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn enforce_alias_update_hostname_to_hostname_same_alias_allowed() {
    // Swapping hosts within the same domain suffix keeps the derived alias.
    let old_eps = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "us.vendor.com".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "eu.vendor.com".into(),
            port: 443,
        },
    ];
    let new_eps = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "ap.vendor.com".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "eu.vendor.com".into(),
            port: 443,
        },
    ];
    let alias = enforce_alias_update_derived(&new_eps, "vendor.com", &old_eps).unwrap();
    assert_eq!(alias, "vendor.com");
}

#[test]
fn enforce_alias_update_hostname_multi_endpoint_changes_alias_rejected() {
    // Reviewer scenario: ["my.company.com"] → ["my.company.com", "your.company.com"]
    // would change alias from "my.company.com" to "company.com" — must be rejected.
    let old_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "my.company.com".into(),
        port: 443,
    }];
    let new_eps = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "my.company.com".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "your.company.com".into(),
            port: 443,
        },
    ];
    let err = enforce_alias_update_derived(&new_eps, "my.company.com", &old_eps).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn enforce_alias_update_hostname_to_ip_requires_alias() {
    let old_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    }];
    let new_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.1".into(),
        port: 443,
    }];
    let err = enforce_alias_update_derived(&new_eps, "api.openai.com", &old_eps).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn enforce_alias_update_hostname_to_ip_with_alias_also_rejected() {
    // hostname→IP is always rejected, even with an explicit alias.
    let old_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    }];
    let new_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.1".into(),
        port: 443,
    }];
    let err =
        enforce_alias_update_with("my-backend", &new_eps, "api.openai.com", &old_eps).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn enforce_alias_update_ip_to_ip_retains_existing() {
    let old_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.1".into(),
        port: 443,
    }];
    let new_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.2".into(),
        port: 443,
    }];
    let alias = enforce_alias_update_derived(&new_eps, "my-backend", &old_eps).unwrap();
    assert_eq!(alias, "my-backend");
}

#[test]
fn enforce_alias_update_ip_to_ip_rejects_alias_change() {
    // IP→IP with user-provided alias that differs from existing — rejected.
    let old_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.1".into(),
        port: 443,
    }];
    let new_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.2".into(),
        port: 443,
    }];
    let err = enforce_alias_update_with("new-name", &new_eps, "my-backend", &old_eps).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn enforce_alias_update_ip_to_ip_tolerates_same_alias() {
    // IP→IP with user-provided alias that matches existing — allowed (no-op).
    let old_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.1".into(),
        port: 443,
    }];
    let new_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.2".into(),
        port: 443,
    }];
    let alias = enforce_alias_update_with("my-backend", &new_eps, "my-backend", &old_eps).unwrap();
    assert_eq!(alias, "my-backend");
}

#[test]
fn enforce_alias_update_ip_to_hostname_rejected_when_alias_changes() {
    // IP→hostname changes the alias from "my-backend" to "api.openai.com" — rejected.
    let old_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.1".into(),
        port: 443,
    }];
    let new_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    }];
    let err = enforce_alias_update_derived(&new_eps, "my-backend", &old_eps).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn enforce_alias_update_ip_to_hostname_allowed_when_alias_matches() {
    // IP→hostname where the derived alias happens to match the existing one — allowed.
    let old_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.1".into(),
        port: 443,
    }];
    let new_eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    }];
    let alias = enforce_alias_update_derived(&new_eps, "api.openai.com", &old_eps).unwrap();
    assert_eq!(alias, "api.openai.com");
}

#[tokio::test]
async fn create_hostname_rejects_explicit_alias() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let mut req = make_create_upstream_hostname();
    req.alias = Some("custom".into());
    let err = svc.create_upstream(&ctx, req).await.unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn create_ip_requires_explicit_alias() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let req = CreateUpstreamRequest {
        id: None,
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: "10.0.0.1".into(),
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
    let err = svc.create_upstream(&ctx, req).await.unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn update_hostname_rejects_alias_override() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    assert_eq!(u.alias, "api.openai.com");

    // Try to override alias on hostname-based upstream — should fail.
    let mut update_req = make_update_from_upstream(&u);
    update_req.alias = Some("custom".into());
    let err = svc
        .update_upstream(&ctx, u.id, update_req)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn update_endpoints_different_alias_rejected() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    assert_eq!(u.alias, "api.openai.com");

    // Update endpoints to a different host — would change alias, must be rejected.
    let mut update_req = make_update_from_upstream(&u);
    update_req.server = Server {
        endpoints: vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.anthropic.com".into(),
            port: 443,
        }],
    };
    update_req.alias = None;
    let err = svc
        .update_upstream(&ctx, u.id, update_req)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn update_hostname_to_ip_without_alias_fails() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();

    // Switch to IP endpoints without providing alias.
    let mut update_req = make_update_from_upstream(&u);
    update_req.server = Server {
        endpoints: vec![Endpoint {
            scheme: Scheme::Https,
            host: "10.0.0.1".into(),
            port: 443,
        }],
    };
    update_req.alias = None; // no explicit alias provided
    let err = svc
        .update_upstream(&ctx, u.id, update_req)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn update_hostname_to_ip_with_alias_also_rejected() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();

    // Switch to IP endpoints with explicit alias — still rejected.
    let mut update_req = make_update_from_upstream(&u);
    update_req.server = Server {
        endpoints: vec![Endpoint {
            scheme: Scheme::Https,
            host: "10.0.0.1".into(),
            port: 443,
        }],
    };
    update_req.alias = Some("my-backend".into());
    let err = svc
        .update_upstream(&ctx, u.id, update_req)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn resolve_alias_case_insensitive() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    assert_eq!(u.alias, "api.openai.com");

    // Resolve with different casing — should still find the upstream.
    let chain = svc.build_tenant_chain(&ctx).await.unwrap();
    let (resolved, _) = svc
        .resolve_alias(&ctx, &chain, "Api.OpenAI.COM", None, Tags::Load)
        .await
        .unwrap();
    assert_eq!(resolved.id, u.id);
}

#[tokio::test]
async fn multi_endpoint_common_suffix_alias() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let req = CreateUpstreamRequest {
        id: None,
        server: Server {
            endpoints: vec![
                Endpoint {
                    scheme: Scheme::Https,
                    host: "us.vendor.com".into(),
                    port: 443,
                },
                Endpoint {
                    scheme: Scheme::Https,
                    host: "eu.vendor.com".into(),
                    port: 443,
                },
            ],
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
    let u = svc.create_upstream(&ctx, req).await.unwrap();
    assert_eq!(u.alias, "vendor.com");
}

#[tokio::test]
async fn multi_endpoint_same_suffix_different_ports_get_distinct_aliases() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    // Pool A: non-standard port 8443.
    let req_a = CreateUpstreamRequest {
        id: None,
        server: Server {
            endpoints: vec![
                Endpoint {
                    scheme: Scheme::Https,
                    host: "us.vendor.com".into(),
                    port: 8443,
                },
                Endpoint {
                    scheme: Scheme::Https,
                    host: "eu.vendor.com".into(),
                    port: 8443,
                },
            ],
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
    let u_a = svc.create_upstream(&ctx, req_a).await.unwrap();
    assert_eq!(u_a.alias, "vendor.com:8443");

    // Pool B: non-standard port 9443 — same hosts, different port.
    let req_b = CreateUpstreamRequest {
        id: None,
        server: Server {
            endpoints: vec![
                Endpoint {
                    scheme: Scheme::Https,
                    host: "us.vendor.com".into(),
                    port: 9443,
                },
                Endpoint {
                    scheme: Scheme::Https,
                    host: "eu.vendor.com".into(),
                    port: 9443,
                },
            ],
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
    let u_b = svc.create_upstream(&ctx, req_b).await.unwrap();
    assert_eq!(u_b.alias, "vendor.com:9443");

    // Both stored separately — no 409 conflict.
    assert_ne!(u_a.id, u_b.id);
}

#[tokio::test]
async fn multi_endpoint_public_suffix_requires_explicit_alias() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    // Endpoints share only a public suffix ("co.uk"), not a registrable domain.
    // The system must NOT derive an alias from a bare public suffix.
    let req = CreateUpstreamRequest {
        id: None,
        server: Server {
            endpoints: vec![
                Endpoint {
                    scheme: Scheme::Https,
                    host: "foo.co.uk".into(),
                    port: 443,
                },
                Endpoint {
                    scheme: Scheme::Https,
                    host: "bar.co.uk".into(),
                    port: 443,
                },
            ],
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

    // Should fail because alias cannot be derived and none was provided.
    let err = svc.create_upstream(&ctx, req).await.unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation error, got: {err:?}",
    );
}

#[tokio::test]
async fn multi_endpoint_public_suffix_with_explicit_alias_succeeds() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    // Same public-suffix-only endpoints, but an explicit alias is provided.
    let req = CreateUpstreamRequest {
        id: None,
        server: Server {
            endpoints: vec![
                Endpoint {
                    scheme: Scheme::Https,
                    host: "foo.co.uk".into(),
                    port: 443,
                },
                Endpoint {
                    scheme: Scheme::Https,
                    host: "bar.co.uk".into(),
                    port: 443,
                },
            ],
        },
        protocol: HTTP_PROTOCOL_ID.into(),
        alias: Some("my-uk-backends".into()),
        auth: None,
        headers: None,
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
        enabled: true,
    };

    let u = svc.create_upstream(&ctx, req).await.unwrap();
    assert_eq!(u.alias, "my-uk-backends");
}

// -- Route overlap determinism tests --

#[tokio::test]
async fn create_route_overlap_same_path_priority_method_returns_conflict() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    // First route succeeds.
    svc.create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap();

    // Second route with identical (path, priority, method) → 409 Conflict.
    let err = svc
        .create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict, got: {err:?}"
    );
}

#[tokio::test]
async fn create_route_different_method_no_conflict() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    // POST route.
    svc.create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap();

    // GET route on same path and priority — no overlap.
    let get_route_req = CreateRouteRequest {
        id: None,
        upstream_id: u.id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Get],
                path: "/v1/chat/completions".into(),
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
    };
    svc.create_route(&ctx, get_route_req).await.unwrap();
}

#[tokio::test]
async fn create_route_different_priority_no_conflict() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    svc.create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap();

    // Same path and method, different priority — no overlap.
    let mut req = make_create_route(u.id);
    req.priority = 10;
    svc.create_route(&ctx, req).await.unwrap();
}

#[tokio::test]
async fn create_route_different_path_no_conflict() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    svc.create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap();

    // Different path — no overlap.
    let mut req = make_create_route(u.id);
    req.match_rules.http.as_mut().unwrap().path = "/v1/models".into();
    svc.create_route(&ctx, req).await.unwrap();
}

#[tokio::test]
async fn create_route_disabled_no_conflict() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    svc.create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap();

    // Disabled duplicate — no conflict (disabled routes don't cause ambiguity).
    let mut req = make_create_route(u.id);
    req.enabled = false;
    svc.create_route(&ctx, req).await.unwrap();
}

#[tokio::test]
async fn create_route_against_disabled_existing_no_conflict() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    // First route is disabled.
    let mut req = make_create_route(u.id);
    req.enabled = false;
    svc.create_route(&ctx, req).await.unwrap();

    // Second route enabled with same tuple — allowed because existing is disabled.
    svc.create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap();
}

#[tokio::test]
async fn create_route_different_upstream_no_conflict() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u1 = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();
    let u2 = svc
        .create_upstream(&ctx, make_create_upstream_ip("anthropic"))
        .await
        .unwrap();

    svc.create_route(&ctx, make_create_route(u1.id))
        .await
        .unwrap();

    // Same (path, priority, method) but different upstream — no overlap.
    svc.create_route(&ctx, make_create_route(u2.id))
        .await
        .unwrap();
}

#[tokio::test]
async fn create_route_partial_method_overlap_returns_conflict() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    // Route with [Post, Put].
    let req1 = CreateRouteRequest {
        id: None,
        upstream_id: u.id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Post, HttpMethod::Put],
                path: "/v1/chat".into(),
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
    };
    svc.create_route(&ctx, req1).await.unwrap();

    // Route with [Put, Delete] — overlaps on Put.
    let req2 = CreateRouteRequest {
        id: None,
        upstream_id: u.id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Put, HttpMethod::Delete],
                path: "/v1/chat".into(),
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
    };
    let err = svc.create_route(&ctx, req2).await.unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict, got: {err:?}"
    );
}

#[tokio::test]
async fn update_upstream_rate_limit_change_reflected_in_result() {
    use crate::domain::model::SharingMode;

    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    // Create without rate_limit.
    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();
    assert!(u.rate_limit.is_none());

    // Update: add a rate_limit.
    let mut update_req = make_update_from_upstream(&u);
    let rl1 = make_rate_limit(SharingMode::Private, 100, Window::Second);
    update_req.rate_limit = Some(rl1.clone());
    let updated = svc.update_upstream(&ctx, u.id, update_req).await.unwrap();
    assert_eq!(updated.rate_limit.as_ref(), Some(&rl1));

    // Update: change to a different rate.
    let mut update_req = make_update_from_upstream(&updated);
    let rl2 = make_rate_limit(SharingMode::Private, 500, Window::Minute);
    update_req.rate_limit = Some(rl2.clone());
    let updated = svc.update_upstream(&ctx, u.id, update_req).await.unwrap();
    assert_eq!(updated.rate_limit.as_ref(), Some(&rl2));

    // Update: remove rate_limit.
    let mut update_req = make_update_from_upstream(&updated);
    update_req.rate_limit = None;
    let updated = svc.update_upstream(&ctx, u.id, update_req).await.unwrap();
    assert!(updated.rate_limit.is_none());
}

#[tokio::test]
async fn update_route_rate_limit_change_reflected_in_result() {
    use crate::domain::model::SharingMode;

    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();
    let r = svc
        .create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap();
    assert!(r.rate_limit.is_none());

    // Update: add a rate_limit.
    let mut update_req = make_update_from_route(&r);
    let rl1 = make_rate_limit(SharingMode::Private, 100, Window::Second);
    update_req.rate_limit = Some(rl1.clone());
    let updated = svc.update_route(&ctx, r.id, update_req).await.unwrap();
    assert_eq!(updated.rate_limit.as_ref(), Some(&rl1));

    // Update: change to a different rate.
    let mut update_req = make_update_from_route(&updated);
    let rl2 = make_rate_limit(SharingMode::Private, 500, Window::Minute);
    update_req.rate_limit = Some(rl2.clone());
    let updated = svc.update_route(&ctx, r.id, update_req).await.unwrap();
    assert_eq!(updated.rate_limit.as_ref(), Some(&rl2));

    // Update: remove rate_limit.
    let mut update_req = make_update_from_route(&updated);
    update_req.rate_limit = None;
    let updated = svc.update_route(&ctx, r.id, update_req).await.unwrap();
    assert!(updated.rate_limit.is_none());
}

#[tokio::test]
async fn update_route_introducing_overlap_returns_conflict() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    // Route A: POST /v1/chat, priority 0.
    let _route_a = svc
        .create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap();

    // Route B: POST /v1/models, priority 0.
    let mut req_b = make_create_route(u.id);
    req_b.match_rules.http.as_mut().unwrap().path = "/v1/models".into();
    let route_b = svc.create_route(&ctx, req_b).await.unwrap();

    // Update route B's path to match route A → conflict.
    let mut update_req = make_update_from_route(&route_b);
    update_req.match_rules = MatchRules {
        http: Some(HttpMatch {
            methods: vec![HttpMethod::Post],
            path: "/v1/chat/completions".into(),
            query_allowlist: vec![],
            path_suffix_mode: PathSuffixMode::Append,
        }),
        grpc: None,
    };
    let err = svc
        .update_route(&ctx, route_b.id, update_req)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict, got: {err:?}"
    );
}

#[tokio::test]
async fn update_route_no_self_conflict() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    let route = svc
        .create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap();

    // Update tags only — same (path, priority, method) but it's the same route.
    let mut update_req = make_update_from_route(&route);
    update_req.tags = vec!["new-tag".into()];
    let updated = svc.update_route(&ctx, route.id, update_req).await.unwrap();
    assert_eq!(updated.tags, vec!["new-tag".to_string()]);
}

#[tokio::test]
async fn update_route_enabling_into_overlap_returns_conflict() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    // Route A: enabled.
    svc.create_route(&ctx, make_create_route(u.id))
        .await
        .unwrap();

    // Route B: disabled duplicate.
    let mut req_b = make_create_route(u.id);
    req_b.enabled = false;
    let route_b = svc.create_route(&ctx, req_b).await.unwrap();

    // Enable route B → conflict with route A.
    let mut update_req = make_update_from_route(&route_b);
    update_req.enabled = true;
    let err = svc
        .update_route(&ctx, route_b.id, update_req)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Conflict { .. }),
        "expected Conflict, got: {err:?}"
    );
}

// -- validate_budget_config tests --

#[test]
fn budget_config_allocated_requires_total() {
    use crate::domain::model::{BudgetConfig, BudgetMode};
    let budget = BudgetConfig {
        mode: BudgetMode::Allocated,
        total: None,
        overcommit_ratio: None,
    };
    let err = validate_budget_config(&budget).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn budget_config_shared_requires_total() {
    use crate::domain::model::{BudgetConfig, BudgetMode};
    let budget = BudgetConfig {
        mode: BudgetMode::Shared,
        total: None,
        overcommit_ratio: None,
    };
    let err = validate_budget_config(&budget).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn budget_config_unlimited_skips_total_check() {
    use crate::domain::model::{BudgetConfig, BudgetMode};
    let budget = BudgetConfig {
        mode: BudgetMode::Unlimited,
        total: None,
        overcommit_ratio: None,
    };
    assert!(validate_budget_config(&budget).is_ok());
}

#[test]
fn budget_config_overcommit_ratio_must_be_in_range() {
    use crate::domain::model::{BudgetConfig, BudgetMode};
    // Below range
    let budget = BudgetConfig {
        mode: BudgetMode::Unlimited,
        total: None,
        overcommit_ratio: Some(0.5),
    };
    assert!(validate_budget_config(&budget).is_err());

    // Above range
    let budget = BudgetConfig {
        mode: BudgetMode::Unlimited,
        total: None,
        overcommit_ratio: Some(3.0),
    };
    assert!(validate_budget_config(&budget).is_err());

    // Within range
    let budget = BudgetConfig {
        mode: BudgetMode::Allocated,
        total: Some(100),
        overcommit_ratio: Some(1.5),
    };
    assert!(validate_budget_config(&budget).is_ok());

    // Boundaries
    assert!(
        validate_budget_config(&BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(100),
            overcommit_ratio: Some(1.0),
        })
        .is_ok()
    );
    assert!(
        validate_budget_config(&BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(100),
            overcommit_ratio: Some(2.0),
        })
        .is_ok()
    );
}

// -- Budget allocation validation (ADR example) --

#[tokio::test]
async fn budget_allocated_rejects_over_allocation() {
    // ADR example: Parent 5000/min, A=2000, B=1000, C requests 3000.
    // Sum=6000 > 5000 → REJECT
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let root = Uuid::new_v4();
    let child_a = Uuid::new_v4();
    let child_b = Uuid::new_v4();
    let child_c = Uuid::new_v4();

    let resolver = MockTenantResolverClient::with_siblings(
        TenantId(root),
        vec![TenantId(child_a), TenantId(child_b), TenantId(child_c)],
    );
    let svc = make_service_with_resolver(resolver);

    // Root creates upstream with allocated budget.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.rate_limit = Some(RateLimitConfig {
        sharing: SharingMode::Inherit,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 5000,
            window: Window::Minute,
        },
        burst: None,
        budget: Some(BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(5000),
            overcommit_ratio: Some(1.0),
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        shared_pool: None,
        limit_owner_id: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child A: 2000/min
    let ctx_a = test_ctx(child_a);
    let mut req_a = make_create_upstream_hostname();
    req_a.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 2000, Window::Minute));
    svc.create_upstream(&ctx_a, req_a).await.unwrap();

    // Child B: 1000/min
    let ctx_b = test_ctx(child_b);
    let mut req_b = make_create_upstream_hostname();
    req_b.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 1000, Window::Minute));
    svc.create_upstream(&ctx_b, req_b).await.unwrap();

    // Child C: 3000/min — sum = 6000 > 5000 → REJECT
    let ctx_c = test_ctx(child_c);
    let mut req_c = make_create_upstream_hostname();
    req_c.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 3000, Window::Minute));
    let err = svc.create_upstream(&ctx_c, req_c).await.unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation error for budget exceeded, got: {err:?}"
    );
}

#[tokio::test]
async fn budget_allocated_allows_within_overcommit_ratio() {
    // ADR example: ratio=1.5, sum=6000 ≤ 5000*1.5=7500 → ALLOW
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let root = Uuid::new_v4();
    let child_a = Uuid::new_v4();
    let child_b = Uuid::new_v4();
    let child_c = Uuid::new_v4();

    let resolver = MockTenantResolverClient::with_siblings(
        TenantId(root),
        vec![TenantId(child_a), TenantId(child_b), TenantId(child_c)],
    );
    let svc = make_service_with_resolver(resolver);

    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.rate_limit = Some(RateLimitConfig {
        sharing: SharingMode::Inherit,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 5000,
            window: Window::Minute,
        },
        burst: None,
        budget: Some(BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(5000),
            overcommit_ratio: Some(1.5),
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        shared_pool: None,
        limit_owner_id: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Children: 2000 + 1000 + 3000 = 6000 ≤ 7500 → all allowed.
    let ctx_a = test_ctx(child_a);
    let mut req_a = make_create_upstream_hostname();
    req_a.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 2000, Window::Minute));
    svc.create_upstream(&ctx_a, req_a).await.unwrap();

    let ctx_b = test_ctx(child_b);
    let mut req_b = make_create_upstream_hostname();
    req_b.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 1000, Window::Minute));
    svc.create_upstream(&ctx_b, req_b).await.unwrap();

    let ctx_c = test_ctx(child_c);
    let mut req_c = make_create_upstream_hostname();
    req_c.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 3000, Window::Minute));
    svc.create_upstream(&ctx_c, req_c).await.unwrap(); // Should succeed.
}

#[tokio::test]
async fn budget_unlimited_skips_allocation_validation() {
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let root = Uuid::new_v4();
    let child = Uuid::new_v4();

    let resolver = MockTenantResolverClient::with_siblings(TenantId(root), vec![TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.rate_limit = Some(RateLimitConfig {
        sharing: SharingMode::Inherit,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 100,
            window: Window::Minute,
        },
        burst: None,
        budget: Some(BudgetConfig {
            mode: BudgetMode::Unlimited,
            total: None,
            overcommit_ratio: None,
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        shared_pool: None,
        limit_owner_id: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child can request any rate — no budget enforcement.
    let ctx_c = test_ctx(child);
    let mut req_c = make_create_upstream_hostname();
    req_c.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 99999, Window::Minute));
    svc.create_upstream(&ctx_c, req_c).await.unwrap(); // No budget rejection.
}

#[tokio::test]
async fn budget_shared_skips_allocation_validation() {
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let root = Uuid::new_v4();
    let child = Uuid::new_v4();

    let resolver = MockTenantResolverClient::with_siblings(TenantId(root), vec![TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.rate_limit = Some(RateLimitConfig {
        sharing: SharingMode::Inherit,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 100,
            window: Window::Minute,
        },
        burst: None,
        budget: Some(BudgetConfig {
            mode: BudgetMode::Shared,
            total: Some(100),
            overcommit_ratio: None,
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        shared_pool: None,
        limit_owner_id: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child can create — shared mode doesn't validate allocations.
    let ctx_c = test_ctx(child);
    let mut req_c = make_create_upstream_hostname();
    req_c.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 99999, Window::Minute));
    svc.create_upstream(&ctx_c, req_c).await.unwrap();
}

// -- Shared-pool merge --

#[test]
fn merge_rate_limit_shared_sets_shared_pool() {
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let mut root = make_upstream(
        root_id,
        "openai",
        None,
        Some(RateLimitConfig {
            sharing: SharingMode::Enforce,
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate {
                rate: 1000,
                window: Window::Minute,
            },
            burst: None,
            budget: Some(BudgetConfig {
                mode: BudgetMode::Shared,
                total: Some(1000),
                overcommit_ratio: None,
            }),
            scope: RateLimitScope::Tenant,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
            response_headers: true,
            shared_pool: None,
            limit_owner_id: None,
        }),
        None,
        vec![],
    );

    let child = make_upstream(
        child_id,
        "openai",
        None,
        Some(make_rate_limit(SharingMode::Inherit, 500, Window::Minute)),
        None,
        vec![],
    );

    let root_upstream_id = root.id;
    merge_rate_limit(&mut root, &child);

    let effective_rl = root.rate_limit.as_ref().unwrap();
    // The child's stricter 500/min wins, but the pool keeps the owner's
    // total and window.
    assert_eq!(effective_rl.sustained.rate, 500);
    assert_eq!(
        effective_rl.shared_pool,
        Some(crate::domain::model::SharedPool {
            owner_id: root_upstream_id,
            total: 1000,
            window: Window::Minute,
            algorithm: RateLimitAlgorithm::TokenBucket,
        }),
        "the pool should be the root upstream's shared budget"
    );
}

#[test]
fn merge_rate_limit_shared_pool_owner_pinned_to_defining_ancestor() {
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let tenant = Uuid::new_v4();

    // Root defines a Shared budget — it is the defining ancestor.
    let mut effective = make_upstream(
        tenant,
        "openai",
        None,
        Some(RateLimitConfig {
            sharing: SharingMode::Enforce,
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate {
                rate: 1000,
                window: Window::Minute,
            },
            burst: None,
            budget: Some(BudgetConfig {
                mode: BudgetMode::Shared,
                total: Some(1000),
                overcommit_ratio: None,
            }),
            scope: RateLimitScope::Tenant,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
            response_headers: true,
            shared_pool: None,
            limit_owner_id: None,
        }),
        None,
        vec![],
    );
    // Capture the auto-generated upstream id — this is the defining ancestor.
    let root_upstream_id = effective.id;

    let parent = make_upstream(
        tenant,
        "openai",
        None,
        Some(make_rate_limit(SharingMode::Inherit, 800, Window::Minute)),
        None,
        vec![],
    );

    let child = make_upstream(
        tenant,
        "openai",
        None,
        Some(make_rate_limit(SharingMode::Inherit, 500, Window::Minute)),
        None,
        vec![],
    );

    // Simulate compute_effective_config: merge parent, update id, merge child.
    merge_rate_limit(&mut effective, &parent);
    effective.id = parent.id; // mirrors line 1453 in the loop

    merge_rate_limit(&mut effective, &child);
    effective.id = child.id;

    let effective_rl = effective.rate_limit.as_ref().unwrap();
    assert_eq!(
        effective_rl.shared_pool.map(|p| p.owner_id),
        Some(root_upstream_id),
        "shared pool owner must stay pinned to the root that defined BudgetMode::Shared, \
         not be overwritten to a middle layer"
    );
}

#[test]
fn merge_rate_limit_private_non_enforced_does_not_inherit_shared_pool() {
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let tenant = Uuid::new_v4();

    // Root has Shared budget with Inherit sharing (NOT Enforce).
    let mut root = make_upstream(
        tenant,
        "openai",
        None,
        Some(RateLimitConfig {
            sharing: SharingMode::Inherit,
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate {
                rate: 1000,
                window: Window::Minute,
            },
            burst: None,
            budget: Some(BudgetConfig {
                mode: BudgetMode::Shared,
                total: Some(1000),
                overcommit_ratio: None,
            }),
            scope: RateLimitScope::Tenant,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
            response_headers: true,
            shared_pool: None,
            limit_owner_id: None,
        }),
        None,
        vec![],
    );

    // Child declares Private — should get its own independent bucket.
    let child = make_upstream(
        tenant,
        "openai",
        None,
        Some(make_rate_limit(SharingMode::Private, 500, Window::Minute)),
        None,
        vec![],
    );

    merge_rate_limit(&mut root, &child);

    let effective_rl = root.rate_limit.as_ref().unwrap();
    assert_eq!(
        effective_rl.shared_pool.map(|p| p.owner_id),
        None,
        "shared pool owner must be None for Private (non-enforced) descendant — \
         it should use its own bucket, not the ancestor's shared pool"
    );
    assert_eq!(
        effective_rl.sustained.rate, 500,
        "Private descendant's rate should replace, not merge"
    );
}

#[test]
fn compute_effective_single_element_shared_sets_shared_pool() {
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let tenant = Uuid::new_v4();

    let root = make_upstream(
        tenant,
        "openai",
        None,
        Some(RateLimitConfig {
            sharing: SharingMode::Inherit,
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate {
                rate: 1000,
                window: Window::Minute,
            },
            burst: None,
            budget: Some(BudgetConfig {
                mode: BudgetMode::Shared,
                total: Some(1000),
                overcommit_ratio: None,
            }),
            scope: RateLimitScope::Tenant,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
            response_headers: true,
            shared_pool: None,
            limit_owner_id: None,
        }),
        None,
        vec![],
    );

    let effective = compute_effective_config(std::slice::from_ref(&root), None).unwrap();
    let rl = effective.rate_limit.as_ref().unwrap();
    assert_eq!(
        rl.shared_pool.map(|p| p.owner_id),
        Some(root.id),
        "single-element chain with Shared budget should have shared pool owner = own ID"
    );
}

#[tokio::test]
async fn budget_allocated_ignores_cross_tree_upstreams() {
    // Two independent trees share alias "openai". Tree A has budget=5000.
    // Tree B's child has rate=4000. Tree A's child requests 4000.
    // Without tree filtering: sum=8000 > 5000 → REJECT (wrong).
    // With tree filtering: sum=4000 ≤ 5000 → ALLOW (correct).
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let root_a = Uuid::new_v4();
    let child_a = Uuid::new_v4();
    let root_b = Uuid::new_v4();
    let child_b = Uuid::new_v4();

    let resolver = MockTenantResolverClient::with_two_trees(
        TenantId(root_a),
        vec![TenantId(child_a)],
        TenantId(root_b),
        vec![TenantId(child_b)],
    );
    let svc = make_service_with_resolver(resolver);

    // Tree A root: allocated budget of 5000/min.
    let root_a_ctx = test_ctx(root_a);
    let mut root_a_req = make_create_upstream_hostname();
    root_a_req.rate_limit = Some(RateLimitConfig {
        sharing: SharingMode::Inherit,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 5000,
            window: Window::Minute,
        },
        burst: None,
        budget: Some(BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(5000),
            overcommit_ratio: Some(1.0),
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        shared_pool: None,
        limit_owner_id: None,
    });
    svc.create_upstream(&root_a_ctx, root_a_req).await.unwrap();

    // Tree B root: same alias, different tree, no budget.
    let root_b_ctx = test_ctx(root_b);
    let mut root_b_req = make_create_upstream_hostname();
    root_b_req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 5000, Window::Minute));
    svc.create_upstream(&root_b_ctx, root_b_req).await.unwrap();

    // Tree B child: 4000/min — in a different tree, should not count.
    let child_b_ctx = test_ctx(child_b);
    let mut child_b_req = make_create_upstream_hostname();
    child_b_req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 4000, Window::Minute));
    svc.create_upstream(&child_b_ctx, child_b_req)
        .await
        .unwrap();

    // Tree A child: 4000/min — within tree A's budget of 5000.
    // Without tree filtering this would fail (4000+4000=8000 > 5000).
    let child_a_ctx = test_ctx(child_a);
    let mut child_a_req = make_create_upstream_hostname();
    child_a_req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 4000, Window::Minute));
    svc.create_upstream(&child_a_ctx, child_a_req)
        .await
        .unwrap();
}

#[tokio::test]
async fn budget_allocated_rejects_child_without_rate_limit() {
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let root = Uuid::new_v4();
    let child = Uuid::new_v4();

    let resolver = MockTenantResolverClient::with_siblings(TenantId(root), vec![TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root creates upstream with allocated budget.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.rate_limit = Some(RateLimitConfig {
        sharing: SharingMode::Inherit,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 1000,
            window: Window::Minute,
        },
        burst: None,
        budget: Some(BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(1000),
            overcommit_ratio: None,
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        shared_pool: None,
        limit_owner_id: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child with no rate_limit → should be rejected.
    let ctx_child = test_ctx(child);
    let mut req_child = make_create_upstream_hostname();
    req_child.rate_limit = None;
    let err = svc
        .create_upstream(&ctx_child, req_child)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation error for missing rate_limit, got: {err:?}"
    );
}

#[tokio::test]
async fn budget_lowering_total_rejects_when_descendants_exceed() {
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let root = Uuid::new_v4();
    let child = Uuid::new_v4();

    let resolver = MockTenantResolverClient::with_siblings(TenantId(root), vec![TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root creates upstream with allocated budget of 5000/min.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.rate_limit = Some(RateLimitConfig {
        sharing: SharingMode::Inherit,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 5000,
            window: Window::Minute,
        },
        burst: None,
        budget: Some(BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(5000),
            overcommit_ratio: Some(1.0),
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        shared_pool: None,
        limit_owner_id: None,
    });
    let root_upstream = svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child allocates 3000/min — fits within 5000.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 3000, Window::Minute));
    svc.create_upstream(&child_ctx, child_req).await.unwrap();

    // Root lowers budget to 2000/min — child's 3000 exceeds it.
    let mut update_req = make_update_from_upstream(&root_upstream);
    update_req.rate_limit = Some(RateLimitConfig {
        sharing: SharingMode::Inherit,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 2000,
            window: Window::Minute,
        },
        burst: None,
        budget: Some(BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(2000),
            overcommit_ratio: Some(1.0),
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        shared_pool: None,
        limit_owner_id: None,
    });
    let err = svc
        .update_upstream(&root_ctx, root_upstream.id, update_req)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation error when lowering budget below descendant allocations, got: {err:?}"
    );
}

fn allocated(rate: u32, window: Window, total: u32) -> RateLimitConfig {
    use crate::domain::model::{BudgetConfig, BudgetMode};
    RateLimitConfig {
        budget: Some(BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(total),
            overcommit_ratio: None,
        }),
        ..make_rate_limit(SharingMode::Inherit, rate, window)
    }
}

/// R-15: each allocated budget counts its direct allocations only. A
/// grandchild under a middle tenant that binds the alias allocates from the
/// middle tenant, not from the top.
#[tokio::test]
async fn budget_counts_direct_allocations_only() {
    let (top, middle, leaf) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let svc = make_service_with_resolver(MockTenantResolverClient::with_hierarchy(vec![
        TenantId(top),
        TenantId(middle),
        TenantId(leaf),
    ]));
    let create = |rate_limit| {
        let mut req = make_create_upstream_hostname();
        req.rate_limit = Some(rate_limit);
        req
    };

    let top_upstream = svc
        .create_upstream(&test_ctx(top), create(allocated(100, Window::Minute, 100)))
        .await
        .unwrap();
    let middle_rl = allocated(60, Window::Minute, 60);
    let middle_upstream = svc
        .create_upstream(&test_ctx(middle), create(middle_rl.clone()))
        .await
        .unwrap();
    svc.create_upstream(
        &test_ctx(leaf),
        create(make_rate_limit(SharingMode::Inherit, 50, Window::Minute)),
    )
    .await
    .unwrap();

    // Unchanged PUT of the middle tenant: 60 of the top's 100 (not 110).
    let mut update = make_update_from_upstream(&middle_upstream);
    update.rate_limit = Some(middle_rl);
    svc.update_upstream(&test_ctx(middle), middle_upstream.id, update)
        .await
        .unwrap();

    // The top may shrink to 60: the leaf's 50 is the middle tenant's.
    let mut update = make_update_from_upstream(&top_upstream);
    update.rate_limit = Some(allocated(100, Window::Minute, 60));
    svc.update_upstream(&test_ctx(top), top_upstream.id, update)
        .await
        .unwrap();

    // Control: below 60 the middle tenant's allocation no longer fits.
    let mut update = make_update_from_upstream(&top_upstream);
    update.rate_limit = Some(allocated(100, Window::Minute, 59));
    let err = svc
        .update_upstream(&test_ctx(top), top_upstream.id, update)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation, got: {err:?}"
    );
}

/// A grandchild with no tenant binding the alias in between allocates from
/// the top's budget.
#[tokio::test]
async fn budget_counts_a_grandchild_without_a_binding_in_between() {
    let (top, middle, leaf, sibling) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let mut resolver = MockTenantResolverClient::with_hierarchy(vec![
        TenantId(top),
        TenantId(middle),
        TenantId(leaf),
    ]);
    resolver.add_child(TenantId(top), TenantId(sibling));
    let svc = make_service_with_resolver(resolver);
    let create = |rate_limit| {
        let mut req = make_create_upstream_hostname();
        req.rate_limit = Some(rate_limit);
        req
    };

    svc.create_upstream(&test_ctx(top), create(allocated(100, Window::Minute, 100)))
        .await
        .unwrap();
    svc.create_upstream(
        &test_ctx(leaf),
        create(make_rate_limit(SharingMode::Inherit, 60, Window::Minute)),
    )
    .await
    .unwrap();
    let err = svc
        .create_upstream(
            &test_ctx(sibling),
            create(make_rate_limit(SharingMode::Inherit, 41, Window::Minute)),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation, got: {err:?}"
    );
    svc.create_upstream(
        &test_ctx(sibling),
        create(make_rate_limit(SharingMode::Inherit, 40, Window::Minute)),
    )
    .await
    .unwrap();
}

/// F-1: the same total over a longer window is a smaller budget, so a
/// window change is checked against the children like a total change.
#[tokio::test]
async fn budget_window_change_is_rechecked() {
    let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
    let svc = make_service_with_resolver(MockTenantResolverClient::with_siblings(
        TenantId(root),
        vec![TenantId(child)],
    ));
    let mut req = make_create_upstream_hostname();
    req.rate_limit = Some(allocated(100, Window::Minute, 100));
    let parent = svc.create_upstream(&test_ctx(root), req).await.unwrap();
    let mut req = make_create_upstream_hostname();
    req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 60, Window::Minute));
    svc.create_upstream(&test_ctx(child), req).await.unwrap();

    let mut update = make_update_from_upstream(&parent);
    update.rate_limit = Some(allocated(100, Window::Hour, 100));
    let err = svc
        .update_upstream(&test_ctx(root), parent.id, update)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::Validation { .. }),
        "expected Validation, got: {err:?}"
    );
}

#[tokio::test]
async fn budget_lowering_total_allows_when_descendants_fit() {
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let root = Uuid::new_v4();
    let child = Uuid::new_v4();

    let resolver = MockTenantResolverClient::with_siblings(TenantId(root), vec![TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    // Root creates upstream with allocated budget of 5000/min.
    let root_ctx = test_ctx(root);
    let mut root_req = make_create_upstream_hostname();
    root_req.rate_limit = Some(RateLimitConfig {
        sharing: SharingMode::Inherit,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 5000,
            window: Window::Minute,
        },
        burst: None,
        budget: Some(BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(5000),
            overcommit_ratio: Some(1.0),
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        shared_pool: None,
        limit_owner_id: None,
    });
    let root_upstream = svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child allocates 1000/min.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 1000, Window::Minute));
    svc.create_upstream(&child_ctx, child_req).await.unwrap();

    // Root lowers budget to 2000/min — child's 1000 still fits.
    let mut update_req = make_update_from_upstream(&root_upstream);
    update_req.rate_limit = Some(RateLimitConfig {
        sharing: SharingMode::Inherit,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 2000,
            window: Window::Minute,
        },
        burst: None,
        budget: Some(BudgetConfig {
            mode: BudgetMode::Allocated,
            total: Some(2000),
            overcommit_ratio: Some(1.0),
        }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        shared_pool: None,
        limit_owner_id: None,
    });
    svc.update_upstream(&root_ctx, root_upstream.id, update_req)
        .await
        .unwrap();
}

#[test]
fn merge_rate_limit_allocated_does_not_set_shared_pool() {
    use crate::domain::model::{BudgetConfig, BudgetMode};

    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let mut root = make_upstream(
        root_id,
        "openai",
        None,
        Some(RateLimitConfig {
            sharing: SharingMode::Enforce,
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate {
                rate: 1000,
                window: Window::Minute,
            },
            burst: None,
            budget: Some(BudgetConfig {
                mode: BudgetMode::Allocated,
                total: Some(1000),
                overcommit_ratio: Some(1.0),
            }),
            scope: RateLimitScope::Tenant,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
            response_headers: true,
            shared_pool: None,
            limit_owner_id: None,
        }),
        None,
        vec![],
    );

    let child = make_upstream(
        child_id,
        "openai",
        None,
        Some(make_rate_limit(SharingMode::Inherit, 500, Window::Minute)),
        None,
        vec![],
    );

    merge_rate_limit(&mut root, &child);

    let effective_rl = root.rate_limit.as_ref().unwrap();
    assert_eq!(
        effective_rl.shared_pool.map(|p| p.owner_id),
        None,
        "shared pool owner should NOT be set for allocated budget"
    );
}

// -- Cost against the shared pool --

/// `rate`/min at `cost`, with a shared budget of `total` when given.
fn pool_limit(sharing: SharingMode, rate: u32, cost: u32, total: Option<u32>) -> RateLimitConfig {
    use crate::domain::model::{BudgetConfig, BudgetMode};
    RateLimitConfig {
        budget: total.map(|total| BudgetConfig {
            mode: BudgetMode::Shared,
            total: Some(total),
            overcommit_ratio: None,
        }),
        cost,
        ..make_rate_limit(sharing, rate, Window::Minute)
    }
}

fn create_with(rate_limit: Option<RateLimitConfig>) -> CreateUpstreamRequest {
    CreateUpstreamRequest {
        rate_limit,
        ..make_create_upstream_hostname()
    }
}

const OWN_POOL_SHORT: &str = "rate_limit.cost (4) must not exceed rate_limit.budget.total (3)";
const ANCESTOR_POOL_SHORT: &str = "rate_limit.cost (4) must not exceed budget.total (3) of the \
     shared budget an ancestor's upstream defines for this alias";

/// A request costing more than the upstream's own shared budget would be
/// refused by the pool bucket every time.
#[tokio::test]
async fn cost_above_the_own_shared_budget_is_rejected() {
    let svc = make_service();
    let ctx = test_ctx(Uuid::new_v4());
    assert_validation_detail(
        svc.create_upstream(
            &ctx,
            create_with(Some(pool_limit(SharingMode::Inherit, 10, 4, Some(3)))),
        )
        .await,
        OWN_POOL_SHORT,
        "create",
    );

    // A cost equal to the total fits.
    let up = svc
        .create_upstream(
            &ctx,
            create_with(Some(pool_limit(SharingMode::Inherit, 10, 3, Some(3)))),
        )
        .await
        .unwrap();
    let mut update = make_update_from_upstream(&up);
    update.rate_limit = Some(pool_limit(SharingMode::Private, 10, 4, Some(3)));
    assert_validation_detail(
        svc.update_upstream(&ctx, up.id, update).await,
        OWN_POOL_SHORT,
        "update",
    );
}

/// A descendant's effective limit is charged to the pool an ancestor's
/// shared budget defines: its cost must fit that pool's total.
#[tokio::test]
async fn cost_above_an_inherited_shared_pool_is_rejected() {
    let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
    let svc = make_service_with_resolver(MockTenantResolverClient::with_hierarchy(vec![
        TenantId(root),
        TenantId(child),
    ]));
    svc.create_upstream(
        &test_ctx(root),
        create_with(Some(pool_limit(SharingMode::Inherit, 100, 1, Some(3)))),
    )
    .await
    .unwrap();
    let child_ctx = test_ctx(child);

    // The child's stricter 10/min wins the merge, at cost 4.
    assert_validation_detail(
        svc.create_upstream(
            &child_ctx,
            create_with(Some(pool_limit(SharingMode::Inherit, 10, 4, None))),
        )
        .await,
        ANCESTOR_POOL_SHORT,
        "create",
    );
    // Its own larger shared budget does not replace the ancestor's pool.
    assert_validation_detail(
        svc.create_upstream(
            &child_ctx,
            create_with(Some(pool_limit(SharingMode::Inherit, 10, 4, Some(50)))),
        )
        .await,
        ANCESTOR_POOL_SHORT,
        "create with its own budget",
    );

    let up = svc
        .create_upstream(
            &child_ctx,
            create_with(Some(pool_limit(SharingMode::Inherit, 10, 3, None))),
        )
        .await
        .unwrap();
    let mut update = make_update_from_upstream(&up);
    update.rate_limit = Some(pool_limit(SharingMode::Inherit, 10, 4, None));
    assert_validation_detail(
        svc.update_upstream(&child_ctx, up.id, update).await,
        ANCESTOR_POOL_SHORT,
        "update",
    );

    // Not charged to the pool: a limit that loses the merge (the root's
    // 100/min at cost 1 is effective), and a private one.
    for rate_limit in [
        pool_limit(SharingMode::Inherit, 1000, 4, None),
        pool_limit(SharingMode::Private, 10, 4, None),
    ] {
        let mut update = make_update_from_upstream(&up);
        update.rate_limit = Some(rate_limit.clone());
        svc.update_upstream(&child_ctx, up.id, update)
            .await
            .unwrap_or_else(|e| panic!("{rate_limit:?}: {e:?}"));
    }
}

/// The proxy skips disabled ancestors, so their stricter limit cannot be
/// what keeps the cost within the pool; and one can be re-enabled, so the
/// pool it defines must also fit.
#[tokio::test]
async fn disabled_ancestors_are_checked_both_skipped_and_enabled() {
    let three_tenants = || {
        let ids = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let svc = make_service_with_resolver(MockTenantResolverClient::with_hierarchy(vec![
            TenantId(ids.0),
            TenantId(ids.1),
            TenantId(ids.2),
        ]));
        (svc, ids)
    };
    let disabled = |rate_limit| CreateUpstreamRequest {
        enabled: false,
        ..create_with(Some(rate_limit))
    };

    // Root pool of 3; the disabled middle's 1/min at cost 1 would win the
    // merge, but at runtime the child's 10/min at cost 4 does.
    let (svc, (root, middle, child)) = three_tenants();
    svc.create_upstream(
        &test_ctx(root),
        create_with(Some(pool_limit(SharingMode::Inherit, 100, 1, Some(3)))),
    )
    .await
    .unwrap();
    svc.create_upstream(
        &test_ctx(middle),
        disabled(pool_limit(SharingMode::Inherit, 1, 1, None)),
    )
    .await
    .unwrap();
    assert_validation_detail(
        svc.create_upstream(
            &test_ctx(child),
            create_with(Some(pool_limit(SharingMode::Inherit, 10, 4, None))),
        )
        .await,
        ANCESTOR_POOL_SHORT,
        "disabled middle skipped",
    );
    svc.create_upstream(
        &test_ctx(child),
        create_with(Some(pool_limit(SharingMode::Inherit, 10, 3, None))),
    )
    .await
    .unwrap();

    // Only the disabled middle defines a pool (its stricter limit wins the
    // merge and carries it): none applies today, but its pool of 3 would
    // once it is enabled.
    let (svc, (root, middle, child)) = three_tenants();
    svc.create_upstream(
        &test_ctx(root),
        create_with(Some(pool_limit(SharingMode::Inherit, 100, 1, None))),
    )
    .await
    .unwrap();
    svc.create_upstream(
        &test_ctx(middle),
        disabled(pool_limit(SharingMode::Inherit, 50, 1, Some(3))),
    )
    .await
    .unwrap();
    assert_validation_detail(
        svc.create_upstream(
            &test_ctx(child),
            create_with(Some(pool_limit(SharingMode::Inherit, 10, 4, None))),
        )
        .await,
        ANCESTOR_POOL_SHORT,
        "disabled middle re-enabled",
    );
}

/// A shortfall the ancestors' effective limit already has (here a pool
/// lowered under a middle tenant's cost) does not block a write below them
/// that does not change what is charged.
#[tokio::test]
async fn an_inherited_pool_shortfall_does_not_block_writes_below() {
    let (root, middle, leaf) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let svc = make_service_with_resolver(MockTenantResolverClient::with_hierarchy(vec![
        TenantId(root),
        TenantId(middle),
        TenantId(leaf),
    ]));
    let root_up = svc
        .create_upstream(
            &test_ctx(root),
            create_with(Some(pool_limit(SharingMode::Inherit, 100, 1, Some(10)))),
        )
        .await
        .unwrap();
    svc.create_upstream(
        &test_ctx(middle),
        create_with(Some(pool_limit(SharingMode::Inherit, 10, 5, None))),
    )
    .await
    .unwrap();
    // Lowering a pool is not checked against the descendants' costs.
    let mut update = make_update_from_upstream(&root_up);
    update.rate_limit = Some(pool_limit(SharingMode::Inherit, 100, 1, Some(4)));
    svc.update_upstream(&test_ctx(root), root_up.id, update)
        .await
        .unwrap();

    svc.create_upstream(&test_ctx(leaf), create_with(None))
        .await
        .unwrap();
}

/// [`effective_rate_limit`] is the rate limit [`compute_effective_config`]
/// merges, shared pool and limit owner included, whatever route matched and
/// whatever enforced route limit it shadows: those go to route buckets.
#[test]
fn effective_rate_limit_matches_the_full_merge() {
    let t = Uuid::new_v4();
    let up = |rate_limit| make_upstream(t, "openai", None, rate_limit, None, vec![]);
    let plain = |sharing, rate| Some(make_rate_limit(sharing, rate, Window::Minute));
    // Each chain with the index of the upstream whose limit applies.
    let chains = [
        (
            vec![up(Some(pool_limit(SharingMode::Inherit, 10, 2, Some(3))))],
            Some(0),
        ),
        (
            vec![
                up(Some(pool_limit(SharingMode::Inherit, 100, 1, Some(3)))),
                up(None),
                up(Some(pool_limit(SharingMode::Inherit, 10, 4, Some(50)))),
            ],
            Some(2),
        ),
        (
            vec![
                up(Some(pool_limit(SharingMode::Enforce, 100, 1, Some(3)))),
                up(Some(pool_limit(SharingMode::Private, 10, 4, None))),
            ],
            Some(1),
        ),
        (
            vec![
                up(Some(pool_limit(SharingMode::Inherit, 100, 1, Some(3)))),
                up(Some(pool_limit(SharingMode::Private, 10, 4, None))),
            ],
            Some(1),
        ),
        (vec![up(plain(SharingMode::Inherit, 10)), up(None)], Some(0)),
        (
            vec![
                up(plain(SharingMode::Enforce, 10)),
                up(plain(SharingMode::Private, 100)),
            ],
            Some(0),
        ),
        (vec![up(None), up(None)], None),
    ];
    for (chain, owner) in chains {
        let expected = merge::effective_rate_limit(&chain);
        assert_eq!(
            expected.as_ref().and_then(|rl| rl.limit_owner_id),
            owner.map(|i: usize| chain[i].id),
            "{chain:?}"
        );
        assert_eq!(
            expected,
            compute_effective_config(&chain, None).unwrap().rate_limit,
            "{chain:?}"
        );
        let (root, selected) = (&chain[0], &chain[chain.len() - 1]);
        let mut route = route_on(selected, None);
        route.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 1, Window::Second));
        let mut shadowed = route_on(root, None);
        shadowed.rate_limit = Some(make_rate_limit(SharingMode::Enforce, 1, Window::Second));
        let with_routes = merge::compute_effective_config_reporting(
            &chain,
            Some(&route),
            &[shadowed],
            &mut |_| {},
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(expected, with_routes.rate_limit, "{chain:?}");
    }
}

// -- validate_endpoints_ssrf tests --

fn default_guard() -> SsrfGuard {
    SsrfGuard::from_config(&Default::default()).unwrap()
}

#[test]
fn ssrf_rejects_loopback_ip() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "127.0.0.1".into(),
        port: 443,
    }];
    let err = validate_endpoints_ssrf(&default_guard(), &endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("SSRF") && detail.contains("loopback"),
                "expected SSRF loopback error, got: {detail}"
            );
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

#[test]
fn ssrf_rejects_private_10_x() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.0.1".into(),
        port: 443,
    }];
    let err = validate_endpoints_ssrf(&default_guard(), &endpoints).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn ssrf_rejects_private_172_16() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "172.16.0.1".into(),
        port: 443,
    }];
    let err = validate_endpoints_ssrf(&default_guard(), &endpoints).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn ssrf_rejects_private_192_168() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "192.168.1.1".into(),
        port: 443,
    }];
    let err = validate_endpoints_ssrf(&default_guard(), &endpoints).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn ssrf_rejects_link_local_metadata() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Http,
        host: "169.254.169.254".into(),
        port: 80,
    }];
    let err = validate_endpoints_ssrf(&default_guard(), &endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("link-local"),
                "expected link-local error, got: {detail}"
            );
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

#[test]
fn ssrf_allows_public_ip() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "8.8.8.8".into(),
        port: 443,
    }];
    assert!(validate_endpoints_ssrf(&default_guard(), &endpoints).is_ok());
}

#[test]
fn ssrf_allows_hostname_endpoints() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    }];
    assert!(validate_endpoints_ssrf(&default_guard(), &endpoints).is_ok());
}

#[test]
fn ssrf_rejects_localhost() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "localhost".into(),
        port: 443,
    }];
    let err = validate_endpoints_ssrf(&default_guard(), &endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(detail.contains("SSRF protection"), "got: {detail}");
            assert!(detail.contains("localhost"), "got: {detail}");
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

#[test]
fn ssrf_rejects_localhost_localdomain() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "localhost.localdomain".into(),
        port: 443,
    }];
    assert!(matches!(
        validate_endpoints_ssrf(&default_guard(), &endpoints),
        Err(DomainError::Validation { .. })
    ));
}

#[test]
fn ssrf_extra_deny_hostnames_blocks_suffix() {
    use crate::domain::ssrf::SsrfPolicy;
    let guard = SsrfGuard::from_config(&SsrfPolicy {
        enabled: true,
        extra_deny_hostnames: vec![".cluster.local".into()],
        ..Default::default()
    })
    .unwrap();
    let endpoints = vec![Endpoint {
        scheme: Scheme::Http,
        host: "my-svc.default.svc.cluster.local".into(),
        port: 80,
    }];
    let err = validate_endpoints_ssrf(&guard, &endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(detail.contains("cluster.local"), "got: {detail}");
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

#[test]
fn ssrf_rejects_localhost_case_insensitive() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "LocalHost".into(),
        port: 443,
    }];
    assert!(matches!(
        validate_endpoints_ssrf(&default_guard(), &endpoints),
        Err(DomainError::Validation { .. })
    ));
}

#[test]
fn ssrf_rejects_localhost_trailing_dot() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "localhost.".into(),
        port: 443,
    }];
    assert!(matches!(
        validate_endpoints_ssrf(&default_guard(), &endpoints),
        Err(DomainError::Validation { .. })
    ));
}

#[test]
fn ssrf_rejects_second_endpoint_in_list() {
    let endpoints = vec![
        Endpoint {
            scheme: Scheme::Https,
            host: "8.8.8.8".into(),
            port: 443,
        },
        Endpoint {
            scheme: Scheme::Https,
            host: "10.0.0.1".into(),
            port: 443,
        },
    ];
    let err = validate_endpoints_ssrf(&default_guard(), &endpoints).unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("endpoint[1]"),
                "expected endpoint[1] index, got: {detail}"
            );
        }
        _ => panic!("expected Validation, got: {err:?}"),
    }
}

// -- SSRF integration tests (through ControlPlaneService trait) --

fn make_service_ssrf_enabled() -> ControlPlaneServiceImpl {
    ControlPlaneServiceImpl::new(
        Arc::new(InMemoryUpstreamRepo::new()),
        Arc::new(InMemoryRouteRepo::new()),
        Arc::new(MockTenantResolverClient::single_tenant()),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::empty()),
        Arc::new(SsrfGuard::from_config(&Default::default()).unwrap()),
        None,
    )
}

fn make_create_upstream_with_ip(ip: &str, alias: &str) -> CreateUpstreamRequest {
    CreateUpstreamRequest {
        id: None,
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: ip.into(),
                port: 443,
            }],
        },
        protocol: HTTP_PROTOCOL_ID.into(),
        alias: Some(alias.into()),
        auth: None,
        headers: None,
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
        enabled: true,
    }
}

#[tokio::test]
async fn create_upstream_ssrf_enabled_blocks_private_ip() {
    let svc = make_service_ssrf_enabled();
    let ctx = test_ctx(Uuid::new_v4());
    let req = make_create_upstream_with_ip("10.0.0.1", "internal");

    let err = svc.create_upstream(&ctx, req).await.unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[tokio::test]
async fn create_upstream_ssrf_enabled_allows_public_ip() {
    let svc = make_service_ssrf_enabled();
    let ctx = test_ctx(Uuid::new_v4());
    let req = make_create_upstream_with_ip("8.8.8.8", "dns");

    let result = svc.create_upstream(&ctx, req).await;
    assert!(result.is_ok(), "public IP should be allowed: {result:?}");
}

#[tokio::test]
async fn create_upstream_ssrf_disabled_allows_private_ip() {
    let svc = make_service(); // ssrf_protection=false
    let ctx = test_ctx(Uuid::new_v4());
    let req = make_create_upstream_with_ip("10.0.0.1", "internal");

    let result = svc.create_upstream(&ctx, req).await;
    assert!(
        result.is_ok(),
        "private IP should be allowed when SSRF is disabled: {result:?}"
    );
}

#[tokio::test]
async fn update_upstream_ssrf_enabled_blocks_private_ip() {
    let svc = make_service_ssrf_enabled();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let req = make_create_upstream_with_ip("8.8.8.8", "dns");
    let upstream = svc.create_upstream(&ctx, req).await.unwrap();

    let mut update = make_update_from_upstream(&upstream);
    update.server.endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.0.1".into(),
        port: 443,
    }];
    update.alias = Some("internal".into());

    let err = svc
        .update_upstream(&ctx, upstream.id, update)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

/// An IPv6 endpoint in a denied range is refused with its SSRF reason, not
/// with the generic IPv6 refusal; other IPv6 endpoints still get that one.
#[tokio::test]
async fn create_upstream_names_the_ssrf_reason_for_a_denied_ipv6_endpoint() {
    let ctx = test_ctx(Uuid::new_v4());
    let detail = |err: DomainError| match err {
        DomainError::Validation {
            field: "",
            reason: oagw_sdk::field::VALIDATION,
            detail,
            instance,
        } if instance.is_empty() => detail,
        other => panic!("expected a plain validation error, got: {other:?}"),
    };

    let guarded = make_service_ssrf_enabled();
    for (host, reason) in [
        ("[5f00::1]", "SRv6 segment identifier (5f00::/16)"),
        ("[fec0::1]", "site-local address (fec0::/10)"),
        (
            "[64:ff9b:1::a00:1]",
            "local-use NAT64 address (64:ff9b:1::/48)",
        ),
        (
            "[4000::1]",
            "address outside global unicast space (2000::/3)",
        ),
        ("[::808:808]", "deprecated IPv4-compatible address (::/96)"),
    ] {
        let err = guarded
            .create_upstream(&ctx, make_create_upstream_with_ip(host, "denied-v6"))
            .await
            .unwrap_err();
        assert_eq!(
            detail(err),
            format!("endpoint[0] IP address '{host}' is blocked by SSRF protection: {reason}")
        );
    }

    let not_supported =
        "endpoint[0] uses IPv6 address '[5f00::1]'; IPv6 endpoints are not yet supported";
    let err = make_service()
        .create_upstream(&ctx, make_create_upstream_with_ip("[5f00::1]", "denied-v6"))
        .await
        .unwrap_err();
    assert_eq!(detail(err), not_supported, "SSRF off");
    let err = guarded
        .create_upstream(
            &ctx,
            make_create_upstream_with_ip("[2606:4700::1111]", "public-v6"),
        )
        .await
        .unwrap_err();
    assert_eq!(
        detail(err),
        "endpoint[0] uses IPv6 address '[2606:4700::1111]'; IPv6 endpoints are not yet supported"
    );
}

// -- canonical tags and HTTP methods --

fn unsorted_tags() -> Vec<String> {
    vec!["b".into(), "a".into(), "b".into()]
}

fn sorted_tags() -> Vec<String> {
    vec!["a".into(), "b".into()]
}

#[tokio::test]
async fn create_and_update_upstream_store_canonical_tags() {
    let svc = make_service();
    let ctx = test_ctx(Uuid::new_v4());

    let mut req = make_create_upstream_ip("canon");
    req.tags = unsorted_tags();
    let created = svc.create_upstream(&ctx, req).await.unwrap();
    assert_eq!(created.tags, sorted_tags());
    assert_eq!(
        svc.get_upstream(&ctx, created.id).await.unwrap().tags,
        sorted_tags()
    );

    let mut update = make_update_from_upstream(&created);
    update.tags = vec!["z".into(), "y".into(), "z".into()];
    let updated = svc.update_upstream(&ctx, created.id, update).await.unwrap();
    assert_eq!(updated.tags, vec!["y".to_owned(), "z".to_owned()]);
}

#[tokio::test]
async fn create_and_update_route_store_canonical_tags_and_methods() {
    let svc = make_service();
    let ctx = test_ctx(Uuid::new_v4());
    let upstream = svc
        .create_upstream(&ctx, make_create_upstream_ip("canon-route"))
        .await
        .unwrap();

    let mut req = make_create_route(upstream.id);
    req.tags = unsorted_tags();
    if let Some(http) = req.match_rules.http.as_mut() {
        http.methods = vec![HttpMethod::Patch, HttpMethod::Get, HttpMethod::Get];
    }
    let created = svc.create_route(&ctx, req).await.unwrap();
    assert_eq!(created.tags, sorted_tags());
    assert_eq!(
        created.match_rules.http.as_ref().unwrap().methods,
        vec![HttpMethod::Get, HttpMethod::Patch]
    );

    let mut update = make_update_from_route(&created);
    update.tags = vec!["d".into(), "c".into()];
    if let Some(http) = update.match_rules.http.as_mut() {
        http.methods = vec![HttpMethod::Delete, HttpMethod::Post, HttpMethod::Delete];
    }
    let updated = svc.update_route(&ctx, created.id, update).await.unwrap();
    assert_eq!(updated.tags, vec!["c".to_owned(), "d".to_owned()]);
    assert_eq!(
        updated.match_rules.http.as_ref().unwrap().methods,
        vec![HttpMethod::Post, HttpMethod::Delete]
    );
}

// -- closest ancestor lookup --

#[tokio::test]
async fn closest_ancestor_lookup_picks_nearest_ancestor_with_alias() {
    let (root, mid, leaf) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let resolver = MockTenantResolverClient::with_hierarchy(vec![
        TenantId(root),
        TenantId(mid),
        TenantId(leaf),
    ]);
    let svc = make_service_with_resolver(resolver);

    let root_up = svc
        .create_upstream(&test_ctx(root), make_create_upstream_hostname())
        .await
        .unwrap();
    let mid_up = svc
        .create_upstream(&test_ctx(mid), make_create_upstream_hostname())
        .await
        .unwrap();

    let chain = svc.build_tenant_chain(&test_ctx(leaf)).await.unwrap();
    let found = ancestor::ancestor_upstreams(&*svc.upstreams, &chain, &root_up.alias)
        .await
        .unwrap();
    assert_eq!(
        found.iter().map(|u| u.id).collect::<Vec<_>>(),
        vec![mid_up.id, root_up.id],
        "closest first"
    );

    // The requesting tenant itself is never its own ancestor.
    let mid_chain = svc.build_tenant_chain(&test_ctx(mid)).await.unwrap();
    let found = ancestor::ancestor_upstreams(&*svc.upstreams, &mid_chain, &root_up.alias)
        .await
        .unwrap();
    assert_eq!(
        found.iter().map(|u| u.id).collect::<Vec<_>>(),
        vec![root_up.id]
    );

    // A root tenant has no ancestors.
    let root_chain = svc.build_tenant_chain(&test_ctx(root)).await.unwrap();
    let found = ancestor::ancestor_upstreams(&*svc.upstreams, &root_chain, &root_up.alias)
        .await
        .unwrap();
    assert!(found.is_empty());
}

// ---------------------------------------------------------------------------
// Storage failures are not "not found"
// ---------------------------------------------------------------------------

fn make_service_over(
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
        None,
    )
}

#[track_caller]
fn assert_internal<T: std::fmt::Debug>(result: Result<T, DomainError>, op: &str) {
    assert!(
        matches!(result, Err(DomainError::Internal { .. })),
        "{op}: a storage failure must surface as internal, got {result:?}"
    );
}

/// An unknown upstream and an unreachable database are different answers:
/// a 404 for an outage would tell the client the upstream is gone.
#[tokio::test]
async fn upstream_storage_failure_is_internal_not_not_found() {
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);
    let existing = make_service()
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    // Delete reaches the upstream repository first.
    let svc = make_service_over(
        Arc::new(UnavailableStorage),
        Arc::new(InMemoryRouteRepo::new()),
    );

    assert_internal(svc.get_upstream(&ctx, existing.id).await, "get");
    assert_internal(
        svc.update_upstream(&ctx, existing.id, make_update_from_upstream(&existing))
            .await,
        "update",
    );
    assert_internal(svc.delete_upstream(&ctx, existing.id).await, "delete");
}

#[tokio::test]
async fn route_storage_failure_is_internal_not_not_found() {
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);
    let healthy = make_service();
    let upstream = healthy
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    let existing = healthy
        .create_route(&ctx, make_create_route(upstream.id))
        .await
        .unwrap();
    let svc = make_service_over(
        Arc::new(InMemoryUpstreamRepo::new()),
        Arc::new(UnavailableStorage),
    );

    assert_internal(svc.get_route(&ctx, existing.id).await, "get");
    assert_internal(
        svc.update_route(&ctx, existing.id, make_update_from_route(&existing))
            .await,
        "update",
    );
    assert_internal(svc.delete_route(&ctx, existing.id).await, "delete");
}

/// The upstream check in route create answers 400 only when the upstream is
/// missing; an outage is not a validation error.
#[tokio::test]
async fn route_create_upstream_storage_failure_is_internal_not_validation() {
    let ctx = test_ctx(Uuid::new_v4());
    let svc = make_service_over(
        Arc::new(UnavailableStorage),
        Arc::new(InMemoryRouteRepo::new()),
    );
    assert_internal(
        svc.create_route(&ctx, make_create_route(Uuid::new_v4()))
            .await,
        "create",
    );
}

/// A storage outage during proxy resolution is not a 404: the client would
/// read it as an unknown alias or route.
#[tokio::test]
async fn proxy_resolution_storage_failure_is_internal() {
    let ctx = test_ctx(Uuid::new_v4());
    let svc = make_service_over(
        Arc::new(UnavailableStorage),
        Arc::new(InMemoryRouteRepo::new()),
    );
    assert_internal(
        svc.resolve_proxy_target(
            &crate::domain::services::ProxyGrant::for_tests(ctx.clone()),
            "api.openai.com",
            "POST",
            "/v1/chat/completions",
            Tags::Load,
        )
        .await,
        "alias lookup",
    );

    // The upstream resolves; the route lookup fails.
    let upstreams = Arc::new(InMemoryUpstreamRepo::new());
    make_service_over(upstreams.clone(), Arc::new(InMemoryRouteRepo::new()))
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    let svc = make_service_over(upstreams, Arc::new(UnavailableStorage));
    assert_internal(
        svc.resolve_proxy_target(
            &crate::domain::services::ProxyGrant::for_tests(ctx.clone()),
            "api.openai.com",
            "POST",
            "/v1/chat/completions",
            Tags::Load,
        )
        .await,
        "route lookup",
    );
}

// ---------------------------------------------------------------------------
// Limits on stored values (sized columns and JSON; PostgreSQL rejects NUL)
// ---------------------------------------------------------------------------

#[track_caller]
fn assert_validation<T: std::fmt::Debug>(result: Result<T, DomainError>, case: &str) {
    assert!(
        matches!(result, Err(DomainError::Validation { .. })),
        "{case}: expected a validation error, got {result:?}"
    );
}

/// A validation error whose detail contains `needle`: a limit case must
/// fail on the limit, not on another check of the same value.
#[track_caller]
fn assert_validation_detail<T: std::fmt::Debug>(
    result: Result<T, DomainError>,
    needle: &str,
    case: &str,
) {
    match result {
        Err(DomainError::Validation { detail, .. }) => {
            assert!(detail.contains(needle), "{case}: {detail}");
        }
        other => panic!("{case}: expected a validation error, got {other:?}"),
    }
}

fn tags(count: usize, bytes: usize) -> Vec<String> {
    (0..count).map(|i| format!("{i:0>bytes$}")).collect()
}

fn grpc_rules(service: &str, method: &str) -> MatchRules {
    MatchRules {
        http: None,
        grpc: Some(GrpcMatch {
            service: service.into(),
            method: method.into(),
        }),
    }
}

#[tokio::test]
async fn upstream_tags_are_limited_on_create_and_update() {
    let svc = make_service();
    let ctx = test_ctx(Uuid::new_v4());
    let with_tags = |tags| CreateUpstreamRequest {
        tags,
        ..make_create_upstream_ip("limits")
    };

    let at_limit = svc
        .create_upstream(&ctx, with_tags(tags(64, 128)))
        .await
        .expect("64 tags of 128 bytes are accepted");
    for (case, bad) in [
        ("65 tags", tags(65, 1)),
        ("a 129-byte tag", tags(1, 129)),
        ("a NUL in a tag", vec!["a\0b".to_owned()]),
        ("a newline in a tag", vec!["a\nb".to_owned()]),
    ] {
        assert_validation(
            svc.create_upstream(&ctx, with_tags(bad.clone())).await,
            case,
        );
        let mut update = make_update_from_upstream(&at_limit);
        update.tags = bad;
        assert_validation(svc.update_upstream(&ctx, at_limit.id, update).await, case);
    }
}

#[tokio::test]
async fn route_tags_and_match_fields_are_limited_on_create_and_update() {
    let svc = make_service();
    let ctx = test_ctx(Uuid::new_v4());
    let upstream = svc
        .create_upstream(&ctx, make_create_upstream_ip("limits"))
        .await
        .unwrap();
    let http_rules = |path: String| MatchRules {
        http: Some(HttpMatch {
            methods: vec![HttpMethod::Get],
            path,
            query_allowlist: vec![],
            path_suffix_mode: PathSuffixMode::Append,
        }),
        grpc: None,
    };

    let route = svc
        .create_route(
            &ctx,
            CreateRouteRequest {
                tags: tags(64, 128),
                match_rules: http_rules(format!("/{}", "a".repeat(2047))),
                ..make_create_route(upstream.id)
            },
        )
        .await
        .expect("64 tags and a 2048-byte path are accepted");

    let long_name = "s".repeat(257);
    let cases: [(&str, Vec<String>, MatchRules); 8] = [
        ("65 tags", tags(65, 1), route.match_rules.clone()),
        ("a 129-byte tag", tags(1, 129), route.match_rules.clone()),
        (
            "a NUL in a tag",
            vec!["\0".into()],
            route.match_rules.clone(),
        ),
        (
            "a 2049-byte path",
            vec![],
            http_rules(format!("/{}", "a".repeat(2048))),
        ),
        ("a NUL in the path", vec![], http_rules("/v1\0".into())),
        (
            "a 257-byte gRPC service",
            vec![],
            grpc_rules(&long_name, "Get"),
        ),
        (
            "a 257-byte gRPC method",
            vec![],
            grpc_rules("pkg.Svc", &long_name),
        ),
        (
            "a control character in a gRPC name",
            vec![],
            grpc_rules("pkg.Svc", "Get\u{7}"),
        ),
    ];
    for (case, tags, match_rules) in cases {
        let create = CreateRouteRequest {
            tags: tags.clone(),
            match_rules: match_rules.clone(),
            ..make_create_route(upstream.id)
        };
        assert_validation(svc.create_route(&ctx, create).await, case);
        let mut update = make_update_from_route(&route);
        update.tags = tags;
        update.match_rules = match_rules;
        assert_validation(svc.update_route(&ctx, route.id, update).await, case);
    }
}

fn plugins(plugin_ref: &str) -> Option<crate::domain::model::PluginsConfig> {
    Some(crate::domain::model::PluginsConfig {
        sharing: crate::domain::model::SharingMode::Private,
        items: vec![crate::domain::model::PluginBinding {
            plugin_ref: plugin_ref.into(),
            config: Default::default(),
        }],
    })
}

fn auth(plugin_type: &str) -> Option<crate::domain::model::AuthConfig> {
    Some(crate::domain::model::AuthConfig {
        plugin_type: plugin_type.into(),
        sharing: crate::domain::model::SharingMode::Private,
        config: None,
    })
}

#[tokio::test]
async fn upstream_refs_are_limited_on_create_and_update() {
    let svc = make_service();
    let ctx = test_ctx(Uuid::new_v4());
    let at_limit = "r".repeat(256);
    let over = "r".repeat(257);

    let upstream = svc
        .create_upstream(
            &ctx,
            CreateUpstreamRequest {
                protocol: at_limit.clone(),
                // Plugin references are checked against the implemented
                // plugins as well, so the at-limit case uses real IDs.
                auth: auth(crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID),
                plugins: plugins(crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID),
                ..make_create_upstream_ip("limits")
            },
        )
        .await
        .expect("a 256-byte protocol is accepted");
    let base = make_create_upstream_ip("limits");
    let cases = [
        (
            "a 257-byte protocol",
            "protocol must not exceed 256 bytes",
            CreateUpstreamRequest {
                protocol: over.clone(),
                ..base.clone()
            },
        ),
        (
            "a NUL in the protocol",
            "protocol must not contain control characters",
            CreateUpstreamRequest {
                protocol: "http\0".into(),
                ..base.clone()
            },
        ),
        (
            "a 257-byte auth plugin type",
            "auth.type must not exceed 256 bytes",
            CreateUpstreamRequest {
                auth: auth(&over),
                ..base.clone()
            },
        ),
        (
            "a NUL in the auth plugin type",
            "auth.type must not contain control characters",
            CreateUpstreamRequest {
                auth: auth("a\0"),
                ..base.clone()
            },
        ),
        (
            "a 257-byte plugin ref",
            "plugins.items[0].plugin_ref must not exceed 256 bytes",
            CreateUpstreamRequest {
                plugins: plugins(&over),
                ..base.clone()
            },
        ),
        (
            "a NUL in a plugin ref",
            "plugins.items[0].plugin_ref must not contain control characters",
            CreateUpstreamRequest {
                plugins: plugins("p\0"),
                ..base.clone()
            },
        ),
    ];
    for (case, needle, bad) in cases {
        let mut alias_free = bad.clone();
        alias_free.alias = Some("limits-bad".into());
        assert_validation_detail(svc.create_upstream(&ctx, alias_free).await, needle, case);
        let mut update = make_update_from_upstream(&upstream);
        update.protocol = bad.protocol;
        update.auth = bad.auth;
        update.plugins = bad.plugins;
        assert_validation_detail(
            svc.update_upstream(&ctx, upstream.id, update).await,
            needle,
            case,
        );
    }
}

#[tokio::test]
async fn route_plugin_refs_and_query_allowlist_are_limited_on_create_and_update() {
    let svc = make_service();
    let ctx = test_ctx(Uuid::new_v4());
    let upstream = svc
        .create_upstream(&ctx, make_create_upstream_ip("limits"))
        .await
        .unwrap();
    let with = |query_allowlist: Vec<String>, plugin_ref: &str| {
        let mut req = make_create_route(upstream.id);
        req.match_rules.http.as_mut().unwrap().query_allowlist = query_allowlist;
        req.plugins = plugins(plugin_ref);
        req
    };

    let route = svc
        .create_route(
            &ctx,
            with(
                tags(64, 128),
                crate::domain::gts_helpers::REQUEST_ID_TRANSFORM_PLUGIN_ID,
            ),
        )
        .await
        .expect("64 query parameters of 128 bytes are accepted");
    let ok = crate::domain::gts_helpers::REQUEST_ID_TRANSFORM_PLUGIN_ID;
    let cases = [
        (
            "65 query parameters",
            "allows at most 64 parameters, got 65",
            with(tags(65, 1), ok),
        ),
        (
            "a 129-byte query parameter",
            "query_allowlist[0] must not exceed 128 bytes",
            with(tags(1, 129), ok),
        ),
        (
            "a NUL in a query parameter",
            "query_allowlist[0] must not contain control characters",
            with(vec!["a\0".into()], ok),
        ),
        (
            "a 257-byte plugin ref",
            "plugins.items[0].plugin_ref must not exceed 256 bytes",
            with(vec![], &"r".repeat(257)),
        ),
        (
            "a NUL in a plugin ref",
            "plugins.items[0].plugin_ref must not contain control characters",
            with(vec![], "p\0"),
        ),
    ];
    for (case, needle, bad) in cases {
        assert_validation_detail(svc.create_route(&ctx, bad.clone()).await, needle, case);
        let mut update = make_update_from_route(&route);
        update.match_rules = bad.match_rules;
        update.plugins = bad.plugins;
        assert_validation_detail(svc.update_route(&ctx, route.id, update).await, needle, case);
    }
}

fn plugin_chain(count: usize) -> Option<crate::domain::model::PluginsConfig> {
    Some(crate::domain::model::PluginsConfig {
        sharing: crate::domain::model::SharingMode::Private,
        items: (0..count)
            .map(|_| crate::domain::model::PluginBinding {
                plugin_ref: crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID.into(),
                config: Default::default(),
            })
            .collect(),
    })
}

#[tokio::test]
async fn plugin_bindings_are_limited_on_create_and_update() {
    let svc = make_service();
    let ctx = test_ctx(Uuid::new_v4());
    let upstream = svc
        .create_upstream(
            &ctx,
            CreateUpstreamRequest {
                plugins: plugin_chain(32),
                ..make_create_upstream_ip("limits")
            },
        )
        .await
        .expect("32 upstream plugins are accepted");
    let over = CreateUpstreamRequest {
        alias: Some("limits-bad".into()),
        plugins: plugin_chain(33),
        ..make_create_upstream_ip("limits")
    };
    assert_validation(svc.create_upstream(&ctx, over).await, "33 upstream plugins");
    let mut update = make_update_from_upstream(&upstream);
    update.plugins = plugin_chain(33);
    assert_validation(
        svc.update_upstream(&ctx, upstream.id, update).await,
        "33 upstream plugins",
    );

    let route = svc
        .create_route(
            &ctx,
            CreateRouteRequest {
                plugins: plugin_chain(32),
                ..make_create_route(upstream.id)
            },
        )
        .await
        .expect("32 route plugins are accepted");
    let over = CreateRouteRequest {
        plugins: plugin_chain(33),
        ..make_create_route(upstream.id)
    };
    assert_validation(svc.create_route(&ctx, over).await, "33 route plugins");
    let mut update = make_update_from_route(&route);
    update.plugins = plugin_chain(33);
    assert_validation(
        svc.update_route(&ctx, route.id, update).await,
        "33 route plugins",
    );
}

/// `PostgreSQL`'s `jsonb` cannot store NUL, so no string stored inside a JSON
/// column may contain one. Other control characters stay allowed there.
#[tokio::test]
async fn nul_in_strings_stored_as_json_is_rejected_on_create_and_update() {
    use crate::domain::model::{
        AuthConfig, HeadersConfig, PluginBinding, PluginsConfig, RequestHeaderRules,
        ResponseHeaderRules,
    };
    use std::collections::HashMap;

    let svc = make_service();
    let ctx = test_ctx(Uuid::new_v4());
    let map = |key: &str, value: &str| HashMap::from([(key.to_owned(), value.to_owned())]);
    let auth = |config| {
        Some(AuthConfig {
            plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Private,
            config: Some(config),
        })
    };
    let request = |rules: RequestHeaderRules| {
        Some(HeadersConfig {
            request: Some(rules),
            response: None,
        })
    };
    let response = |rules: ResponseHeaderRules| {
        Some(HeadersConfig {
            request: None,
            response: Some(rules),
        })
    };
    let plugin_config = |config| {
        Some(PluginsConfig {
            sharing: SharingMode::Private,
            items: vec![PluginBinding {
                plugin_ref: crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID.into(),
                config,
            }],
        })
    };
    let mut cors = make_cors(SharingMode::Private, vec!["https://app.example.com"]);
    cors.expose_headers = vec!["x-trace\0".into()];

    let upstream = svc
        .create_upstream(
            &ctx,
            CreateUpstreamRequest {
                auth: auth(map("prefix", "Bearer\t")),
                plugins: plugin_config(map("note", "line\nbreak")),
                ..make_create_upstream_ip("json-text")
            },
        )
        .await
        .expect("control characters other than NUL are accepted in JSON-stored values");
    let base = CreateUpstreamRequest {
        alias: Some("json-text-bad".into()),
        ..make_create_upstream_ip("json-text")
    };
    let cases = [
        (
            "a NUL in an auth config value",
            CreateUpstreamRequest {
                auth: auth(map("header", "x\0")),
                ..base.clone()
            },
        ),
        (
            "a NUL in an auth config key",
            CreateUpstreamRequest {
                auth: auth(map("x\0", "header")),
                ..base.clone()
            },
        ),
        (
            "a NUL in a set request header",
            CreateUpstreamRequest {
                headers: request(RequestHeaderRules {
                    set: map("x-team", "a\0"),
                    ..Default::default()
                }),
                ..base.clone()
            },
        ),
        (
            "a NUL in a passthrough header",
            CreateUpstreamRequest {
                headers: request(RequestHeaderRules {
                    passthrough_allowlist: vec!["x-\0".into()],
                    ..Default::default()
                }),
                ..base.clone()
            },
        ),
        (
            "a NUL in a removed response header",
            CreateUpstreamRequest {
                headers: response(ResponseHeaderRules {
                    remove: vec!["server\0".into()],
                    ..Default::default()
                }),
                ..base.clone()
            },
        ),
        (
            "a NUL in a CORS exposed header",
            CreateUpstreamRequest {
                cors: Some(cors.clone()),
                ..base.clone()
            },
        ),
        (
            "a NUL in a plugin config value",
            CreateUpstreamRequest {
                plugins: plugin_config(map("timeout_ms", "5\0")),
                ..base.clone()
            },
        ),
    ];
    for (case, bad) in cases {
        assert_validation(svc.create_upstream(&ctx, bad.clone()).await, case);
        let mut update = make_update_from_upstream(&upstream);
        update.auth = bad.auth;
        update.headers = bad.headers;
        update.cors = bad.cors;
        update.plugins = bad.plugins;
        assert_validation(svc.update_upstream(&ctx, upstream.id, update).await, case);
    }

    let route = svc
        .create_route(&ctx, make_create_route(upstream.id))
        .await
        .unwrap();
    let cases = [
        (
            "a NUL in a route plugin config key",
            CreateRouteRequest {
                plugins: plugin_config(map("k\0", "v")),
                ..make_create_route(upstream.id)
            },
        ),
        (
            "a NUL in a route CORS exposed header",
            CreateRouteRequest {
                cors: Some(cors.clone()),
                ..make_create_route(upstream.id)
            },
        ),
    ];
    for (case, bad) in cases {
        assert_validation(svc.create_route(&ctx, bad.clone()).await, case);
        let mut update = make_update_from_route(&route);
        update.plugins = bad.plugins;
        update.cors = bad.cors;
        assert_validation(svc.update_route(&ctx, route.id, update).await, case);
    }
}

/// A derived alias appends a non-standard port to the hostname, and the
/// 253-character alias limit applies to the result, so every stored alias
/// fits the `VARCHAR(253)` alias column.
#[tokio::test]
async fn derived_alias_with_a_port_stays_within_the_alias_limit() {
    let (svc, registry) = in_memory_planes();
    // The longest valid hostname.
    let host = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61)
    );
    assert_eq!(host.len(), 253);
    let mut req = make_create_upstream_hostname();
    req.server.endpoints[0].host = host.clone();

    let ctx = test_ctx(Uuid::new_v4());
    let stored = svc.create_upstream(&ctx, req.clone()).await.unwrap();
    assert_eq!(stored.alias, host);
    let mut with_port = req.clone();
    with_port.server.endpoints[0].port = 8443;
    let err = svc
        .create_upstream(&ctx, with_port.clone())
        .await
        .expect_err("a 258-character alias");
    assert!(err.to_string().contains("must not exceed 253"), "{err}");

    // The registry writer derives the alias by the same rules on update.
    let ctx = test_ctx(Uuid::new_v4());
    let stored = registry.create_registry_upstream(&ctx, req).await.unwrap();
    let mut update = make_update_from_upstream(&stored);
    update.server = with_port.server;
    update.alias = None;
    let err = registry
        .update_registry_upstream(&ctx, stored.id, update)
        .await
        .expect_err("a 258-character alias");
    assert!(err.to_string().contains("must not exceed 253"), "{err}");
}

// ---------------------------------------------------------------------------
// SSRF hostname deny-list at resolution
// ---------------------------------------------------------------------------

/// An upstream stored before its hostname was denied stops resolving.
///
/// ADR-0018: PERS-PROXY-003.
#[tokio::test]
async fn resolution_rejects_an_upstream_whose_hostname_was_denied_later() {
    let upstreams: Arc<dyn UpstreamRepository> = Arc::new(InMemoryUpstreamRepo::new());
    let routes: Arc<dyn RouteRepository> = Arc::new(InMemoryRouteRepo::new());
    let service = |guard| {
        ControlPlaneServiceImpl::new(
            upstreams.clone(),
            routes.clone(),
            Arc::new(MockTenantResolverClient::single_tenant()),
            allow_all_enforcer(),
            Arc::new(MockCredStoreClient::empty()),
            Arc::new(guard),
            None,
        )
    };
    let ctx = test_ctx(Uuid::new_v4());
    let before = service(SsrfGuard::from_config(&SsrfPolicy::default()).unwrap());
    let upstream = before
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    before
        .create_route(&ctx, make_create_route(upstream.id))
        .await
        .unwrap();
    let target = (&upstream.alias, "POST", "/v1/chat/completions");
    before
        .resolve_proxy_target(
            &crate::domain::services::ProxyGrant::for_tests(ctx.clone()),
            target.0,
            target.1,
            target.2,
            Tags::Load,
        )
        .await
        .map(crate::domain::model::ProxyTarget::into_parts)
        .expect("resolves before the policy changes");

    let after = service(
        SsrfGuard::from_config(&SsrfPolicy {
            extra_deny_hostnames: vec!["api.openai.com".into()],
            ..SsrfPolicy::default()
        })
        .unwrap(),
    );
    let err = after
        .resolve_proxy_target(
            &crate::domain::services::ProxyGrant::for_tests(ctx.clone()),
            target.0,
            target.1,
            target.2,
            Tags::Load,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::Validation { detail, .. }
            if detail.contains("is blocked by SSRF protection")),
        "{err:?}"
    );
}

// ---------------------------------------------------------------------------
// Route create racing an upstream delete
// ---------------------------------------------------------------------------

/// The upstream passes the check but is gone when the route is inserted:
/// the foreign-key rejection answers like the check, with a validation error.
#[tokio::test]
async fn route_create_after_its_upstream_was_deleted_is_a_validation_error() {
    use crate::infra::storage::DbRouteRepo;
    use crate::infra::storage::testing::shared_sqlite;
    use toolkit_db::DBProvider;

    let upstreams: Arc<dyn UpstreamRepository> = Arc::new(InMemoryUpstreamRepo::new());
    let db = DBProvider::<RepositoryError>::new(shared_sqlite().await);
    let svc = ControlPlaneServiceImpl::new(
        upstreams,
        Arc::new(DbRouteRepo::new(db)),
        Arc::new(MockTenantResolverClient::single_tenant()),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::empty()),
        Arc::new(SsrfGuard::disabled()),
        None,
    );
    let ctx = test_ctx(Uuid::new_v4());
    // Only the in-memory repository the check reads holds the upstream.
    let upstream = svc
        .create_upstream(&ctx, make_create_upstream_ip("gone"))
        .await
        .unwrap();

    let err = svc
        .create_route(&ctx, make_create_route(upstream.id))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::Validation { detail, .. } if detail.contains("not found for this tenant")),
        "{err:?}"
    );
}

// -- Overlaps among the routes the types registry provides --

#[test]
fn registry_route_overlaps_follow_the_overlap_rule() {
    let tenant = Uuid::new_v4();
    let upstream = Uuid::new_v4();
    let key = |tenant_id| RowKey {
        tenant_id,
        id: Uuid::new_v4(),
    };
    let base = make_create_route(upstream);
    let a = (key(tenant), base.clone());
    let check =
        |b: (RowKey, CreateRouteRequest)| registry_route_overlaps(&[(a.0, &a.1), (b.0, &b.1)]);

    let twin = key(tenant);
    let (low, high) = if a.0.id < twin.id {
        (a.0, twin)
    } else {
        (twin, a.0)
    };
    assert_eq!(
        check((twin, base.clone())),
        vec![(low, high, HttpMethod::Post)]
    );

    // Same content elsewhere, or not competing for a request: no overlap.
    let other_upstream = make_create_route(Uuid::new_v4());
    assert!(check((key(tenant), other_upstream)).is_empty());
    assert!(check((key(Uuid::new_v4()), base.clone())).is_empty());
    let disabled = CreateRouteRequest {
        enabled: false,
        ..base.clone()
    };
    assert!(check((key(tenant), disabled)).is_empty());
    let other_priority = CreateRouteRequest {
        priority: 1,
        ..base.clone()
    };
    assert!(check((key(tenant), other_priority)).is_empty());
    let mut other_method = base.clone();
    other_method.match_rules.http.as_mut().unwrap().methods = vec![HttpMethod::Get];
    assert!(check((key(tenant), other_method)).is_empty());
    let mut grpc = base;
    grpc.match_rules = MatchRules {
        http: None,
        grpc: Some(GrpcMatch {
            service: "svc".into(),
            method: "M".into(),
        }),
    };
    assert!(check((key(tenant), grpc)).is_empty());
}

/// Every overlapping pair is reported, not only the first: three routes at
/// the same path and method are three pairs.
#[test]
fn registry_route_overlaps_reports_every_pair() {
    let tenant = Uuid::new_v4();
    let base = make_create_route(Uuid::new_v4());
    let mut keys: Vec<RowKey> = (0..3)
        .map(|_| RowKey {
            tenant_id: tenant,
            id: Uuid::new_v4(),
        })
        .collect();
    let routes: Vec<_> = keys.iter().map(|k| (*k, &base)).collect();
    let pairs = registry_route_overlaps(&routes);
    keys.sort();
    assert_eq!(
        pairs
            .iter()
            .map(|(a, b, _)| (a.id, b.id))
            .collect::<Vec<_>>(),
        vec![
            (keys[0].id, keys[1].id),
            (keys[0].id, keys[2].id),
            (keys[1].id, keys[2].id)
        ]
    );
}

// -- Ownership: the registry reconcile never writes API rows --

#[tokio::test]
async fn registry_writes_to_api_rows_are_conflicts() {
    let (svc, registry) = in_memory_planes();
    let ctx = test_ctx(Uuid::new_v4());
    let upstream = svc
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    let route = svc
        .create_route(&ctx, make_create_route(upstream.id))
        .await
        .unwrap();

    let assert_api_owned = |result: Result<(), DomainError>, entity: &str, id: Uuid| match result {
        Err(DomainError::Conflict { detail, .. }) => assert_eq!(
            detail,
            format!("{entity} {id} was created through the Management API, not the types registry")
        ),
        other => panic!("expected Conflict for {entity} {id}, got {other:?}"),
    };
    assert_api_owned(
        registry
            .update_registry_upstream(&ctx, upstream.id, make_update_from_upstream(&upstream))
            .await
            .map(drop),
        "upstream",
        upstream.id,
    );
    assert_api_owned(
        registry
            .update_registry_route(
                &ctx,
                route.id,
                route.upstream_id,
                make_update_from_route(&route),
                UpstreamCheck::Read,
            )
            .await
            .map(drop),
        "route",
        route.id,
    );
    assert_api_owned(
        registry.delete_registry_route(&ctx, route.id).await,
        "route",
        route.id,
    );
    assert!(svc.get_route(&ctx, route.id).await.is_ok());
    assert!(svc.get_upstream(&ctx, upstream.id).await.is_ok());
}

// ---------------------------------------------------------------------------
// Resolution through the proxy configuration cache (ADR-0020)
// ---------------------------------------------------------------------------

/// A [`ProxyConfigReader`] over a real [`CachedProxyConfigReader`] that
/// counts its calls, so a test knows resolution went through it.
struct CountingReader {
    inner: crate::infra::proxy_cache::CachedProxyConfigReader,
    aliases: std::sync::atomic::AtomicUsize,
    routes: std::sync::atomic::AtomicUsize,
}

impl CountingReader {
    fn calls(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering::SeqCst;
        (self.aliases.load(SeqCst), self.routes.load(SeqCst))
    }
}

#[async_trait]
impl ProxyConfigReader for CountingReader {
    async fn upstreams_by_alias(
        &self,
        alias: &str,
        chain: &[Uuid],
    ) -> Result<Arc<[Upstream]>, RepositoryError> {
        self.aliases
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.upstreams_by_alias(alias, chain).await
    }

    async fn routes_of(&self, upstreams: &[RowKey]) -> Result<Vec<Arc<[Route]>>, RepositoryError> {
        self.routes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.routes_of(upstreams).await
    }
}

/// Two Control Planes over one store and tenant hierarchy (root first): the
/// first resolves through a proxy configuration cache over the store, the
/// second reads the store. The reader counts the cached one's reads.
fn cached_and_uncached(
    stores: &Stores,
    hierarchy: &[Uuid],
) -> (
    ControlPlaneServiceImpl,
    ControlPlaneServiceImpl,
    Arc<CountingReader>,
) {
    use crate::infra::proxy_cache::{CachedProxyConfigReader, ProxyConfigCache};

    let reader = Arc::new(CountingReader {
        inner: CachedProxyConfigReader::new(
            Arc::new(ProxyConfigCache::new(
                std::time::Duration::from_secs(60),
                std::time::Duration::ZERO,
                100,
                Arc::new(crate::domain::ports::NoopMetrics),
            )),
            stores.upstreams.clone(),
            stores.routes.clone(),
        ),
        aliases: std::sync::atomic::AtomicUsize::new(0),
        routes: std::sync::atomic::AtomicUsize::new(0),
    });
    let service = |reader: Option<Arc<dyn ProxyConfigReader>>| {
        ControlPlaneServiceImpl::new(
            stores.upstreams.clone(),
            stores.routes.clone(),
            Arc::new(MockTenantResolverClient::with_hierarchy(
                hierarchy.iter().copied().map(TenantId).collect(),
            )),
            allow_all_enforcer(),
            Arc::new(MockCredStoreClient::empty()),
            Arc::new(SsrfGuard::disabled()),
            reader,
        )
    };
    (service(Some(reader.clone())), service(None), reader)
}

/// What `tenant` resolves `alias` (and `method_path`) to, as a comparable
/// value: the effective upstream and route, or the error.
async fn resolution(
    svc: &ControlPlaneServiceImpl,
    tenant: Uuid,
    alias: &str,
    method_path: Option<(&str, &str)>,
) -> Resolved {
    let ctx = test_ctx(tenant);
    let chain = svc.build_tenant_chain(&ctx).await.unwrap();
    svc.resolve_alias(&ctx, &chain, alias, method_path, Tags::Skip)
        .await
        .map_err(Failed)
}

/// Over four levels, with a shadowing child, an unshared middle upstream,
/// inherited rate limits and routes on several levels, every tenant resolves
/// every target through the cache to the same upstream, route and effective
/// config as without it, on a miss and on a hit.
///
/// ADR-0020: CACHE-HIT-002.
async fn cached_resolution_matches_uncached(backend: Backend) {
    let [root, a, b, leaf] = std::array::from_fn(|_| Uuid::new_v4());
    let stores = Stores::new(backend);
    let (cached, uncached, reader) = cached_and_uncached(&stores, &[root, a, b, leaf]);
    let shared = |rate| CreateUpstreamRequest {
        auth: Some(AuthConfig {
            plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Inherit,
            config: None,
        }),
        rate_limit: Some(make_rate_limit(SharingMode::Inherit, rate, Window::Minute)),
        ..make_create_upstream_hostname()
    };
    let route = |upstream_id, methods, path: &str, priority| CreateRouteRequest {
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods,
                path: path.into(),
                query_allowlist: vec![],
                path_suffix_mode: PathSuffixMode::Append,
            }),
            grpc: None,
        },
        priority,
        ..make_create_route(upstream_id)
    };
    let root_up = uncached
        .create_upstream(&test_ctx(root), shared(1000))
        .await
        .unwrap();
    let a_up = uncached
        .create_upstream(&test_ctx(a), make_create_upstream_hostname())
        .await
        .unwrap();
    let b_up = uncached
        .create_upstream(&test_ctx(b), shared(100))
        .await
        .unwrap();
    let root_chat = uncached
        .create_route(&test_ctx(root), make_create_route(root_up.id))
        .await
        .unwrap();
    let root_v1 = CreateRouteRequest {
        rate_limit: Some(make_rate_limit(SharingMode::Private, 10, Window::Minute)),
        ..route(
            root_up.id,
            vec![HttpMethod::Get, HttpMethod::Post],
            "/v1",
            0,
        )
    };
    uncached
        .create_route(&test_ctx(root), root_v1)
        .await
        .unwrap();
    // A better match on the unshared upstream, which b and leaf must not see.
    uncached
        .create_route(
            &test_ctx(a),
            route(a_up.id, vec![HttpMethod::Post], "/v1/chat/completions", 9),
        )
        .await
        .unwrap();
    let b_embeddings = uncached
        .create_route(
            &test_ctx(b),
            route(b_up.id, vec![HttpMethod::Post], "/v1/embeddings", 1),
        )
        .await
        .unwrap();
    // Shadowed by b's: its enforced guard still applies to b and leaf.
    let root_embeddings = CreateRouteRequest {
        plugins: Some(plugins_of(SharingMode::Enforce, &[(RH, "root")])),
        ..route(root_up.id, vec![HttpMethod::Post], "/v1/embeddings", 0)
    };
    uncached
        .create_route(&test_ctx(root), root_embeddings)
        .await
        .unwrap();

    let targets = [
        None,
        Some(("POST", "/v1/chat/completions")),
        Some(("POST", "/v1/embeddings/x")),
        Some(("GET", "/v1/models")),
        Some(("DELETE", "/v1/models")),
        Some(("BREW", "/v1")),
        Some(("POST", "/v2")),
    ];
    for pass in ["miss", "hit"] {
        for tenant in [root, a, b, leaf] {
            for target in targets {
                assert_eq!(
                    resolution(&cached, tenant, "API.openai.com", target).await,
                    resolution(&uncached, tenant, "API.openai.com", target).await,
                    "{pass}: tenant {tenant}, {target:?}"
                );
            }
        }
    }
    let lookups = 2 * 4 * targets.len();
    assert_eq!(reader.calls().0, lookups, "every lookup reads its alias");
    assert_eq!(
        reader.calls().1,
        lookups - 2 * 4 * 2,
        "every lookup with a known method reads its routes"
    );

    // The hierarchy resolves as designed, so the comparison covers it.
    let resolved = async |tenant, target| {
        let (upstream, route) = resolution(&cached, tenant, "api.openai.com", Some(target))
            .await
            .unwrap();
        (upstream.id, route.map(|r| r.id))
    };
    let chat = ("POST", "/v1/chat/completions");
    let embeddings = ("POST", "/v1/embeddings/x");
    assert_eq!(resolved(leaf, chat).await, (b_up.id, Some(root_chat.id)));
    assert_eq!(
        resolved(leaf, embeddings).await,
        (b_up.id, Some(b_embeddings.id))
    );
    let (effective, _) = resolution(&cached, leaf, "api.openai.com", Some(embeddings))
        .await
        .unwrap();
    assert_eq!(plugin_refs(effective.plugins.as_ref()), vec![(RH, "root")]);
    assert_eq!(
        resolved(a, chat).await.0,
        a_up.id,
        "its own, though unshared"
    );
    assert_ne!(resolved(a, chat).await.1, Some(root_chat.id));
}

mod cached_resolution_matches_uncached {
    use super::{Backend, cached_resolution_matches_uncached as case};

    #[tokio::test]
    async fn in_memory() {
        case(Backend::Memory).await;
    }

    #[tokio::test]
    async fn sqlite() {
        case(Backend::Db(
            crate::infra::storage::testing::shared_sqlite().await,
        ))
        .await;
    }

    #[cfg(feature = "integration")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn postgres() {
        let server = crate::infra::storage::testing::postgres_server().await;
        case(Backend::Db(server.fresh().await)).await;
    }

    #[cfg(feature = "integration")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mysql() {
        let server = crate::infra::storage::testing::mysql_server().await;
        case(Backend::Db(server.fresh().await)).await;
    }
}

/// Through the cache, an unknown alias is not found, an alias whose visible
/// upstreams are all disabled is disabled, and a target no route matches is
/// a route not found, as without it, on a miss and on a hit.
///
/// ADR-0020: CACHE-HIT-002.
#[tokio::test]
async fn cached_resolution_answers_not_found_and_disabled_as_uncached() {
    let [root, child] = std::array::from_fn(|_| Uuid::new_v4());
    let stores = Stores::new(Backend::Db(
        crate::infra::storage::testing::shared_sqlite().await,
    ));
    let (cached, uncached, reader) = cached_and_uncached(&stores, &[root, child]);
    let disabled = CreateUpstreamRequest {
        auth: Some(AuthConfig {
            plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Inherit,
            config: None,
        }),
        enabled: false,
        ..make_create_upstream_ip("off")
    };
    uncached
        .create_upstream(&test_ctx(root), disabled)
        .await
        .unwrap();
    let up = uncached
        .create_upstream(&test_ctx(child), make_create_upstream_hostname())
        .await
        .unwrap();
    uncached
        .create_route(&test_ctx(child), make_create_route(up.id))
        .await
        .unwrap();

    let not_found: fn(&Resolved) -> bool = is_not_found;
    let cases = [
        ("nope", Some(("POST", "/v1/chat/completions")), not_found),
        ("off", None, is_disabled),
        ("off", Some(("POST", "/v1/chat/completions")), is_disabled),
        (
            "api.openai.com",
            Some(("GET", "/v1/chat/completions")),
            not_found,
        ),
        ("api.openai.com", Some(("POST", "/v2")), not_found),
    ];
    for pass in ["miss", "hit"] {
        for (alias, target, expected) in cases {
            let got = resolution(&cached, child, alias, target).await;
            assert_eq!(
                got,
                resolution(&uncached, child, alias, target).await,
                "{pass}: {alias} {target:?}"
            );
            assert!(expected(&got), "{pass}: {alias} {target:?}: {got:?}");
        }
    }
    assert_eq!(reader.calls(), (10, 4));
}

/// An alias no upstream can have (longer than the alias rules allow, with a
/// character an alias cannot hold, or empty once normalized) is not found,
/// as without the cache, and never reaches the cache: nothing is loaded or
/// stored for it.
///
/// ADR-0020: CACHE-HIT-002.
#[tokio::test]
async fn an_alias_no_upstream_can_have_is_not_found_without_touching_the_cache() {
    let tenant = Uuid::new_v4();
    let stores = Stores::new(Backend::Db(
        crate::infra::storage::testing::shared_sqlite().await,
    ));
    let (cached, uncached, reader) = cached_and_uncached(&stores, &[tenant]);
    let too_long = "a".repeat(MAX_ALIAS_LENGTH + 1);
    for alias in [too_long.as_str(), "api/openai", "...", "-"] {
        for target in [None, Some(("GET", "/v1"))] {
            assert_eq!(
                resolution(&cached, tenant, alias, target).await,
                resolution(&uncached, tenant, alias, target).await,
                "{alias} {target:?}"
            );
            let ctx = test_ctx(tenant);
            let chain = cached.build_tenant_chain(&ctx).await.unwrap();
            let err = cached
                .resolve_alias(&ctx, &chain, alias, target, Tags::Skip)
                .await
                .unwrap_err();
            assert!(
                matches!(err, DomainError::NotFound { .. }),
                "{alias} {target:?}: {err:?}"
            );
        }
    }
    assert_eq!(reader.calls(), (0, 0), "nothing loaded or stored");
}

/// A lookup with tags reads the repositories, never the cache: the cache
/// holds no tags.
///
/// ADR-0020: CACHE-CONF-003.
#[tokio::test]
async fn resolution_with_tags_bypasses_the_cache() {
    let tenant = Uuid::new_v4();
    let stores = Stores::new(Backend::Db(
        crate::infra::storage::testing::shared_sqlite().await,
    ));
    let (cached, _, reader) = cached_and_uncached(&stores, &[tenant]);
    let ctx = test_ctx(tenant);
    let up = cached
        .create_upstream(
            &ctx,
            CreateUpstreamRequest {
                tags: vec!["team-a".into()],
                ..make_create_upstream_hostname()
            },
        )
        .await
        .unwrap();
    cached
        .create_route(
            &ctx,
            CreateRouteRequest {
                tags: vec!["chat".into()],
                ..make_create_route(up.id)
            },
        )
        .await
        .unwrap();

    for _ in 0..2 {
        let ProxyTarget {
            upstream, route, ..
        } = cached
            .resolve_proxy_target(
                &crate::domain::services::ProxyGrant::for_tests(ctx.clone()),
                "api.openai.com",
                "POST",
                "/v1/chat/completions",
                Tags::Load,
            )
            .await
            .unwrap();
        // The effective config carries the upstream's and the route's tags.
        assert_eq!(upstream.tags, ["team-a", "chat"]);
        assert_eq!(route.tags, ["chat"]);
    }
    assert_eq!(reader.calls(), (0, 0));
}

// ---------------------------------------------------------------------------
// Freshness of the proxy configuration cache (ADR-0020)
// ---------------------------------------------------------------------------

/// On one Control Plane resolving through the cache, the next lookup sees
/// each upstream and route write, including the registry reconcile's
/// upstream renames and route moves.
///
/// ADR-0020: CACHE-FRESH-001.
#[tokio::test]
async fn each_write_is_seen_by_the_next_cached_lookup() {
    let tenant = Uuid::new_v4();
    let f = CachedPlanes::new(Arc::new(MockTenantResolverClient::single_tenant())).await;
    let ctx = test_ctx(tenant);
    let chat = Some(("POST", "/v1/chat/completions"));
    let openai = |target| (tenant, "api.openai.com", target);

    let up = f
        .assert_sees(
            "upstream create",
            &[openai(None), openai(chat)],
            async || {
                f.cp.create_upstream(&ctx, make_create_upstream_hostname())
                    .await
                    .unwrap()
            },
        )
        .await;
    let route = f
        .assert_sees("route create", &[openai(chat)], async || {
            f.cp.create_route(&ctx, make_create_route(up.id))
                .await
                .unwrap()
        })
        .await;
    let route = f
        .assert_sees("route update", &[openai(chat)], async || {
            let req = UpdateRouteRequest {
                priority: 5,
                ..make_update_from_route(&route)
            };
            f.cp.update_route(&ctx, route.id, req).await.unwrap()
        })
        .await;
    let mut route = route;
    for (name, enabled) in [("route disable", false), ("route enable", true)] {
        route = f
            .assert_sees(name, &[openai(chat)], async || {
                let req = UpdateRouteRequest {
                    enabled,
                    ..make_update_from_route(&route)
                };
                f.cp.update_route(&ctx, route.id, req).await.unwrap()
            })
            .await;
    }
    let up = f
        .assert_sees(
            "upstream update",
            &[openai(None), openai(chat)],
            async || {
                let req = UpdateUpstreamRequest {
                    rate_limit: Some(make_rate_limit(SharingMode::Private, 10, Window::Minute)),
                    ..make_update_from_upstream(&up)
                };
                f.cp.update_upstream(&ctx, up.id, req).await.unwrap()
            },
        )
        .await;
    for (name, enabled) in [("upstream disable", false), ("upstream enable", true)] {
        f.assert_sees(name, &[openai(None), openai(chat)], async || {
            let req = UpdateUpstreamRequest {
                enabled,
                ..make_update_from_upstream(&up)
            };
            f.cp.update_upstream(&ctx, up.id, req).await.unwrap()
        })
        .await;
    }
    f.assert_sees("route delete", &[openai(chat)], async || {
        f.cp.delete_route(&ctx, route.id).await.unwrap();
    })
    .await;
    f.assert_sees("upstream delete", &[openai(None)], async || {
        f.cp.delete_upstream(&ctx, up.id).await.unwrap();
    })
    .await;

    let registry = &f.provisioner;
    let from = registry
        .create_registry_upstream(&ctx, make_create_upstream_ip("from"))
        .await
        .unwrap();
    let to = registry
        .create_registry_upstream(&ctx, make_create_upstream_ip("to"))
        .await
        .unwrap();
    let route = registry
        .create_registry_route(&ctx, make_create_route(from.id), UpstreamCheck::Read)
        .await
        .unwrap();
    f.assert_sees(
        "route move",
        &[(tenant, "from", chat), (tenant, "to", chat)],
        async || {
            registry
                .update_registry_route(
                    &ctx,
                    route.id,
                    to.id,
                    make_update_from_route(&route),
                    UpstreamCheck::Read,
                )
                .await
                .unwrap()
        },
    )
    .await;
    let renamed = f
        .assert_sees(
            "upstream rename",
            &[
                (tenant, "to", None),
                (tenant, "to", chat),
                (tenant, "renamed", None),
                (tenant, "renamed", chat),
            ],
            async || {
                let req = UpdateUpstreamRequest {
                    alias: Some("renamed".into()),
                    ..make_update_from_upstream(&to)
                };
                registry
                    .update_registry_upstream(&ctx, to.id, req)
                    .await
                    .unwrap()
            },
        )
        .await;
    assert_eq!(renamed.alias, "renamed");
}

/// An upstream repository whose update commits, then never returns.
struct UpdateThenHang {
    inner: Arc<dyn UpstreamRepository>,
    committed: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl UpstreamRepository for UpdateThenHang {
    async fn create(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        self.inner.create(upstream).await
    }

    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Upstream, RepositoryError> {
        self.inner.get_by_id(tenant_id, id).await
    }

    async fn list(
        &self,
        tenant_id: Uuid,
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        self.inner.list(tenant_id, query).await
    }

    async fn update(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        self.inner.update(upstream).await?;
        self.committed.notify_one();
        std::future::pending().await
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
}

/// A write whose future is dropped after its commit (its HTTP request was
/// cancelled) is still seen by the next lookup.
///
/// ADR-0020: CACHE-FRESH-002.
#[tokio::test]
async fn a_write_dropped_after_its_commit_is_seen_by_the_next_lookup() {
    let tenant = Uuid::new_v4();
    let (db, recorder) = crate::infra::storage::testing::recorded_sqlite().await;
    let stores = Stores::new(Backend::Db(db));
    let committed = Arc::new(tokio::sync::Notify::new());
    let hanging = UpdateThenHang {
        inner: stores.upstreams.clone(),
        committed: committed.clone(),
    };
    let f = CachedPlanes::over(
        Stores {
            upstreams: Arc::new(hanging),
            ..stores
        },
        recorder,
        Arc::new(MockTenantResolverClient::single_tenant()),
    );
    let ctx = test_ctx(tenant);
    let up =
        f.cp.create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();

    let disabled = UpdateUpstreamRequest {
        enabled: false,
        ..make_update_from_upstream(&up)
    };
    let lookup = (tenant, "api.openai.com", None);
    f.assert_sees("a dropped upstream update", &[lookup], async || {
        let update = f.cp.update_upstream(&ctx, up.id, disabled);
        tokio::select! {
            result = update => panic!("the update never returns: {result:?}"),
            () = committed.notified() => {}
        }
        // `update` is dropped here, after its commit.
    })
    .await;
}

/// Writes in an ancestor tenant, sharing changes included, change a
/// descendant's next lookup through the cache.
///
/// ADR-0020: CACHE-FRESH-004.
#[tokio::test]
async fn an_ancestor_write_is_seen_by_a_descendant() {
    let [root, child] = std::array::from_fn(|_| Uuid::new_v4());
    let f = CachedPlanes::new(Arc::new(MockTenantResolverClient::with_hierarchy(vec![
        TenantId(root),
        TenantId(child),
    ])))
    .await;
    let ctx = test_ctx(root);
    let chat = Some(("POST", "/v1/chat/completions"));
    let lookups = [
        (child, "api.openai.com", None),
        (child, "api.openai.com", chat),
    ];
    let auth = |sharing| {
        Some(AuthConfig {
            plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
            sharing,
            config: None,
        })
    };
    let shared = CreateUpstreamRequest {
        auth: auth(SharingMode::Inherit),
        ..make_create_upstream_hostname()
    };

    let up = f
        .assert_sees("ancestor upstream create", &lookups, async || {
            f.cp.create_upstream(&ctx, shared).await.unwrap()
        })
        .await;
    f.assert_sees("ancestor route create", &lookups[1..], async || {
        f.cp.create_route(&ctx, make_create_route(up.id))
            .await
            .unwrap()
    })
    .await;
    // Private: nothing on the upstream is shared, so the descendant no
    // longer sees it; shared again, it does.
    let mut up = up;
    for (name, sharing, visible) in [
        (
            "ancestor upstream made private",
            SharingMode::Private,
            false,
        ),
        ("ancestor upstream shared again", SharingMode::Inherit, true),
    ] {
        up = f
            .assert_sees(name, &lookups, async || {
                let req = UpdateUpstreamRequest {
                    auth: auth(sharing),
                    ..make_update_from_upstream(&up)
                };
                f.cp.update_upstream(&ctx, up.id, req).await.unwrap()
            })
            .await;
        let answer = f.cached(lookups[0]).await;
        if visible {
            assert_eq!(answer.map(|(u, _)| u.id), Ok(up.id), "{name}");
        } else {
            assert!(is_not_found(&answer), "{name}: {answer:?}");
        }
    }
    let up = f
        .assert_sees("ancestor upstream update", &lookups, async || {
            let req = UpdateUpstreamRequest {
                rate_limit: Some(make_rate_limit(SharingMode::Inherit, 10, Window::Minute)),
                ..make_update_from_upstream(&up)
            };
            f.cp.update_upstream(&ctx, up.id, req).await.unwrap()
        })
        .await;
    f.assert_sees("ancestor upstream disable", &lookups, async || {
        let req = UpdateUpstreamRequest {
            enabled: false,
            ..make_update_from_upstream(&up)
        };
        f.cp.update_upstream(&ctx, up.id, req).await.unwrap()
    })
    .await;
    f.assert_sees("ancestor upstream delete", &lookups, async || {
        f.cp.delete_upstream(&ctx, up.id).await.unwrap();
    })
    .await;
    for lookup in lookups {
        let answer = f.cached(lookup).await;
        assert!(is_not_found(&answer), "deleted: {lookup:?}: {answer:?}");
    }
}

/// A tenant whose chain changed (it moved under another parent) resolves
/// under a new key: the answer for its new chain, while its old chain's
/// entry stays cached.
///
/// ADR-0020: CACHE-FRESH-006.
#[tokio::test]
async fn a_changed_tenant_chain_resolves_under_a_new_key() {
    let [root, before, after, tenant] = std::array::from_fn(|_| Uuid::new_v4());
    let f = CachedPlanes::new(Arc::new(MockTenantResolverClient::single_tenant())).await;
    let shared = CreateUpstreamRequest {
        auth: Some(AuthConfig {
            plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Inherit,
            config: None,
        }),
        ..make_create_upstream_ip("x")
    };
    let mut ids = Vec::new();
    for parent in [before, after] {
        let up =
            f.cp.create_upstream(&test_ctx(parent), shared.clone())
                .await
                .unwrap();
        ids.push(up.id);
    }
    // As `build_tenant_chain` returns them: the tenant, then its ancestors.
    let (old, new) = ([tenant, before, root], [tenant, after, root]);
    let lookup = async |svc: &ControlPlaneServiceImpl, chain: &[Uuid]| {
        svc.resolve_alias(&test_ctx(tenant), chain, "x", None, Tags::Skip)
            .await
            .map(|(upstream, _)| upstream.id)
            .map_err(Failed)
    };

    assert_eq!(lookup(&f.cp, &old).await, Ok(ids[0]));
    let selects = f.selects();
    assert_eq!(lookup(&f.cp, &old).await, Ok(ids[0]));
    assert_eq!(f.selects(), selects, "the old chain's answer is cached");

    assert_eq!(lookup(&f.cp, &new).await, Ok(ids[1]));
    assert_eq!(lookup(&f.uncached, &new).await, Ok(ids[1]));
    assert!(f.selects() > selects, "the new chain loads");

    let selects = f.selects();
    assert_eq!(lookup(&f.cp, &old).await, Ok(ids[0]));
    assert_eq!(f.selects(), selects, "the old chain's entry stays cached");
}

/// The SSRF check runs on every lookup, also on an upstream served from the
/// cache: a Control Plane whose policy denies the host rejects an upstream
/// another Control Plane cached, from that entry (no `SELECT`).
///
/// ADR-0020: CACHE-SAFE-001.
#[tokio::test]
async fn a_cached_upstream_whose_host_is_denied_later_is_rejected() {
    use crate::infra::proxy_cache::{CachedProxyConfigReader, ProxyConfigCache};
    use toolkit_db::test_support::QueryKind;

    let (db, recorder) = crate::infra::storage::testing::recorded_sqlite().await;
    let stores = Stores::new(Backend::Db(db));
    let reader: Arc<dyn ProxyConfigReader> = Arc::new(CachedProxyConfigReader::new(
        Arc::new(ProxyConfigCache::new(
            std::time::Duration::from_secs(60),
            std::time::Duration::ZERO,
            100,
            Arc::new(crate::domain::ports::NoopMetrics),
        )),
        stores.upstreams.clone(),
        stores.routes.clone(),
    ));
    let service = |policy: SsrfPolicy| {
        ControlPlaneServiceImpl::new(
            stores.upstreams.clone(),
            stores.routes.clone(),
            Arc::new(MockTenantResolverClient::single_tenant()),
            allow_all_enforcer(),
            Arc::new(MockCredStoreClient::empty()),
            Arc::new(SsrfGuard::from_config(&policy).unwrap()),
            Some(reader.clone()),
        )
    };
    let selects = || {
        recorder
            .events()
            .iter()
            .filter(|e| e.kind == QueryKind::Select)
            .count()
    };
    let ctx = test_ctx(Uuid::new_v4());
    let before = service(SsrfPolicy::default());
    let upstream = before
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    before
        .create_route(&ctx, make_create_route(upstream.id))
        .await
        .unwrap();
    let target = (&upstream.alias, "POST", "/v1/chat/completions");
    before
        .resolve_proxy_target(
            &crate::domain::services::ProxyGrant::for_tests(ctx.clone()),
            target.0,
            target.1,
            target.2,
            Tags::Skip,
        )
        .await
        .map(crate::domain::model::ProxyTarget::into_parts)
        .expect("resolves before the policy changes");

    let after = service(SsrfPolicy {
        extra_deny_hostnames: vec!["api.openai.com".into()],
        ..SsrfPolicy::default()
    });
    let cached = selects();
    let err = after
        .resolve_proxy_target(
            &crate::domain::services::ProxyGrant::for_tests(ctx.clone()),
            target.0,
            target.1,
            target.2,
            Tags::Skip,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::Validation { detail, .. }
            if detail.contains("is blocked by SSRF protection")),
        "{err:?}"
    );
    assert_eq!(selects(), cached, "the upstream came from the cache");
}

/// An encoded separator that selects another route is a 400 on both read
/// paths: through the proxy cache (`Tags::Skip`) and the repositories.
#[tokio::test]
async fn an_encoded_separator_bypass_is_refused_through_the_cache_too() {
    use crate::infra::proxy_cache::{CachedProxyConfigReader, ProxyConfigCache};

    let stores = Stores::new(Backend::Memory);
    let reader: Arc<dyn ProxyConfigReader> = Arc::new(CachedProxyConfigReader::new(
        Arc::new(ProxyConfigCache::new(
            std::time::Duration::from_secs(60),
            std::time::Duration::ZERO,
            100,
            Arc::new(crate::domain::ports::NoopMetrics),
        )),
        stores.upstreams.clone(),
        stores.routes.clone(),
    ));
    let svc = ControlPlaneServiceImpl::new(
        stores.upstreams.clone(),
        stores.routes.clone(),
        Arc::new(MockTenantResolverClient::single_tenant()),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::empty()),
        Arc::new(SsrfGuard::disabled()),
        Some(reader),
    );
    let ctx = test_ctx(Uuid::new_v4());
    let upstream = svc
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    let mut narrow_id = None;
    for path in ["/v1", "/v1/models"] {
        let mut req = make_create_route(upstream.id);
        req.match_rules.http.as_mut().unwrap().path = path.into();
        narrow_id = Some(svc.create_route(&ctx, req).await.unwrap().id);
    }
    let grant = crate::domain::services::ProxyGrant::for_tests(ctx.clone());
    for tags in [Tags::Skip, Tags::Load] {
        let err = svc
            .resolve_proxy_target(&grant, &upstream.alias, "POST", "/v1%2Fmodels", tags)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DomainError::Validation { reason, instance, .. }
                if *reason == oagw_sdk::field::INVALID_PROXY_PATH && instance == "/v1%2Fmodels"),
            "{tags:?}: {err:?}"
        );
        let ProxyTarget { route, .. } = svc
            .resolve_proxy_target(&grant, &upstream.alias, "POST", "/v1/models", tags)
            .await
            .unwrap();
        assert_eq!(Some(route.id), narrow_id, "{tags:?}");
    }
}

fn ep(scheme: Scheme, host: &str) -> Endpoint {
    Endpoint {
        scheme,
        host: host.into(),
        port: 443,
    }
}

fn on(hosts: &[&str]) -> Server {
    Server {
        endpoints: hosts.iter().map(|h| ep(Scheme::Https, h)).collect(),
    }
}

#[test]
fn server_within_is_a_one_way_subset_check() {
    use super::merge::server_within;

    let owner = on(&["a.example", "b.example"]);
    assert!(server_within(&owner, &owner), "equal");
    assert!(
        server_within(&on(&["b.example", "a.example"]), &owner),
        "order"
    );
    assert!(server_within(&on(&["b.example"]), &owner), "subset");
    assert!(
        server_within(&on(&["A.Example."]), &owner),
        "case and trailing dot"
    );
    assert!(
        !server_within(&on(&["a.example", "c.example"]), &owner),
        "extra endpoint"
    );
    assert!(!server_within(&on(&["c.example"]), &owner), "disjoint");
    assert!(!server_within(&on(&[]), &owner), "empty server");
    assert!(!server_within(&on(&["a.example"]), &on(&[])), "empty owner");
    let other_port = Server {
        endpoints: vec![Endpoint {
            port: 8443,
            ..ep(Scheme::Https, "a.example")
        }],
    };
    assert!(!server_within(&other_port, &owner), "port");
    let plaintext = Server {
        endpoints: vec![ep(Scheme::Http, "a.example")],
    };
    assert!(!server_within(&plaintext, &owner), "scheme");
}

/// An upstream on `hosts` with the given auth sharing (none for `None`) and,
/// when `headers` is set, a request header naming it.
fn layer(hosts: &[&str], auth: Option<SharingMode>, headers: Option<&str>) -> Upstream {
    let mut u = make_upstream(
        Uuid::new_v4(),
        "svc",
        auth.map(|sharing| AuthConfig {
            plugin_type: crate::domain::gts_helpers::APIKEY_AUTH_PLUGIN_ID.into(),
            sharing,
            config: None,
        }),
        None,
        None,
        vec![],
    );
    u.server = on(hosts);
    u.headers = headers.map(|value| crate::domain::model::HeadersConfig {
        request: Some(crate::domain::model::RequestHeaderRules {
            set: [("x-owner".to_owned(), value.to_owned())].into(),
            ..Default::default()
        }),
        response: None,
    });
    u
}

fn merged(chain: &[Upstream]) -> (Result<Upstream, DomainError>, Vec<super::merge::Withheld>) {
    let mut withheld = Vec::new();
    let result = super::merge::compute_effective_config_reporting(
        chain,
        None,
        &[],
        &mut |w| withheld.push(w),
        &mut |_| {},
    );
    (result, withheld)
}

#[test]
fn a_child_on_part_of_the_owners_endpoints_keeps_the_credential() {
    let root = layer(&["a", "a2"], Some(SharingMode::Inherit), Some("root"));
    let child = layer(&["a"], None, None);
    let (effective, withheld) = merged(&[root.clone(), child]);
    let effective = effective.unwrap();
    assert!(effective.auth.is_some());
    assert!(effective.headers.is_some());
    assert!(withheld.is_empty());
}

#[test]
fn a_child_with_an_endpoint_the_owner_lacks_loses_the_credential() {
    use super::merge::WithheldKind;

    let root = layer(&["a"], Some(SharingMode::Inherit), Some("root"));
    let child = layer(&["a", "b"], None, None);
    let (effective, withheld) = merged(&[root.clone(), child.clone()]);
    let effective = effective.unwrap();
    assert!(effective.auth.is_none());
    assert!(effective.headers.is_none());
    let kinds: Vec<_> = withheld.iter().map(|w| w.kind).collect();
    assert_eq!(kinds, vec![WithheldKind::Auth, WithheldKind::Headers]);
    assert!(
        withheld
            .iter()
            .all(|w| w.owner_id == root.id && w.upstream_id == child.id)
    );
}

/// The credential is checked against its owner's endpoints, not the level
/// above: a leaf may use an endpoint the root lists that the middle tenant
/// left out.
#[test]
fn an_enforced_auth_is_checked_against_its_owner() {
    let root = layer(&["a", "a2"], Some(SharingMode::Enforce), None);
    let mid = layer(&["a"], None, None);

    let (effective, _) = merged(&[root.clone(), mid.clone(), layer(&["a2"], None, None)]);
    assert!(effective.unwrap().auth.is_some());

    let (refused, withheld) = merged(&[root.clone(), mid, layer(&["b"], None, None)]);
    assert!(
        matches!(&refused, Err(DomainError::Forbidden { reason, .. })
            if reason == super::merge::ENFORCED_AUTH_OTHER_SERVER),
        "{refused:?}"
    );
    assert_eq!(withheld.len(), 1);
    assert_eq!(withheld[0].kind, super::merge::WithheldKind::EnforcedAuth);
    assert_eq!(withheld[0].owner_id, root.id);
}

/// The middle tenant's own key belongs to the middle tenant's endpoints,
/// even if the root lists more.
#[test]
fn a_middle_tenants_key_stays_on_its_endpoints() {
    let root = layer(&["a", "a2"], Some(SharingMode::Inherit), None);
    let mid = layer(&["a"], Some(SharingMode::Inherit), None);
    let leaf = layer(&["a2"], None, None);
    let (effective, withheld) = merged(&[root, mid.clone(), leaf]);
    assert!(effective.unwrap().auth.is_none());
    assert_eq!(withheld[0].owner_id, mid.id);
}

/// Headers have their own owner: a child that sets its auth but not its
/// headers still checks the root's headers against the root's endpoints.
#[test]
fn headers_and_auth_have_separate_owners() {
    let root = layer(&["a"], Some(SharingMode::Inherit), Some("root"));
    let child = layer(&["b"], Some(SharingMode::Private), None);
    let (effective, withheld) = merged(&[root, child]);
    let effective = effective.unwrap();
    assert!(effective.auth.is_some(), "the child's own auth");
    assert!(effective.headers.is_none());
    assert_eq!(
        withheld.iter().map(|w| w.kind).collect::<Vec<_>>(),
        vec![super::merge::WithheldKind::Headers]
    );
}

/// An enforced root auth binds a grandchild even when the tenant between them
/// bound the alias without auth of its own: the grandchild cannot move the
/// alias to another server.
#[tokio::test]
async fn an_auth_enforced_above_the_closest_ancestor_still_pins_the_server() {
    use crate::domain::model::{AuthConfig, SharingMode};

    let (root, mid, leaf) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let resolver = MockTenantResolverClient::with_hierarchy(vec![
        TenantId(root),
        TenantId(mid),
        TenantId(leaf),
    ]);
    let svc = make_service_with_resolver(resolver);

    let mut root_req = make_create_upstream_ip("svc");
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Enforce,
        config: None,
    });
    svc.create_upstream(&test_ctx(root), root_req)
        .await
        .unwrap();
    // The middle tenant binds on the root's server, with no auth of its own.
    svc.create_upstream(&test_ctx(mid), make_create_upstream_ip("svc"))
        .await
        .unwrap();

    let mut moved = make_create_upstream_ip("svc");
    moved.server.endpoints[0].host = "10.6.6.6".into();
    let err = svc
        .create_upstream(&test_ctx(leaf), moved)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");

    // On the root's server the bind is fine.
    svc.create_upstream(&test_ctx(leaf), make_create_upstream_ip("svc"))
        .await
        .unwrap();
}

/// The server that counts is the enforcing ancestor's, not the closest one's:
/// a tenant in between that shadows the alias elsewhere with nothing to share
/// is invisible to its descendants and skipped by the merge.
#[tokio::test]
async fn an_enforced_auth_pins_bindings_to_the_enforcing_ancestors_server() {
    use crate::domain::model::{AuthConfig, SharingMode};

    let (root, mid, leaf) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let resolver = MockTenantResolverClient::with_hierarchy(vec![
        TenantId(root),
        TenantId(mid),
        TenantId(leaf),
    ]);
    let svc = make_service_with_resolver(resolver);
    let auth = |sharing| AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing,
        config: None,
    };

    let mut root_req = make_create_upstream_ip("svc");
    root_req.auth = Some(auth(SharingMode::Inherit));
    let root_up = svc
        .create_upstream(&test_ctx(root), root_req)
        .await
        .unwrap();
    // The middle tenant shadows on another server and shares nothing.
    let mut mid_req = make_create_upstream_ip("svc");
    mid_req.server.endpoints[0].host = "10.0.0.2".into();
    svc.create_upstream(&test_ctx(mid), mid_req).await.unwrap();
    // The root enforces its auth afterwards.
    let mut enforce = make_update_from_upstream(&root_up);
    enforce.auth = Some(auth(SharingMode::Enforce));
    svc.update_upstream(&test_ctx(root), root_up.id, enforce)
        .await
        .unwrap();

    let mut on_mid_server = make_create_upstream_ip("svc");
    on_mid_server.server.endpoints[0].host = "10.0.0.2".into();
    let err = svc
        .create_upstream(&test_ctx(leaf), on_mid_server)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
    svc.create_upstream(&test_ctx(leaf), make_create_upstream_ip("svc"))
        .await
        .unwrap();
}

/// Under an enforced auth a binding is checked against the enforcing
/// ancestor's endpoints: a subset is fine, an extra endpoint is not.
#[tokio::test]
async fn an_enforced_auth_allows_bindings_on_part_of_its_endpoints() {
    use crate::domain::model::{AuthConfig, SharingMode};

    let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let svc = make_service_with_resolver(resolver);

    let two = |hosts: &[&str]| {
        let mut req = make_create_upstream_ip("svc");
        req.server.endpoints = hosts
            .iter()
            .map(|h| Endpoint {
                scheme: Scheme::Https,
                host: (*h).into(),
                port: 443,
            })
            .collect();
        req
    };
    let mut root_req = two(&["10.0.0.1", "10.0.0.2"]);
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Enforce,
        config: None,
    });
    svc.create_upstream(&test_ctx(root), root_req)
        .await
        .unwrap();

    let err = svc
        .create_upstream(&test_ctx(child), two(&["10.0.0.2", "10.6.6.6"]))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
    svc.create_upstream(&test_ctx(child), two(&["10.0.0.2"]))
        .await
        .unwrap();
}

/// On the proxy path every withheld credential is counted, and each binding
/// warns at most once a minute; the SDK's resolve reports nothing.
#[tokio::test]
#[tracing_test::traced_test]
async fn withheld_credentials_are_counted_and_warned_once() {
    use crate::domain::model::{AuthConfig, SharingMode};
    use crate::domain::ports::metric_labels::withheld_outcome;
    use crate::domain::test_support::ProvisioningMetrics;

    let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
    let resolver = MockTenantResolverClient::with_hierarchy(vec![TenantId(root), TenantId(child)]);
    let metrics = Arc::new(ProvisioningMetrics::default());
    let svc = make_service_with_resolver(resolver).with_metrics(metrics.clone());

    let mut root_req = make_create_upstream_ip("svc");
    root_req.auth = Some(AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let root_up = svc
        .create_upstream(&test_ctx(root), root_req)
        .await
        .unwrap();
    svc.create_route(&test_ctx(root), make_create_route(root_up.id))
        .await
        .unwrap();
    let mut child_req = make_create_upstream_ip("svc");
    child_req.server.endpoints[0].host = "10.6.6.6".into();
    svc.create_upstream(&test_ctx(child), child_req)
        .await
        .unwrap();

    let (method, path) = ("POST", "/v1/chat/completions");
    for _ in 0..2 {
        let ProxyTarget {
            upstream: effective,
            ..
        } = svc
            .resolve_proxy_target(
                &crate::domain::services::ProxyGrant::for_tests(test_ctx(child)),
                "svc",
                method,
                path,
                Tags::Skip,
            )
            .await
            .unwrap();
        assert!(effective.auth.is_none());
    }
    svc.resolve_proxy_target(
        &crate::domain::services::ProxyGrant::for_tests(test_ctx(child)),
        "svc",
        method,
        path,
        Tags::Load,
    )
    .await
    .map(crate::domain::model::ProxyTarget::into_parts)
    .unwrap();

    // Copy out of the lock before asserting, so no guard lives in the macro.
    let withheld = metrics.withheld.lock().unwrap().clone();
    assert_eq!(
        withheld,
        vec![
            ("svc".to_owned(), withheld_outcome::DROPPED_AUTH),
            ("svc".to_owned(), withheld_outcome::DROPPED_AUTH),
        ]
    );
    logs_assert(|lines: &[&str]| {
        match lines
            .iter()
            .filter(|l| l.contains("ancestor auth dropped"))
            .count()
        {
            1 => Ok(()),
            n => Err(format!("expected one warning, got {n}")),
        }
    });
}

/// A full warning log keeps the quiet windows it has: a binding that warned
/// stays quiet, and a new one waits for a slot.
#[test]
fn a_full_withheld_log_keeps_its_quiet_windows() {
    use super::merge::{Withheld, WithheldKind};
    use super::withheld::WithheldLog;

    let log = WithheldLog::default();
    let withheld = |owner_id| Withheld {
        owner_id,
        upstream_id: Uuid::nil(),
        alias: "svc".into(),
        kind: WithheldKind::Auth,
    };
    let first = withheld(Uuid::new_v4());
    assert!(log.warns(&first));
    for _ in 1..1024 {
        assert!(log.warns(&withheld(Uuid::new_v4())));
    }
    assert!(!log.warns(&withheld(Uuid::new_v4())), "no free slot");
    assert!(!log.warns(&first), "still quiet");
}

/// A disabled ancestor is not in the proxy merge, so its enforced auth does
/// not decide where a binding may go.
#[tokio::test]
async fn a_disabled_ancestors_enforced_auth_does_not_pin_bindings() {
    use crate::domain::model::{AuthConfig, SharingMode};

    let (root, mid, leaf) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let resolver = MockTenantResolverClient::with_hierarchy(vec![
        TenantId(root),
        TenantId(mid),
        TenantId(leaf),
    ]);
    let svc = make_service_with_resolver(resolver);
    let auth = |sharing| AuthConfig {
        plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
        sharing,
        config: None,
    };
    let at = |host: &str| {
        let mut req = make_create_upstream_ip("svc");
        req.server.endpoints[0].host = host.into();
        req
    };

    let mut root_req = at("10.0.0.1");
    root_req.auth = Some(auth(SharingMode::Inherit));
    let root_up = svc
        .create_upstream(&test_ctx(root), root_req)
        .await
        .unwrap();
    let mut mid_req = at("10.0.0.2");
    mid_req.auth = Some(auth(SharingMode::Enforce));
    svc.create_upstream(&test_ctx(mid), mid_req).await.unwrap();
    // The root enforces its auth afterwards, and is disabled.
    let mut update = make_update_from_upstream(&root_up);
    update.auth = Some(auth(SharingMode::Enforce));
    update.enabled = false;
    svc.update_upstream(&test_ctx(root), root_up.id, update)
        .await
        .unwrap();

    let err = svc
        .create_upstream(&test_ctx(leaf), at("10.0.0.1"))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
    svc.create_upstream(&test_ctx(leaf), at("10.0.0.2"))
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// Management permissions (#4972)
// ---------------------------------------------------------------------------

mod management_permissions {
    use super::*;
    use crate::domain::error::DeniedOn;
    use crate::domain::test_support::{CapturingAuthZResolverClient, DenyingAuthZResolverClient};
    use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
    use oagw_sdk::{ROUTE_SCHEMA, UPSTREAM_SCHEMA};

    /// Two services over the same rows: one allowed, one with `authz`.
    fn services(
        authz: Arc<dyn AuthZResolverApi>,
    ) -> (ControlPlaneServiceImpl, ControlPlaneServiceImpl) {
        let (allowed, other, _) = services_with_rows(authz);
        (allowed, other)
    }

    /// [`services`], with the registry rows of the same store.
    fn services_with_rows(
        authz: Arc<dyn AuthZResolverApi>,
    ) -> (
        ControlPlaneServiceImpl,
        ControlPlaneServiceImpl,
        Arc<dyn crate::domain::repo::RegistryRowRepository>,
    ) {
        let stores = Stores::new(Backend::Memory);
        let build = |enforcer| {
            ControlPlaneServiceImpl::new(
                stores.upstreams.clone(),
                stores.routes.clone(),
                Arc::new(MockTenantResolverClient::single_tenant()),
                enforcer,
                Arc::new(MockCredStoreClient::empty()),
                Arc::new(SsrfGuard::disabled()),
                None,
            )
        };
        (
            build(allow_all_enforcer()),
            build(PolicyEnforcer::new(authz)),
            stores.rows.clone(),
        )
    }

    /// Answers by action: `denied` actions are refused, every other one is
    /// granted under `constraints`.
    struct ScriptedAuthZ {
        denied: Vec<&'static str>,
        constraints: Vec<authz_resolver_sdk::Constraint>,
    }

    #[async_trait::async_trait]
    impl AuthZResolverApi for ScriptedAuthZ {
        async fn evaluate(
            &self,
            _ctx: toolkit_security::PlatformSecurityContext,
            request: authz_resolver_sdk::EvaluationRequest,
        ) -> Result<authz_resolver_sdk::EvaluationResponse, toolkit_canonical_errors::CanonicalError>
        {
            let decision = !self.denied.contains(&request.action.name.as_str());
            Ok(authz_resolver_sdk::EvaluationResponse {
                decision,
                context: authz_resolver_sdk::EvaluationResponseContext {
                    constraints: if decision {
                        self.constraints.clone()
                    } else {
                        Vec::new()
                    },
                    deny_reason: None,
                },
            })
        }
    }

    fn checks(client: &CapturingAuthZResolverClient) -> Vec<(String, String, Option<Uuid>)> {
        client
            .recorded()
            .into_iter()
            .map(|r| (r.action.name, r.resource.resource_type, r.resource.id))
            .collect()
    }

    #[tokio::test]
    async fn each_management_call_asks_for_its_permission() {
        let capturing = Arc::new(CapturingAuthZResolverClient::new());
        let (_, svc) = services(capturing.clone());
        let ctx = test_ctx(Uuid::new_v4());

        let up = svc
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        svc.get_upstream(&ctx, up.id).await.unwrap();
        svc.list_upstreams(&ctx, &ListQuery::default())
            .await
            .unwrap();
        svc.update_upstream(&ctx, up.id, make_update_from_upstream(&up))
            .await
            .unwrap();
        let route = svc
            .create_route(&ctx, make_create_route(up.id))
            .await
            .unwrap();
        svc.get_route(&ctx, route.id).await.unwrap();
        svc.list_routes(&ctx, None, &ListQuery::default())
            .await
            .unwrap();
        svc.update_route(&ctx, route.id, make_update_from_route(&route))
            .await
            .unwrap();
        svc.delete_route(&ctx, route.id).await.unwrap();
        svc.delete_upstream(&ctx, up.id).await.unwrap();

        let u = || UPSTREAM_SCHEMA.to_owned();
        let r = || ROUTE_SCHEMA.to_owned();
        assert_eq!(
            checks(&capturing),
            vec![
                ("create".into(), u(), None),
                ("read".into(), u(), Some(up.id)),
                ("read".into(), u(), None),
                ("override".into(), u(), Some(up.id)),
                ("create".into(), r(), None),
                ("read".into(), r(), Some(route.id)),
                ("read".into(), r(), None),
                ("override".into(), r(), Some(route.id)),
                ("delete".into(), r(), Some(route.id)),
                ("delete".into(), u(), Some(up.id)),
            ]
        );
        // The caller's tenant is the owner and the context tenant, alone
        // (no subtree expansion).
        for request in capturing.recorded() {
            assert_eq!(
                request
                    .context
                    .tenant_context
                    .as_ref()
                    .map(|t| t.mode.clone()),
                Some(authz_resolver_sdk::TenantMode::RootOnly),
            );
            assert_eq!(
                request.resource.properties.get("owner_tenant_id"),
                Some(&serde_json::json!(ctx.subject_tenant_id())),
            );
            assert!(!request.context.require_constraints);
            assert_eq!(
                request
                    .context
                    .tenant_context
                    .as_ref()
                    .and_then(|t| t.root_id),
                Some(ctx.subject_tenant_id()),
            );
        }
    }

    fn assert_forbidden_on(result: Result<impl std::fmt::Debug, DomainError>, expected: DeniedOn) {
        match result {
            Err(DomainError::Forbidden { entity, .. }) => assert_eq!(entity, expected),
            other => panic!("expected Forbidden on {expected:?}, got {other:?}"),
        }
    }

    #[tokio::test]
    #[tracing_test::traced_test]
    async fn denied_management_calls_are_forbidden_and_change_nothing() {
        let (allowed, denied) = services(Arc::new(DenyingAuthZResolverClient));
        let ctx = test_ctx(Uuid::new_v4());
        let up = allowed
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        let route = allowed
            .create_route(&ctx, make_create_route(up.id))
            .await
            .unwrap();

        assert_forbidden_on(
            denied
                .create_upstream(&ctx, make_create_upstream_ip("other"))
                .await,
            DeniedOn::Upstream,
        );
        assert_forbidden_on(denied.get_upstream(&ctx, up.id).await, DeniedOn::Upstream);
        assert_forbidden_on(
            denied.list_upstreams(&ctx, &ListQuery::default()).await,
            DeniedOn::Upstream,
        );
        let mut disable = make_update_from_upstream(&up);
        disable.enabled = false;
        assert_forbidden_on(
            denied.update_upstream(&ctx, up.id, disable).await,
            DeniedOn::Upstream,
        );
        assert_forbidden_on(
            denied.delete_upstream(&ctx, up.id).await,
            DeniedOn::Upstream,
        );

        let mut other_route = make_create_route(up.id);
        if let Some(http) = other_route.match_rules.http.as_mut() {
            http.path = "/v2".into();
        }
        assert_forbidden_on(
            denied.create_route(&ctx, other_route).await,
            DeniedOn::Route,
        );
        assert_forbidden_on(denied.get_route(&ctx, route.id).await, DeniedOn::Route);
        assert_forbidden_on(
            denied.list_routes(&ctx, None, &ListQuery::default()).await,
            DeniedOn::Route,
        );
        let mut disable = make_update_from_route(&route);
        disable.enabled = false;
        assert_forbidden_on(
            denied.update_route(&ctx, route.id, disable).await,
            DeniedOn::Route,
        );
        assert_forbidden_on(denied.delete_route(&ctx, route.id).await, DeniedOn::Route);

        assert_eq!(
            allowed
                .list_upstreams(&ctx, &ListQuery::default())
                .await
                .unwrap(),
            vec![up.clone()]
        );
        assert_eq!(
            allowed
                .list_routes(&ctx, None, &ListQuery::default())
                .await
                .unwrap(),
            vec![route]
        );
        // Each denial is logged with the action and resource type.
        assert!(logs_contain("OAGW management call denied"));
        assert!(logs_contain(oagw_sdk::UPSTREAM_SCHEMA));
        assert!(logs_contain(oagw_sdk::ROUTE_SCHEMA));
        assert!(logs_contain("override"));
    }

    /// Registry writes drop the buckets they make stale too: a changed
    /// limit on update, and an upstream deleted as unused.
    #[tokio::test]
    async fn registry_writes_drop_the_buckets_they_make_stale() {
        use crate::domain::model::{
            RateLimitAlgorithm, RateLimitConfig, RateLimitScope, RateLimitStrategy, SharingMode,
            SustainedRate, Window,
        };
        use crate::domain::rate_limit::RateLimiter;
        use crate::domain::repo::DeleteOutcome;
        use crate::domain::services::RegistryProvisioner;
        let limit = |rate| RateLimitConfig {
            sharing: SharingMode::Private,
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate {
                rate,
                window: Window::Minute,
            },
            burst: None,
            scope: RateLimitScope::Tenant,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
            budget: None,
            response_headers: true,
            shared_pool: None,
            limit_owner_id: None,
        };
        let limiter = Arc::new(RateLimiter::new());
        let (_, svc, rows) = services_with_rows(Arc::new(CapturingAuthZResolverClient::new()));
        let provisioner =
            RegistryProvisionerImpl::new(Arc::new(svc.with_rate_limiter(limiter.clone())), rows);
        let ctx = test_ctx(Uuid::new_v4());
        let mut create = make_create_upstream_hostname();
        create.rate_limit = Some(limit(10));
        let up = provisioner
            .create_registry_upstream(&ctx, create)
            .await
            .unwrap();
        let mut route_req = make_create_route(up.id);
        route_req.rate_limit = Some(limit(5));
        let route = provisioner
            .create_registry_route(&ctx, route_req, UpstreamCheck::Read)
            .await
            .unwrap();
        let consume = |key: String, rate| limiter.try_consume(&key, &limit(rate), "/").unwrap();
        consume(format!("oagw:ratelimit:upstream:{}:t", up.id), 10);
        consume(format!("oagw:ratelimit:route:{}:t", route.id), 5);

        let mut update = make_update_from_route(&route);
        update.rate_limit = Some(limit(6));
        provisioner
            .update_registry_route(&ctx, route.id, up.id, update, UpstreamCheck::Read)
            .await
            .unwrap();
        assert_eq!(limiter.bucket_count(), 1);
        let mut update = make_update_from_upstream(&up);
        update.rate_limit = Some(limit(11));
        provisioner
            .update_registry_upstream(&ctx, up.id, update)
            .await
            .unwrap();
        assert_eq!(limiter.bucket_count(), 0);

        consume(format!("oagw:ratelimit:upstream:{}:t", up.id), 11);
        // Still in use: nothing is deleted and the bucket stays.
        assert_eq!(
            provisioner
                .delete_registry_upstream_if_unused(&ctx, up.id)
                .await
                .unwrap(),
            DeleteOutcome::InUse
        );
        assert_eq!(limiter.bucket_count(), 1);
        provisioner
            .delete_registry_route(&ctx, route.id)
            .await
            .unwrap();
        assert_eq!(
            provisioner
                .delete_registry_upstream_if_unused(&ctx, up.id)
                .await
                .unwrap(),
            DeleteOutcome::Deleted
        );
        assert_eq!(limiter.bucket_count(), 0);
    }

    /// Subquery grants (tenant subtree, group membership) need a database to
    /// decide; the management check does not take them as a tenant grant.
    #[tokio::test]
    async fn subquery_grants_fail_closed() {
        use authz_resolver_sdk::{Constraint, InTenantSubtreePredicate, Predicate};
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);
        let (_, svc) = services(Arc::new(ScriptedAuthZ {
            denied: Vec::new(),
            constraints: vec![Constraint {
                predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate::new(
                    "owner_tenant_id",
                    tenant,
                ))],
            }],
        }));
        // Without a declared capability the enforcer cannot compile the
        // subquery: the call fails closed.
        assert!(matches!(
            svc.list_upstreams(&ctx, &ListQuery::default()).await,
            Err(DomainError::Internal { .. })
        ));
    }

    /// An ancestor bind refused by the policy is a 403 on the upstream, on
    /// the API and the registry path alike.
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn refused_ancestor_bind_is_forbidden_on_the_upstream() {
        use crate::domain::model::{AuthConfig, SharingMode};
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let stores = Stores::new(Backend::Memory);
        let build = |enforcer| {
            ControlPlaneServiceImpl::new(
                stores.upstreams.clone(),
                stores.routes.clone(),
                Arc::new(MockTenantResolverClient::with_hierarchy(vec![
                    TenantId(root),
                    TenantId(child),
                ])),
                enforcer,
                Arc::new(MockCredStoreClient::empty()),
                Arc::new(SsrfGuard::disabled()),
                None,
            )
        };
        let allowed = build(allow_all_enforcer());
        let mut shared = make_create_upstream_hostname();
        shared.auth = Some(AuthConfig {
            plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        allowed
            .create_upstream(&test_ctx(root), shared)
            .await
            .unwrap();

        let no_bind = build(PolicyEnforcer::new(Arc::new(ScriptedAuthZ {
            denied: vec!["bind"],
            constraints: Vec::new(),
        })));
        assert_forbidden_on(
            no_bind
                .create_upstream(&test_ctx(child), make_create_upstream_hostname())
                .await,
            DeniedOn::Upstream,
        );
        assert_forbidden_on(
            no_bind
                .create_upstream_as(
                    &test_ctx(child),
                    make_create_upstream_hostname(),
                    ManagedBy::Registry,
                )
                .await,
            DeniedOn::Upstream,
        );
        assert!(logs_contain("OAGW ancestor bind denied"));
        assert!(logs_contain("action=\"bind\""));
    }

    /// The bind checks take the policy's decision without applying the
    /// grant's scope: a bind granted only on some upstream `id` fails closed
    /// instead of allowing a bind to any ancestor upstream.
    #[tokio::test]
    async fn row_level_bind_grant_fails_closed() {
        use authz_resolver_sdk::{Constraint, EqPredicate, Predicate};

        /// Grants every action; `bind` only on one (other) upstream.
        struct BindOnOneUpstream(Uuid);

        #[async_trait::async_trait]
        impl AuthZResolverApi for BindOnOneUpstream {
            async fn evaluate(
                &self,
                _ctx: toolkit_security::PlatformSecurityContext,
                request: authz_resolver_sdk::EvaluationRequest,
            ) -> Result<
                authz_resolver_sdk::EvaluationResponse,
                toolkit_canonical_errors::CanonicalError,
            > {
                let constraints = if request.action.name == "bind" {
                    vec![Constraint {
                        predicates: vec![Predicate::Eq(EqPredicate::new("id", self.0))],
                    }]
                } else {
                    Vec::new()
                };
                Ok(authz_resolver_sdk::EvaluationResponse {
                    decision: true,
                    context: authz_resolver_sdk::EvaluationResponseContext {
                        constraints,
                        deny_reason: None,
                    },
                })
            }
        }

        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let stores = Stores::new(Backend::Memory);
        let build = |enforcer| {
            ControlPlaneServiceImpl::new(
                stores.upstreams.clone(),
                stores.routes.clone(),
                Arc::new(MockTenantResolverClient::with_hierarchy(vec![
                    TenantId(root),
                    TenantId(child),
                ])),
                enforcer,
                Arc::new(MockCredStoreClient::empty()),
                Arc::new(SsrfGuard::disabled()),
                None,
            )
        };
        build(allow_all_enforcer())
            .create_upstream(&test_ctx(root), make_create_upstream_hostname())
            .await
            .unwrap();

        let scoped = build(PolicyEnforcer::new(Arc::new(BindOnOneUpstream(
            Uuid::new_v4(),
        ))));
        assert!(matches!(
            scoped
                .create_upstream(&test_ctx(child), make_create_upstream_hostname())
                .await,
            Err(DomainError::Internal { .. })
        ));
    }

    /// A prefix no request path could match is refused on write.
    #[tokio::test]
    async fn unreachable_route_prefix_is_rejected() {
        let svc = make_service();
        let ctx = test_ctx(Uuid::new_v4());
        let up = svc
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        for path in [
            "/v1/a%",
            "/v1/x;y",
            "/v1/%2e%2e/admin",
            "/v1/%ff",
            "/v1//chat",
            "/v1/./chat",
            "/v1/../chat",
            "items",
            "",
            "//",
            "/v1//",
        ] {
            let mut req = make_create_route(up.id);
            if let Some(http) = req.match_rules.http.as_mut() {
                http.path = path.into();
            }
            let err = svc.create_route(&ctx, req).await.unwrap_err();
            assert!(
                matches!(
                    &err,
                    DomainError::Validation {
                        field: "match.http.path",
                        ..
                    }
                ),
                "{path}: {err:?}"
            );
        }
        // A trailing `/`, the root and an encoded `/` stay valid.
        for path in ["/v2/", "/", "/v3/a%2Fb"] {
            let mut req = make_create_route(up.id);
            if let Some(http) = req.match_rules.http.as_mut() {
                http.path = path.into();
            }
            svc.create_route(&ctx, req)
                .await
                .unwrap_or_else(|e| panic!("{path}: {e:?}"));
        }
    }

    /// A tenant-resolver refusal on a management write is reported on the
    /// upstream, not on the proxy.
    #[tokio::test]
    async fn tenant_resolver_refusal_is_forbidden_on_the_upstream() {
        let svc = ControlPlaneServiceImpl::new(
            Arc::new(InMemoryUpstreamRepo::new()),
            Arc::new(InMemoryRouteRepo::new()),
            Arc::new(crate::domain::test_support::UnauthorizedTenantResolver),
            allow_all_enforcer(),
            Arc::new(MockCredStoreClient::empty()),
            Arc::new(SsrfGuard::disabled()),
            None,
        );
        match svc
            .create_upstream(&test_ctx(Uuid::new_v4()), make_create_upstream_hostname())
            .await
        {
            Err(DomainError::Forbidden { reason, entity, .. }) => {
                assert_eq!(
                    reason,
                    oagw_sdk::reason::permission::TENANT_RESOLVER_UNAUTHORIZED
                );
                assert_eq!(entity, DeniedOn::Upstream);
            }
            other => panic!("expected Forbidden on the upstream, got {other:?}"),
        }
    }

    /// A prefix is stored in the form requests are matched in, so two
    /// spellings of one prefix conflict.
    #[tokio::test]
    async fn route_prefix_is_stored_in_normal_form() {
        let svc = make_service();
        let ctx = test_ctx(Uuid::new_v4());
        let up = svc
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        let with_path = |path: &str| {
            let mut req = make_create_route(up.id);
            if let Some(http) = req.match_rules.http.as_mut() {
                http.path = path.into();
            }
            req
        };
        let route = svc
            .create_route(&ctx, with_path("/v1/%7euser/a%2fb"))
            .await
            .unwrap();
        assert_eq!(
            route.match_rules.http.as_ref().map(|h| h.path.as_str()),
            Some("/v1/~user/a%2Fb")
        );
        let err = svc
            .create_route(&ctx, with_path("/v1/~user/a%2Fb"))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Conflict { .. }), "{err:?}");
    }

    /// The SDK's `resolve_proxy_target` returns upstream and route
    /// configuration, so it needs `read` on both.
    #[tokio::test]
    async fn client_resolution_needs_read() {
        let capturing = Arc::new(CapturingAuthZResolverClient::new());
        let (allowed, checked) = services(capturing.clone());
        let ctx = test_ctx(Uuid::new_v4());
        let up = allowed
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        allowed
            .create_route(&ctx, make_create_route(up.id))
            .await
            .unwrap();

        let (resolved_up, resolved_route) = checked
            .resolve_proxy_target_for_client(&ctx, &up.alias, "POST", "/v1/chat/completions")
            .await
            .unwrap();
        // The path is matched in the form the proxy matches it in.
        let capturing_calls = checks(&capturing).len();
        checked
            .resolve_proxy_target_for_client(&ctx, &up.alias, "POST", "/v1/x/../chat/c%6fmpletions")
            .await
            .unwrap();
        assert!(matches!(
            checked
                .resolve_proxy_target_for_client(
                    &ctx,
                    &up.alias,
                    "POST",
                    "/v1/%2e%2e/chat/completions"
                )
                .await,
            Err(DomainError::Validation { .. })
        ));
        assert_eq!(capturing_calls, 3);
        // `read` on the caller's tenant first, then on the rows returned
        // (single-tenant chain), under their owners.
        assert_eq!(
            checks(&capturing)[..3].to_vec(),
            vec![
                ("read".into(), UPSTREAM_SCHEMA.to_owned(), None),
                (
                    "read".into(),
                    UPSTREAM_SCHEMA.to_owned(),
                    Some(resolved_up.id)
                ),
                (
                    "read".into(),
                    ROUTE_SCHEMA.to_owned(),
                    Some(resolved_route.id)
                ),
            ]
        );

        let (allowed, denied) = services(Arc::new(ScriptedAuthZ {
            denied: vec!["read"],
            constraints: Vec::new(),
        }));
        let up = allowed
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        allowed
            .create_route(&ctx, make_create_route(up.id))
            .await
            .unwrap();
        assert_forbidden_on(
            denied
                .resolve_proxy_target_for_client(&ctx, &up.alias, "POST", "/v1/chat/completions")
                .await,
            DeniedOn::Upstream,
        );
    }

    /// A client resolving through a hierarchy is checked against every
    /// upstream whose configuration is merged in: a grant on its own tenant
    /// only does not cover an inherited target, but covers its own upstream
    /// when no ancestor shares with it.
    #[tokio::test]
    async fn client_resolution_checks_the_owner_of_an_inherited_target() {
        use crate::domain::model::{AuthConfig, SharingMode};
        use authz_resolver_sdk::{Constraint, InPredicate, Predicate};
        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let stores = Stores::new(Backend::Memory);
        let build = |enforcer| {
            ControlPlaneServiceImpl::new(
                stores.upstreams.clone(),
                stores.routes.clone(),
                Arc::new(MockTenantResolverClient::with_hierarchy(vec![
                    TenantId(root),
                    TenantId(child),
                ])),
                enforcer,
                Arc::new(MockCredStoreClient::empty()),
                Arc::new(SsrfGuard::disabled()),
                None,
            )
        };
        let allowed = build(allow_all_enforcer());
        let mut shared = make_create_upstream_hostname();
        shared.auth = Some(AuthConfig {
            plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        let up = allowed
            .create_upstream(&test_ctx(root), shared)
            .await
            .unwrap();
        allowed
            .create_route(&test_ctx(root), make_create_route(up.id))
            .await
            .unwrap();

        // The child may read its own tenant's rows only.
        let own_tenant_only = build(PolicyEnforcer::new(Arc::new(ScriptedAuthZ {
            denied: Vec::new(),
            constraints: vec![Constraint {
                predicates: vec![Predicate::In(InPredicate::new("owner_tenant_id", [child]))],
            }],
        })));
        assert_forbidden_on(
            own_tenant_only
                .resolve_proxy_target_for_client(
                    &test_ctx(child),
                    &up.alias,
                    "POST",
                    "/v1/chat/completions",
                )
                .await,
            DeniedOn::Upstream,
        );
        // The child's own upstream is its own to read, also when the root
        // has an upstream on the alias that shares nothing with it.
        for (alias, owners) in [
            ("quiet-shadowed", &[root, child][..]),
            ("quiet-own", &[child]),
        ] {
            let mut own = None;
            for &owner in owners {
                let up = allowed
                    .create_upstream(&test_ctx(owner), make_create_upstream_ip(alias))
                    .await
                    .unwrap();
                allowed
                    .create_route(&test_ctx(owner), make_create_route(up.id))
                    .await
                    .unwrap();
                own = Some(up);
            }
            let own = own.unwrap();
            let (resolved, route) = own_tenant_only
                .resolve_proxy_target_for_client(
                    &test_ctx(child),
                    alias,
                    "POST",
                    "/v1/chat/completions",
                )
                .await
                .unwrap();
            assert_eq!(
                (resolved.id, route.upstream_id),
                (own.id, own.id),
                "{alias}"
            );
        }

        // An unknown alias or path is refused the same way: a 404 would tell
        // the child which aliases and routes the root shares.
        for (alias, path) in [
            ("no-such-alias", "/v1/chat/completions"),
            (up.alias.as_str(), "/no/such/route"),
        ] {
            assert_forbidden_on(
                own_tenant_only
                    .resolve_proxy_target_for_client(&test_ctx(child), alias, "POST", path)
                    .await,
                DeniedOn::Upstream,
            );
        }

        // A grant on the whole chain resolves the inherited target.
        let whole_chain = build(PolicyEnforcer::new(Arc::new(ScriptedAuthZ {
            denied: Vec::new(),
            constraints: vec![Constraint {
                predicates: vec![Predicate::In(InPredicate::new(
                    "owner_tenant_id",
                    [root, child],
                ))],
            }],
        })));
        whole_chain
            .resolve_proxy_target_for_client(
                &test_ctx(child),
                &up.alias,
                "POST",
                "/v1/chat/completions",
            )
            .await
            .unwrap();
        assert!(matches!(
            whole_chain
                .resolve_proxy_target_for_client(
                    &test_ctx(child),
                    "no-such-alias",
                    "POST",
                    "/v1/chat/completions",
                )
                .await,
            Err(DomainError::NotFound { .. })
        ));

        // The child's own upstream on the alias merges the root's inherited
        // auth: still refused without `read` on the root.
        let mut own = make_create_upstream_hostname();
        own.auth = None;
        let child_up = allowed
            .create_upstream(&test_ctx(child), own)
            .await
            .unwrap();
        allowed
            .create_route(&test_ctx(child), make_create_route(child_up.id))
            .await
            .unwrap();
        assert_forbidden_on(
            own_tenant_only
                .resolve_proxy_target_for_client(
                    &test_ctx(child),
                    &up.alias,
                    "POST",
                    "/v1/chat/completions",
                )
                .await,
            DeniedOn::Upstream,
        );
    }

    /// Denies `read` on the listed resources only.
    struct DenyReadOn(Vec<Uuid>);

    #[async_trait::async_trait]
    impl AuthZResolverApi for DenyReadOn {
        async fn evaluate(
            &self,
            _ctx: toolkit_security::PlatformSecurityContext,
            request: authz_resolver_sdk::EvaluationRequest,
        ) -> Result<authz_resolver_sdk::EvaluationResponse, toolkit_canonical_errors::CanonicalError>
        {
            let denied = request.action.name == "read"
                && request.resource.id.is_some_and(|id| self.0.contains(&id));
            Ok(authz_resolver_sdk::EvaluationResponse {
                decision: !denied,
                context: authz_resolver_sdk::EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: None,
                },
            })
        }
    }

    /// The answer carries the enforced plugins of the parent route the
    /// child's route shadows: a client without `read` on that route is
    /// refused as for any route it may not read, with nothing in the error
    /// naming the route. A parent route that adds nothing to the answer is
    /// not asked about, nor is one that enforces only a rate limit (that
    /// limit is not in the answer), and the proxy's resolution is unchanged.
    #[tokio::test]
    async fn client_resolution_checks_read_on_shadowed_enforcing_routes() {
        use crate::domain::model::{AuthConfig, PluginBinding, PluginsConfig, SharingMode};
        use std::collections::HashMap;

        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let stores = Stores::new(Backend::Memory);
        let build = |enforcer| {
            ControlPlaneServiceImpl::new(
                stores.upstreams.clone(),
                stores.routes.clone(),
                Arc::new(MockTenantResolverClient::with_hierarchy(vec![
                    TenantId(root),
                    TenantId(child),
                ])),
                enforcer,
                Arc::new(MockCredStoreClient::empty()),
                Arc::new(SsrfGuard::disabled()),
                None,
            )
        };
        let guard = |sharing| PluginsConfig {
            sharing,
            items: vec![PluginBinding {
                plugin_ref: crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID.into(),
                config: HashMap::from([("by".to_owned(), "root-route".to_owned())]),
            }],
        };
        let on_path = |upstream_id, path: &str, plugins| {
            let mut req = make_create_route(upstream_id);
            if let Some(http) = req.match_rules.http.as_mut() {
                http.path = path.into();
            }
            req.plugins = plugins;
            req
        };
        let allowed = build(allow_all_enforcer());
        let (root_ctx, child_ctx) = (test_ctx(root), test_ctx(child));
        let mut shared = make_create_upstream_hostname();
        shared.auth = Some(AuthConfig {
            plugin_type: crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Inherit,
            config: None,
        });
        let root_up = allowed.create_upstream(&root_ctx, shared).await.unwrap();
        let mut root_routes = Vec::new();
        for (path, sharing) in [
            ("/v1/chat/completions", SharingMode::Enforce),
            ("/v1/embeddings", SharingMode::Inherit),
        ] {
            let route = on_path(root_up.id, path, Some(guard(sharing)));
            root_routes.push(allowed.create_route(&root_ctx, route).await.unwrap().id);
        }
        let mut limit_only = on_path(root_up.id, "/v1/models", None);
        limit_only.rate_limit = Some(make_rate_limit(SharingMode::Enforce, 10, Window::Minute));
        root_routes.push(
            allowed
                .create_route(&root_ctx, limit_only)
                .await
                .unwrap()
                .id,
        );
        let child_up = allowed
            .create_upstream(&child_ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        for path in ["/v1/chat/completions", "/v1/embeddings", "/v1/models"] {
            allowed
                .create_route(&child_ctx, on_path(child_up.id, path, None))
                .await
                .unwrap();
        }
        let has_guard = |upstream: &Upstream| {
            upstream.plugins.as_ref().is_some_and(|p| {
                p.items
                    .iter()
                    .any(|b| b.config.get("by").is_some_and(|by| by == "root-route"))
            })
        };

        // Granted: the answer carries the parent route's guard.
        let (effective, route) = allowed
            .resolve_proxy_target_for_client(
                &child_ctx,
                &root_up.alias,
                "POST",
                "/v1/chat/completions",
            )
            .await
            .unwrap();
        assert_eq!(route.upstream_id, child_up.id);
        assert!(has_guard(&effective), "{:?}", effective.plugins);

        let no_root_routes = build(PolicyEnforcer::new(Arc::new(DenyReadOn(
            root_routes.clone(),
        ))));
        let denied = no_root_routes
            .resolve_proxy_target_for_client(
                &child_ctx,
                &root_up.alias,
                "POST",
                "/v1/chat/completions",
            )
            .await;
        match &denied {
            Err(DomainError::Forbidden { detail, .. }) => {
                assert!(!detail.contains(&root_routes[0].to_string()), "{detail}");
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }
        assert_forbidden_on(denied, DeniedOn::Route);

        // The parent's `inherit` route adds nothing to the answer.
        let (effective, _) = no_root_routes
            .resolve_proxy_target_for_client(&child_ctx, &root_up.alias, "POST", "/v1/embeddings")
            .await
            .unwrap();
        assert!(!has_guard(&effective), "{:?}", effective.plugins);

        // Nor does a parent route that enforces only its rate limit: the
        // proxy checks it on that route's bucket, the client gets no part of it.
        let (_, route) = no_root_routes
            .resolve_proxy_target_for_client(&child_ctx, &root_up.alias, "POST", "/v1/models")
            .await
            .unwrap();
        assert_eq!(route.upstream_id, child_up.id);

        // The proxy holds a grant and is not asked for `read`.
        let target = no_root_routes
            .resolve_proxy_target(
                &crate::domain::services::ProxyGrant::for_tests(child_ctx.clone()),
                &root_up.alias,
                "POST",
                "/v1/chat/completions",
                Tags::Skip,
            )
            .await
            .unwrap();
        assert!(has_guard(&target.upstream), "{:?}", target.upstream.plugins);
    }

    /// A shadowed parent route whose enforced `plugin_ref` the root upstream
    /// already enforces adds nothing to the answer: the root's config stays,
    /// and the client needs no `read` on that route.
    #[tokio::test]
    async fn client_resolution_skips_read_on_shadowed_routes_whose_enforce_is_taken() {
        use crate::domain::model::{PluginBinding, PluginsConfig, SharingMode};
        use std::collections::HashMap;

        let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
        let stores = Stores::new(Backend::Memory);
        let build = |enforcer| {
            ControlPlaneServiceImpl::new(
                stores.upstreams.clone(),
                stores.routes.clone(),
                Arc::new(MockTenantResolverClient::with_hierarchy(vec![
                    TenantId(root),
                    TenantId(child),
                ])),
                enforcer,
                Arc::new(MockCredStoreClient::empty()),
                Arc::new(SsrfGuard::disabled()),
                None,
            )
        };
        let guard = |by: &str| PluginsConfig {
            sharing: SharingMode::Enforce,
            items: vec![PluginBinding {
                plugin_ref: crate::domain::gts_helpers::REQUIRED_HEADERS_GUARD_PLUGIN_ID.into(),
                config: HashMap::from([("by".to_owned(), by.to_owned())]),
            }],
        };
        let allowed = build(allow_all_enforcer());
        let (root_ctx, child_ctx) = (test_ctx(root), test_ctx(child));
        let mut root_req = make_create_upstream_hostname();
        root_req.plugins = Some(guard("root-upstream"));
        let root_up = allowed.create_upstream(&root_ctx, root_req).await.unwrap();
        let mut root_route = make_create_route(root_up.id);
        root_route.plugins = Some(guard("root-route"));
        let root_route = allowed.create_route(&root_ctx, root_route).await.unwrap();
        let child_up = allowed
            .create_upstream(&child_ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        allowed
            .create_route(&child_ctx, make_create_route(child_up.id))
            .await
            .unwrap();

        let no_root_route = build(PolicyEnforcer::new(Arc::new(DenyReadOn(vec![
            root_route.id,
        ]))));
        let (effective, route) = no_root_route
            .resolve_proxy_target_for_client(
                &child_ctx,
                &root_up.alias,
                "POST",
                "/v1/chat/completions",
            )
            .await
            .unwrap();
        assert_eq!(route.upstream_id, child_up.id);
        let by: Vec<_> = effective
            .plugins
            .iter()
            .flat_map(|p| &p.items)
            .filter_map(|b| b.config.get("by").map(String::as_str))
            .collect();
        assert_eq!(by, vec!["root-upstream"]);
    }

    /// The `read` check runs before resolution, so a denied caller learns
    /// nothing about which aliases exist or which paths are valid.
    #[tokio::test]
    async fn client_resolution_checks_read_before_resolving() {
        let (_, denied) = services(Arc::new(ScriptedAuthZ {
            denied: vec!["read"],
            constraints: Vec::new(),
        }));
        let ctx = test_ctx(Uuid::new_v4());
        for (alias, path) in [
            ("no-such-alias", "/v1/chat/completions"),
            ("no-such-alias", "/v1/%2e%2e/admin"),
        ] {
            assert_forbidden_on(
                denied
                    .resolve_proxy_target_for_client(&ctx, alias, "POST", path)
                    .await,
                DeniedOn::Upstream,
            );
        }
    }

    /// An update needs `override` only: a policy that denies `read` still
    /// lets the caller replace the resource.
    #[tokio::test]
    async fn update_needs_override_not_read() {
        let (allowed, no_read) = services(Arc::new(ScriptedAuthZ {
            denied: vec!["read"],
            constraints: Vec::new(),
        }));
        let ctx = test_ctx(Uuid::new_v4());
        let up = allowed
            .create_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        let route = allowed
            .create_route(&ctx, make_create_route(up.id))
            .await
            .unwrap();

        assert_forbidden_on(no_read.get_upstream(&ctx, up.id).await, DeniedOn::Upstream);
        no_read
            .update_upstream(&ctx, up.id, make_update_from_upstream(&up))
            .await
            .unwrap();
        no_read
            .update_route(&ctx, route.id, make_update_from_route(&route))
            .await
            .unwrap();
    }

    /// A grant constrained to the caller's tenant (what tr-authz returns) is
    /// accepted; one narrower than the tenant cannot be applied by the
    /// tenant-scoped repositories and is refused.
    #[tokio::test]
    async fn constrained_grants_must_cover_the_tenant() {
        use authz_resolver_sdk::{Constraint, EqPredicate, InPredicate, Predicate};
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);
        let grant = |predicates: Vec<Predicate>| {
            services(Arc::new(ScriptedAuthZ {
                denied: Vec::new(),
                constraints: vec![Constraint { predicates }],
            }))
            .1
        };

        let tenant_and_children = grant(vec![Predicate::In(InPredicate::new(
            "owner_tenant_id",
            [tenant, Uuid::new_v4()],
        ))]);
        tenant_and_children
            .list_upstreams(&ctx, &ListQuery::default())
            .await
            .unwrap();

        let other_tenant = grant(vec![Predicate::Eq(EqPredicate::new(
            "owner_tenant_id",
            Uuid::new_v4(),
        ))]);
        // A grant on another tenant only is a plain denial.
        match other_tenant
            .list_upstreams(&ctx, &ListQuery::default())
            .await
        {
            Err(DomainError::Forbidden { reason, entity, .. }) => {
                assert_eq!(reason, oagw_sdk::reason::permission::AUTHZ_DENIED);
                assert_eq!(entity, DeniedOn::Upstream);
            }
            other => panic!("expected a denial, got {other:?}"),
        }

        // Every predicate of a constraint must admit the tenant.
        let narrowed = grant(vec![
            Predicate::In(InPredicate::new(
                "owner_tenant_id",
                [tenant, Uuid::new_v4()],
            )),
            Predicate::Eq(EqPredicate::new("owner_tenant_id", Uuid::new_v4())),
        ]);
        assert_forbidden_on(
            narrowed.list_upstreams(&ctx, &ListQuery::default()).await,
            DeniedOn::Upstream,
        );

        // Constraints are alternatives: one that covers the tenant is enough.
        let two = services(Arc::new(ScriptedAuthZ {
            denied: Vec::new(),
            constraints: vec![
                Constraint {
                    predicates: vec![Predicate::Eq(EqPredicate::new(
                        "owner_tenant_id",
                        Uuid::new_v4(),
                    ))],
                },
                Constraint {
                    predicates: vec![Predicate::Eq(EqPredicate::new("owner_tenant_id", tenant))],
                },
            ],
        }))
        .1;
        two.list_upstreams(&ctx, &ListQuery::default())
            .await
            .unwrap();

        // A grant on the tenant and a second tenant is still whole-tenant.
        let two_tenants = grant(vec![Predicate::In(InPredicate::new(
            "owner_tenant_id",
            [Uuid::new_v4(), tenant],
        ))]);
        two_tenants
            .list_routes(&ctx, None, &ListQuery::default())
            .await
            .unwrap();

        // A row-level grant (on `id`) compiles and is refused as unsupported,
        // not failed as an internal error.
        let row_level = grant(vec![
            Predicate::Eq(EqPredicate::new("owner_tenant_id", tenant)),
            Predicate::Eq(EqPredicate::new("id", Uuid::new_v4())),
        ]);
        match row_level
            .list_routes(&ctx, None, &ListQuery::default())
            .await
        {
            Err(DomainError::Forbidden { reason, entity, .. }) => {
                assert_eq!(
                    reason,
                    oagw_sdk::reason::permission::AUTHZ_SCOPE_NOT_SUPPORTED
                );
                assert_eq!(entity, DeniedOn::Route);
            }
            other => panic!("expected a scope refusal, got {other:?}"),
        }
    }

    /// The registry provisioner reads through the unchecked helpers: a policy
    /// that denies every management call does not stop the reconcile.
    #[tokio::test]
    async fn registry_provisioner_reads_skip_management_permissions() {
        use crate::domain::services::RegistryProvisioner;
        let capturing = Arc::new(CapturingAuthZResolverClient::denying());
        let (_, denied, rows) = services_with_rows(capturing.clone());
        let ctx = test_ctx(Uuid::new_v4());
        let up = denied
            .create_upstream_as(&ctx, make_create_upstream_hostname(), ManagedBy::Registry)
            .await
            .unwrap();
        let route = denied
            .create_route_as(
                &ctx,
                make_create_route(up.id),
                ManagedBy::Registry,
                UpstreamCheck::Read,
            )
            .await
            .unwrap();
        let provisioner = RegistryProvisionerImpl::new(Arc::new(denied), rows);

        assert_eq!(provisioner.get_upstream(&ctx, up.id).await.unwrap(), up);
        assert_eq!(provisioner.get_route(&ctx, route.id).await.unwrap(), route);
        assert_eq!(
            provisioner
                .list_routes(&ctx, Some(up.id), &ListQuery::default())
                .await
                .unwrap(),
            vec![route]
        );
        assert!(capturing.recorded().is_empty());
    }

    /// The registry reconcile writes as the gear: it asks the policy engine
    /// only for ancestor binds (ADR-0018), so a policy that denies every
    /// management call does not stop provisioning.
    #[tokio::test]
    async fn registry_writes_and_reads_skip_management_permissions() {
        use crate::domain::services::RegistryProvisioner;
        let capturing = Arc::new(CapturingAuthZResolverClient::denying());
        let (_, svc, rows) = services_with_rows(capturing.clone());
        let provisioner = RegistryProvisionerImpl::new(Arc::new(svc), rows);
        let ctx = test_ctx(Uuid::new_v4());

        let up = provisioner
            .create_registry_upstream(&ctx, make_create_upstream_hostname())
            .await
            .unwrap();
        let route = provisioner
            .create_registry_route(&ctx, make_create_route(up.id), UpstreamCheck::Read)
            .await
            .unwrap();
        let mut update = make_update_from_upstream(&up);
        update.enabled = false;
        provisioner
            .update_registry_upstream(&ctx, up.id, update)
            .await
            .unwrap();
        let mut update = make_update_from_route(&route);
        update.priority = 7;
        provisioner
            .update_registry_route(&ctx, route.id, up.id, update, UpstreamCheck::Read)
            .await
            .unwrap();
        provisioner
            .delete_registry_route(&ctx, route.id)
            .await
            .unwrap();
        assert!(capturing.recorded().is_empty());
    }

    /// A write that changes or removes a rate limit drops its buckets, on
    /// every write path; one that keeps the limit keeps them.
    #[tokio::test]
    async fn writes_drop_the_buckets_they_make_stale() {
        use crate::domain::model::{
            RateLimitAlgorithm, RateLimitConfig, RateLimitScope, RateLimitStrategy, SharingMode,
            SustainedRate, Window,
        };
        use crate::domain::rate_limit::RateLimiter;
        let limit = |rate| RateLimitConfig {
            sharing: SharingMode::Private,
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate {
                rate,
                window: Window::Minute,
            },
            burst: None,
            scope: RateLimitScope::Tenant,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
            budget: None,
            response_headers: true,
            shared_pool: None,
            limit_owner_id: None,
        };
        let limiter = Arc::new(RateLimiter::new());
        let svc = make_service().with_rate_limiter(limiter.clone());
        let ctx = test_ctx(Uuid::new_v4());
        let mut create = make_create_upstream_hostname();
        create.rate_limit = Some(limit(10));
        let up = svc.create_upstream(&ctx, create).await.unwrap();
        let mut route_req = make_create_route(up.id);
        route_req.rate_limit = Some(limit(5));
        let route = svc.create_route(&ctx, route_req).await.unwrap();
        let fill = || {
            limiter
                .try_consume(
                    &format!("oagw:ratelimit:upstream:{}:t", up.id),
                    &limit(10),
                    "/",
                )
                .unwrap();
            limiter
                .try_consume(
                    &format!("oagw:ratelimit:route:{}:t", route.id),
                    &limit(5),
                    "/",
                )
                .unwrap();
        };
        fill();

        // Same limits: the buckets stay.
        svc.update_upstream(&ctx, up.id, make_update_from_upstream(&up))
            .await
            .unwrap();
        svc.update_route(&ctx, route.id, make_update_from_route(&route))
            .await
            .unwrap();
        assert_eq!(limiter.bucket_count(), 2);

        // A changed upstream limit drops the upstream's bucket only.
        let mut update = make_update_from_upstream(&up);
        update.rate_limit = Some(limit(20));
        svc.update_upstream(&ctx, up.id, update).await.unwrap();
        assert_eq!(limiter.bucket_count(), 1);

        // A removed route limit drops the route's.
        let mut update = make_update_from_route(&route);
        update.rate_limit = None;
        svc.update_route(&ctx, route.id, update).await.unwrap();
        assert_eq!(limiter.bucket_count(), 0);

        // Deletes drop them too.
        fill();
        svc.delete_route(&ctx, route.id).await.unwrap();
        svc.delete_upstream(&ctx, up.id).await.unwrap();
        assert_eq!(limiter.bucket_count(), 0);
    }
}

#[tokio::test]
async fn empty_scope_denies_each_ancestor_permission() {
    struct EmptyActionPdp {
        action: &'static str,
        live: bool,
    }
    #[async_trait::async_trait]
    impl authz_resolver_sdk::AuthZResolverApi for EmptyActionPdp {
        async fn evaluate(
            &self,
            _: toolkit_security::PlatformSecurityContext,
            request: authz_resolver_sdk::EvaluationRequest,
        ) -> Result<
            authz_resolver_sdk::EvaluationResponse,
            toolkit::api::canonical_prelude::CanonicalError,
        > {
            use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
            let mut constraints = vec![];
            if request.action.name == self.action {
                constraints.push(Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        "owner_tenant_id",
                        Vec::<Uuid>::new(),
                    ))],
                });
                if self.live {
                    constraints.push(Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            "owner_tenant_id",
                            [Uuid::from_u128(1)],
                        ))],
                    });
                }
            }
            Ok(authz_resolver_sdk::EvaluationResponse {
                decision: true,
                context: authz_resolver_sdk::EvaluationResponseContext {
                    constraints,
                    deny_reason: None,
                },
            })
        }
    }
    let auth = AuthConfig {
        plugin_type: "apikey".into(),
        sharing: SharingMode::Inherit,
        config: None,
    };
    let rate = make_rate_limit(SharingMode::Inherit, 10, Window::Second);
    let plugins = PluginsConfig {
        sharing: SharingMode::Inherit,
        items: vec![],
    };
    let ancestor = make_upstream(
        Uuid::from_u128(1),
        "api",
        Some(auth.clone()),
        Some(rate.clone()),
        Some(plugins.clone()),
        vec![],
    );
    let overrides = BindOverrides {
        server: &ancestor.server,
        auth: Some(&auth),
        rate_limit: Some(&rate),
        plugins: Some(&plugins),
        cors: None,
    };
    for action in ["bind", "override_auth", "override_rate", "add_plugins"] {
        for live in [false, true] {
            let pep =
                authz_resolver_sdk::PolicyEnforcer::new(Arc::new(EmptyActionPdp { action, live }));
            let result = super::bind::validate_bind_constraints(
                &test_ctx(Uuid::from_u128(2)),
                &pep,
                std::slice::from_ref(&ancestor),
                &overrides,
            )
            .await;
            assert_eq!(result.is_ok(), live, "action={action}, live OR={live}");
            if !live {
                assert!(matches!(result, Err(DomainError::Forbidden { .. })));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Write-time validation (#4974)
// ---------------------------------------------------------------------------

mod write_validation {
    use super::*;
    use crate::domain::gts_helpers::{
        APIKEY_AUTH_PLUGIN_ID, BASIC_AUTH_PLUGIN_ID, NOOP_AUTH_PLUGIN_ID,
        OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, REQUIRED_HEADERS_GUARD_PLUGIN_ID,
        TIMEOUT_GUARD_PLUGIN_ID,
    };
    use crate::domain::model::{
        AuthConfig, BurstConfig, PluginBinding, PluginsConfig, RateLimitAlgorithm, RateLimitConfig,
        RateLimitScope, RateLimitStrategy, SharingMode, SustainedRate, Window,
    };

    /// A validation error on `field` with `reason`.
    #[track_caller]
    fn assert_violation<T: std::fmt::Debug>(
        result: Result<T, DomainError>,
        field: &str,
        reason: &str,
        case: &str,
    ) {
        match result {
            Err(DomainError::Validation {
                field: f,
                reason: r,
                ..
            }) => {
                assert_eq!((f, r), (field, reason), "{case}");
            }
            other => panic!("{case}: expected a {reason} violation on {field}, got {other:?}"),
        }
    }

    fn limit(rate: u32, capacity: Option<u32>, cost: u32) -> RateLimitConfig {
        RateLimitConfig {
            sharing: SharingMode::Private,
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate {
                rate,
                window: Window::Minute,
            },
            burst: capacity.map(|capacity| BurstConfig { capacity }),
            scope: RateLimitScope::Tenant,
            strategy: RateLimitStrategy::Reject,
            cost,
            budget: None,
            response_headers: true,
            shared_pool: None,
            limit_owner_id: None,
        }
    }

    fn binding(plugin_ref: &str) -> Option<PluginsConfig> {
        Some(PluginsConfig {
            sharing: SharingMode::Private,
            items: vec![PluginBinding {
                plugin_ref: plugin_ref.into(),
                config: Default::default(),
            }],
        })
    }

    async fn upstream(svc: &ControlPlaneServiceImpl, ctx: &SecurityContext) -> Upstream {
        svc.create_upstream(ctx, make_create_upstream_hostname())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn route_paths_and_methods_are_required() {
        let svc = make_service();
        let ctx = test_ctx(Uuid::new_v4());
        let up = upstream(&svc, &ctx).await;
        let stored = svc
            .create_route(&ctx, make_create_route(up.id))
            .await
            .unwrap();
        for (case, path, methods, needle) in [
            (
                "an empty path",
                "",
                vec![HttpMethod::Post],
                "route path must start with `/`",
            ),
            (
                "a path without a leading /",
                "v1/chat",
                vec![HttpMethod::Post],
                "route path must start with `/`",
            ),
            (
                "no methods",
                "/v1/chat",
                vec![],
                "match.http.methods must list at least one method",
            ),
        ] {
            let mut req = make_create_route(up.id);
            let http = req.match_rules.http.as_mut().unwrap();
            http.path = path.into();
            http.methods = methods.clone();
            assert_validation_detail(svc.create_route(&ctx, req).await, needle, case);

            let mut update = make_update_from_route(&stored);
            let http = update.match_rules.http.as_mut().unwrap();
            http.path = path.into();
            http.methods = methods;
            assert_validation_detail(
                svc.update_route(&ctx, stored.id, update).await,
                needle,
                &format!("update: {case}"),
            );
        }
    }

    #[tokio::test]
    async fn endpoint_port_zero_is_rejected() {
        let svc = make_service();
        let ctx = test_ctx(Uuid::new_v4());
        let needle = "endpoint[0] port must be between 1 and 65535";
        let mut req = make_create_upstream_hostname();
        req.server.endpoints[0].port = 0;
        assert_validation_detail(svc.create_upstream(&ctx, req).await, needle, "port 0");

        let stored = upstream(&svc, &ctx).await;
        let mut update = make_update_from_upstream(&stored);
        update.server.endpoints[0].port = 0;
        assert_validation_detail(
            svc.update_upstream(&ctx, stored.id, update).await,
            needle,
            "update: port 0",
        );
    }

    #[tokio::test]
    async fn tags_follow_the_tag_pattern() {
        let svc = make_service();
        let ctx = test_ctx(Uuid::new_v4());
        let needle = "tags[0] must match ^[a-z0-9_-]+$";
        let stored = upstream(&svc, &ctx).await;
        let up = upstream_ip(&svc, &ctx, "tag-routes").await;
        let route = svc
            .create_route(&ctx, make_create_route(up.id))
            .await
            .unwrap();
        for tag in ["UPPER", "with space", "dot.ted", "", "é"] {
            let mut req = make_create_upstream_hostname();
            req.tags = vec![tag.into()];
            assert_validation_detail(svc.create_upstream(&ctx, req).await, needle, tag);

            let mut update = make_update_from_upstream(&stored);
            update.tags = vec![tag.into()];
            assert_validation_detail(
                svc.update_upstream(&ctx, stored.id, update).await,
                needle,
                &format!("upstream update: {tag}"),
            );

            let mut req = make_create_route(up.id);
            req.tags = vec![tag.into()];
            assert_validation_detail(
                svc.create_route(&ctx, req).await,
                needle,
                &format!("route: {tag}"),
            );

            let mut update = make_update_from_route(&route);
            update.tags = vec![tag.into()];
            assert_validation_detail(
                svc.update_route(&ctx, route.id, update).await,
                needle,
                &format!("route update: {tag}"),
            );
        }
        let mut update = make_update_from_upstream(&stored);
        update.tags = vec!["team_a-1".into()];
        svc.update_upstream(&ctx, stored.id, update).await.unwrap();
    }

    /// The edge rule is a create rule: an update cannot change the alias
    /// (`aliases_stored_before_the_edge_rule_keep_resolving` covers the
    /// update of a stored one).
    #[tokio::test]
    async fn alias_does_not_start_or_end_with_punctuation() {
        let svc = make_service();
        let ctx = test_ctx(Uuid::new_v4());
        for alias in ["-svc", "svc-", ":svc", "svc:", ".svc"] {
            assert_validation_detail(
                svc.create_upstream(&ctx, make_create_upstream_ip(alias))
                    .await,
                "alias must not start or end with '.', '-' or ':'",
                alias,
            );
        }
        for alias in ["_svc", "svc_", "my-svc.v1"] {
            svc.create_upstream(&ctx, make_create_upstream_ip(alias))
                .await
                .unwrap_or_else(|e| panic!("{alias}: {e:?}"));
        }
        // Trailing dots are stripped before the check (FQDN form).
        let stored = svc
            .create_upstream(&ctx, make_create_upstream_ip("dotted."))
            .await
            .unwrap();
        assert_eq!(stored.alias, "dotted");
    }

    async fn upstream_ip(
        svc: &ControlPlaneServiceImpl,
        ctx: &SecurityContext,
        alias: &str,
    ) -> Upstream {
        svc.create_upstream(ctx, make_create_upstream_ip(alias))
            .await
            .unwrap()
    }

    /// The edge rule runs on create only: an upstream stored before it,
    /// with such an alias, keeps resolving at proxy time.
    #[tokio::test]
    async fn aliases_stored_before_the_edge_rule_keep_resolving() {
        let upstreams = Arc::new(InMemoryUpstreamRepo::new());
        let svc = make_service_over(upstreams.clone(), Arc::new(InMemoryRouteRepo::new()));
        let tenant = Uuid::new_v4();
        let ctx = test_ctx(tenant);
        upstreams
            .create(make_upstream(tenant, "svc-", None, None, None, vec![]))
            .await
            .unwrap();
        let chain = svc.build_tenant_chain(&ctx).await.unwrap();
        let found = svc
            .resolve_alias(&ctx, &chain, "svc-", None, Tags::Load)
            .await
            .unwrap();
        assert_eq!(found.0.alias, "svc-");

        // An API update that keeps the alias goes through: the alias is not
        // re-checked.
        let stored = found.0;
        let mut update = make_update_from_upstream(&stored);
        update.alias = None;
        let updated = svc.update_upstream(&ctx, stored.id, update).await.unwrap();
        assert_eq!(updated.alias, "svc-");
        // Nor when the client echoes the stored alias back (a PUT of a GET).
        let echoed = make_update_from_upstream(&updated);
        assert_eq!(echoed.alias.as_deref(), Some("svc-"));
        let updated = svc.update_upstream(&ctx, stored.id, echoed).await.unwrap();
        assert_eq!(updated.alias, "svc-");

        // The registry re-applies the create rules on every update, so its
        // instance has to be fixed first (the row is kept meanwhile).
        let mut registry_row = make_upstream(tenant, "reg-", None, None, None, vec![]);
        registry_row.server.endpoints[0].host = "10.0.0.1".into();
        registry_row.managed_by = ManagedBy::Registry;
        let registry_row = upstreams.create(registry_row).await.unwrap();
        // An IP-based legacy row (the usual kind) keeps its alias when its
        // endpoints change, with or without the alias echoed back.
        let mut ip_row = make_upstream(tenant, "ip-", None, None, None, vec![]);
        ip_row.server.endpoints[0].host = "10.0.0.2".into();
        let ip_row = upstreams.create(ip_row).await.unwrap();
        for (case, alias) in [("alias omitted", None), ("alias echoed", Some("ip-"))] {
            let mut update = make_update_from_upstream(&ip_row);
            update.server.endpoints[0].host = "10.0.0.3".into();
            update.alias = alias.map(Into::into);
            let updated = svc
                .update_upstream(&ctx, ip_row.id, update)
                .await
                .unwrap_or_else(|e| panic!("{case}: {e:?}"));
            assert_eq!(updated.alias, "ip-", "{case}");
        }

        // The registry's create rules refuse such an instance, so the
        // reconcile skips it and keeps the row, as for any invalid instance.
        let mut instance = make_create_upstream_ip("reg-");
        instance.alias = Some("reg-".into());
        assert_validation_detail(
            svc.create_upstream_as(&ctx, instance, ManagedBy::Registry)
                .await,
            "must not start or end with",
            "registry create of a legacy alias",
        );
        let mut update = make_update_from_upstream(&registry_row);
        update.alias = Some("reg-".into());
        assert_validation_detail(
            svc.update_upstream_as(&ctx, registry_row.id, update, ManagedBy::Registry)
                .await,
            "must not start or end with",
            "registry update of a legacy alias",
        );
    }

    #[tokio::test]
    async fn zero_rate_limit_values_are_rejected_on_upstreams_and_routes() {
        let svc = make_service();
        let ctx = test_ctx(Uuid::new_v4());
        let up = upstream(&svc, &ctx).await;
        for (case, rl) in [
            ("rate 0", limit(0, None, 1)),
            ("capacity 0", limit(10, Some(0), 1)),
            ("cost 0", limit(10, None, 0)),
        ] {
            let mut req = make_create_upstream_ip("rl-up");
            req.rate_limit = Some(rl.clone());
            assert_validation(svc.create_upstream(&ctx, req).await, case);
            let mut route = make_create_route(up.id);
            route.rate_limit = Some(rl);
            assert_validation(svc.create_route(&ctx, route).await, case);
        }
        let mut route = make_create_route(up.id);
        route.rate_limit = Some(limit(10, Some(20), 4));
        let route = svc.create_route(&ctx, route).await.unwrap();

        // Updates are checked the same way.
        for (case, field, rl) in [
            ("rate 0", "rate_limit.sustained.rate", limit(0, None, 1)),
            (
                "capacity 0",
                "rate_limit.burst.capacity",
                limit(10, Some(0), 1),
            ),
            ("cost 0", "rate_limit.cost", limit(10, None, 0)),
        ] {
            let mut update = make_update_from_upstream(&up);
            update.rate_limit = Some(rl.clone());
            assert_validation_detail(svc.update_upstream(&ctx, up.id, update).await, field, case);
            let mut update = make_update_from_route(&route);
            update.rate_limit = Some(rl);
            assert_validation_detail(svc.update_route(&ctx, route.id, update).await, field, case);
        }
    }

    /// A request costing more than a full bucket could never pass: the
    /// capacity is `burst.capacity`, else `sustained.rate`; a sliding window
    /// ignores `burst`.
    #[tokio::test]
    async fn cost_above_the_bucket_capacity_is_rejected() {
        let svc = make_service();
        let ctx = test_ctx(Uuid::new_v4());
        let up = upstream(&svc, &ctx).await;
        let sliding = |rate, capacity, cost| RateLimitConfig {
            algorithm: RateLimitAlgorithm::SlidingWindow,
            ..limit(rate, capacity, cost)
        };
        for (case, rl, detail) in [
            (
                "over burst",
                limit(10, Some(5), 6),
                "rate_limit.cost (6) must not exceed rate_limit.burst.capacity (5)",
            ),
            (
                "over rate",
                limit(5, None, 6),
                "rate_limit.cost (6) must not exceed rate_limit.sustained.rate (5)",
            ),
            (
                "sliding over rate",
                sliding(5, Some(50), 6),
                "rate_limit.cost (6) must not exceed rate_limit.sustained.rate (5)",
            ),
        ] {
            let mut req = make_create_upstream_ip("rl-cost");
            req.rate_limit = Some(rl.clone());
            assert_validation_detail(svc.create_upstream(&ctx, req).await, detail, case);
            let mut route = make_create_route(up.id);
            route.rate_limit = Some(rl.clone());
            assert_validation_detail(svc.create_route(&ctx, route).await, detail, case);
            let mut update = make_update_from_upstream(&up);
            update.rate_limit = Some(rl);
            assert_validation_detail(svc.update_upstream(&ctx, up.id, update).await, detail, case);
        }

        // A cost equal to the capacity fits.
        for (case, rl) in [
            ("at burst", limit(1, Some(5), 5)),
            ("at rate", limit(5, None, 5)),
            ("sliding at rate", sliding(5, Some(1), 5)),
        ] {
            let mut update = make_update_from_upstream(&up);
            update.rate_limit = Some(rl);
            svc.update_upstream(&ctx, up.id, update)
                .await
                .unwrap_or_else(|e| panic!("{case}: {e:?}"));
        }
    }

    #[tokio::test]
    async fn http_endpoints_need_allow_http_upstream() {
        let ctx = test_ctx(Uuid::new_v4());
        let http = || {
            let mut req = make_create_upstream_hostname();
            req.server.endpoints[0].scheme = Scheme::Http;
            req.server.endpoints[0].port = 80;
            req
        };
        assert_violation(
            make_service().create_upstream(&ctx, http()).await,
            "server.endpoints.scheme",
            oagw_sdk::field::HTTP_UPSTREAM_FORBIDDEN,
            "http without the flag",
        );
        make_service()
            .with_allow_http_upstream(true)
            .create_upstream(&ctx, http())
            .await
            .unwrap();

        // An update cannot switch a stored upstream to plain HTTP either.
        for allow in [false, true] {
            let svc = make_service().with_allow_http_upstream(allow);
            let up = upstream(&svc, &ctx).await;
            let mut update = make_update_from_upstream(&up);
            update.server = http().server;
            let result = svc.update_upstream(&ctx, up.id, update).await;
            if allow {
                result.unwrap();
            } else {
                match result {
                    Err(DomainError::Validation { field, reason, .. }) => {
                        assert_eq!(field, "server.endpoints.scheme");
                        assert_eq!(reason, oagw_sdk::field::HTTP_UPSTREAM_FORBIDDEN);
                    }
                    other => panic!("expected HTTP_UPSTREAM_FORBIDDEN, got {other:?}"),
                }
            }
        }
    }

    #[tokio::test]
    async fn plugin_ids_must_be_implemented_and_of_the_right_kind() {
        let svc = make_service();
        let ctx = test_ctx(Uuid::new_v4());
        let not_implemented = oagw_sdk::field::NOT_IMPLEMENTED;
        for (case, auth_type, reason) in [
            ("a short id", "noop", not_implemented),
            (
                "a catalog-only auth plugin",
                BASIC_AUTH_PLUGIN_ID,
                not_implemented,
            ),
            // Implemented, but not an auth plugin: the wrong kind.
            (
                "a guard as auth",
                REQUIRED_HEADERS_GUARD_PLUGIN_ID,
                oagw_sdk::field::VALIDATION,
            ),
            (
                "a transform as auth",
                crate::domain::gts_helpers::REQUEST_ID_TRANSFORM_PLUGIN_ID,
                oagw_sdk::field::VALIDATION,
            ),
        ] {
            let mut req = make_create_upstream_hostname();
            req.auth = Some(AuthConfig {
                plugin_type: auth_type.into(),
                sharing: SharingMode::Private,
                config: None,
            });
            assert_violation(
                svc.create_upstream(&ctx, req).await,
                "auth.type",
                reason,
                case,
            );
        }
        for (case, plugin_ref) in [
            ("an unknown plugin", "audit-log"),
            ("a catalog-only guard", TIMEOUT_GUARD_PLUGIN_ID),
            (
                "a custom plugin UUID",
                "gts.cf.core.oagw.guard_plugin.v1~5e0a1900-0000-4000-8000-000000000001",
            ),
        ] {
            let mut req = make_create_upstream_hostname();
            req.plugins = binding(plugin_ref);
            assert_violation(
                svc.create_upstream(&ctx, req).await,
                "plugins.items.plugin_ref",
                not_implemented,
                case,
            );
        }
        // An auth plugin bound in the wrong place, implemented or not: a
        // plain validation error.
        for plugin_ref in [
            APIKEY_AUTH_PLUGIN_ID,
            BASIC_AUTH_PLUGIN_ID,
            "gts.cf.core.oagw.auth_plugin.v1~5e0a1900-0000-4000-8000-000000000002",
        ] {
            let mut req = make_create_upstream_hostname();
            req.plugins = binding(plugin_ref);
            assert_violation(
                svc.create_upstream(&ctx, req).await,
                "plugins.items.plugin_ref",
                oagw_sdk::field::VALIDATION,
                plugin_ref,
            );
        }
        let mut req = make_create_upstream_hostname();
        req.auth = Some(AuthConfig {
            plugin_type: NOOP_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Private,
            config: None,
        });
        req.plugins = binding(REQUIRED_HEADERS_GUARD_PLUGIN_ID);
        svc.create_upstream(&ctx, req).await.unwrap();
    }

    /// Every credential key an auth plugin reads is resolved on write, and
    /// the error names the key.
    #[tokio::test]
    async fn every_credential_ref_is_checked_on_write() {
        let svc = make_service_with_resolver_and_creds(
            MockTenantResolverClient::single_tenant(),
            vec![
                ("cred://client-id".into(), "id".into()),
                ("cred://client-secret".into(), "secret".into()),
            ],
        );
        let ctx = test_ctx(Uuid::new_v4());
        let oauth2 = |refs: &[(&str, &str)]| {
            let mut config: std::collections::HashMap<String, String> = [(
                "token_endpoint".to_owned(),
                "https://idp.example.com/token".to_owned(),
            )]
            .into_iter()
            .collect();
            config.extend(refs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
            Some(AuthConfig {
                plugin_type: OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
                sharing: SharingMode::Private,
                config: Some(config),
            })
        };
        let missing_secret = [
            ("client_id_ref", "cred://client-id"),
            ("client_secret_ref", "cred://missing-secret"),
        ];

        let mut req = make_create_upstream_hostname();
        req.auth = oauth2(&missing_secret);
        assert_ref_not_accessible(
            svc.create_upstream(&ctx, req).await,
            "auth.config.client_secret_ref",
        );

        // The basic-auth variant reads the same references.
        let mut req = make_create_upstream_hostname();
        let mut basic = oauth2(&missing_secret).unwrap();
        basic.plugin_type =
            crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID.into();
        req.auth = Some(basic);
        assert_ref_not_accessible(
            svc.create_upstream(&ctx, req).await,
            "auth.config.client_secret_ref",
        );

        // Only the credential keys are resolved: `other_ref` is not.
        let mut req = make_create_upstream_hostname();
        req.auth = oauth2(&[
            ("client_id_ref", "cred://client-id"),
            ("client_secret_ref", "cred://client-secret"),
            ("other_ref", "cred://missing-other"),
        ]);
        let upstream = svc.create_upstream(&ctx, req).await.unwrap();

        // The update path checks them too.
        let mut update = make_update_from_upstream(&upstream);
        update.auth = oauth2(&missing_secret);
        assert_ref_not_accessible(
            svc.update_upstream(&ctx, upstream.id, update).await,
            "auth.config.client_secret_ref",
        );

        // A malformed reference names its key too.
        let mut req = make_create_upstream_hostname();
        req.auth = oauth2(&[
            ("client_id_ref", "cred://not a ref"),
            ("client_secret_ref", "cred://client-secret"),
        ]);
        match svc.create_upstream(&ctx, req).await {
            Err(DomainError::Validation { field, detail, .. }) => {
                assert_eq!(field, "auth.config.client_id_ref");
                assert!(!detail.contains("not a ref"), "the ref is echoed: {detail}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    /// A credential store that never answers fails the write after a
    /// bounded wait instead of holding it (an SDK write has no deadline).
    #[tokio::test(start_paused = true)]
    async fn a_stalled_credential_store_fails_the_write() {
        let svc = ControlPlaneServiceImpl::new(
            Arc::new(InMemoryUpstreamRepo::new()),
            Arc::new(InMemoryRouteRepo::new()),
            Arc::new(MockTenantResolverClient::single_tenant()),
            allow_all_enforcer(),
            Arc::new(crate::domain::test_support::StalledCredStore),
            Arc::new(SsrfGuard::disabled()),
            None,
        );
        let mut req = make_create_upstream_hostname();
        req.auth = Some(auth_with_secret_ref("cred://my-key"));
        let started = tokio::time::Instant::now();
        match svc.create_upstream(&test_ctx(Uuid::new_v4()), req).await {
            Err(DomainError::Internal { message }) => {
                assert!(message.contains("did not answer"), "{message}");
            }
            other => panic!("expected an internal error, got {other:?}"),
        }
        // The documented bound (the clock is paused, so this is exact).
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(5));
    }

    /// A credential key is resolved whatever the configured plugin: an
    /// `apikey` config with an OAuth2 key, and a noop or OAuth2 config with a
    /// `secret_ref`, are checked like the plugin that reads the key. A stored
    /// value then stays valid if the upstream switches to that plugin.
    #[tokio::test]
    async fn credential_keys_are_resolved_whatever_the_plugin() {
        let svc = make_service_with_resolver_and_creds(
            MockTenantResolverClient::single_tenant(),
            vec![
                ("cred://my-key".into(), "k".into()),
                ("cred://client-id".into(), "id".into()),
                ("cred://client-secret".into(), "secret".into()),
            ],
        );
        let ctx = test_ctx(Uuid::new_v4());
        let auth = |plugin_type: &str, refs: &[(&str, &str)]| {
            Some(AuthConfig {
                plugin_type: plugin_type.into(),
                sharing: SharingMode::Private,
                config: Some(
                    refs.iter()
                        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                        .collect(),
                ),
            })
        };
        let oauth2_refs = [
            ("token_endpoint", "https://idp.example.com/token"),
            ("client_id_ref", "cred://client-id"),
            ("client_secret_ref", "cred://client-secret"),
        ];

        let mut req = make_create_upstream_hostname();
        req.auth = auth(
            APIKEY_AUTH_PLUGIN_ID,
            &[
                ("header", "authorization"),
                ("secret_ref", "cred://my-key"),
                ("client_id_ref", "cred://missing-secret"),
            ],
        );
        assert_ref_not_accessible(
            svc.create_upstream(&ctx, req).await,
            "auth.config.client_id_ref",
        );

        let mut req = make_create_upstream_hostname();
        req.auth = auth(
            NOOP_AUTH_PLUGIN_ID,
            &[("secret_ref", "cred://missing-secret")],
        );
        assert_ref_not_accessible(
            svc.create_upstream(&ctx, req).await,
            "auth.config.secret_ref",
        );

        let mut with_secret_ref = oauth2_refs.to_vec();
        with_secret_ref.push(("secret_ref", "cred://missing-secret"));
        let mut req = make_create_upstream_hostname();
        req.auth = auth(OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, &with_secret_ref);
        assert_ref_not_accessible(
            svc.create_upstream(&ctx, req).await,
            "auth.config.secret_ref",
        );

        // A malformed one is refused too, on create and update.
        let mut req = make_create_upstream_hostname();
        req.auth = auth(NOOP_AUTH_PLUGIN_ID, &[("secret_ref", "cred://not a ref")]);
        assert_validation_on(
            svc.create_upstream(&ctx, req).await,
            "auth.config.secret_ref",
        );
        let mut req = make_create_upstream_hostname();
        req.auth = auth(OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, &oauth2_refs);
        let upstream = svc.create_upstream(&ctx, req).await.unwrap();
        let mut with_secret_ref = oauth2_refs.to_vec();
        with_secret_ref.push(("secret_ref", "cred://not a ref"));
        let mut update = make_update_from_upstream(&upstream);
        update.auth = auth(OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID, &with_secret_ref);
        assert_validation_on(
            svc.update_upstream(&ctx, upstream.id, update).await,
            "auth.config.secret_ref",
        );
    }

    #[track_caller]
    fn assert_validation_on<T: std::fmt::Debug>(result: Result<T, DomainError>, expected: &str) {
        match result {
            Err(DomainError::Validation { field, detail, .. }) => {
                assert_eq!(field, expected);
                assert!(!detail.contains("not a ref"), "the ref is echoed: {detail}");
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[track_caller]
    fn assert_ref_not_accessible<T: std::fmt::Debug>(
        result: Result<T, DomainError>,
        expected: &str,
    ) {
        match result {
            Err(DomainError::SecretRefNotAccessible { field, detail, .. }) => {
                assert_eq!(field, expected);
                assert!(!detail.contains("missing-secret"), "the ref is echoed");
            }
            other => panic!("expected SecretRefNotAccessible, got {other:?}"),
        }
    }
}
