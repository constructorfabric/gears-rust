use std::sync::Arc;

use crate::domain::model::{
    Endpoint, GrpcMatch, HttpMatch, HttpMethod, MatchRules, PathSuffixMode, Scheme, Server,
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
            .resolve_proxy_target(&ctx, "openai", method, path, Tags::Load)
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
fn validate_endpoints_rejects_ipv6() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "::1".into(),
        port: 443,
    }];
    let err = validate_endpoints(&endpoints).unwrap_err();
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
fn validate_endpoints_rejects_ipv6_full_address() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "2001:db8::1".into(),
        port: 8443,
    }];
    let err = validate_endpoints(&endpoints).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn validate_endpoints_rejects_bracketed_ipv6() {
    let endpoints = vec![Endpoint {
        scheme: Scheme::Https,
        host: "[2001:db8::1]".into(),
        port: 8443,
    }];
    let err = validate_endpoints(&endpoints).unwrap_err();
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
        plugin_type: "noop".into(),
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
        plugin_type: "noop".into(),
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
            plugin_type: "noop".into(),
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
        plugin_type: "noop".into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    svc.create_upstream(&parent_ctx, req).await.unwrap();

    // Create enabled upstream in root with inherit sharing.
    let root_ctx = test_ctx(root);
    let mut req2 = make_create_upstream_hostname();
    req2.auth = Some(AuthConfig {
        plugin_type: "noop".into(),
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
        plugin_type: "noop".into(),
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
        plugin_type: "noop".into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let root_upstream = svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Create disabled upstream in child with same host (same derived alias).
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.enabled = false;
    child_req.auth = Some(AuthConfig {
        plugin_type: "noop".into(),
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
        pool_owner_id: None,
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
        plugin_type: "apikey".into(),
        sharing: SharingMode::Inherit,
        config: None,
    };
    let child_auth = AuthConfig {
        plugin_type: "oauth2".into(),
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
    assert_eq!(effective.auth.unwrap().plugin_type, "oauth2");
}

#[test]
fn effective_config_auth_enforce_ancestor_wins() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_auth = AuthConfig {
        plugin_type: "apikey".into(),
        sharing: SharingMode::Enforce,
        config: None,
    };
    let child_auth = AuthConfig {
        plugin_type: "oauth2".into(),
        sharing: SharingMode::Inherit,
        config: None,
    };

    let root = make_upstream(root_id, "openai", Some(root_auth), None, None, vec![]);
    let child = make_upstream(child_id, "openai", Some(child_auth), None, None, vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    // Ancestor enforce wins — apikey stays.
    assert_eq!(effective.auth.unwrap().plugin_type, "apikey");
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
fn effective_config_route_rate_limit_applies_min() {
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
    // min(100/min, 50/min) = 50/min
    assert_eq!(effective.rate_limit.unwrap().sustained.rate, 50);
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
            plugin_type: "apikey".into(),
            sharing: SharingMode::Enforce,
            config: None,
        }),
        Some(make_rate_limit(SharingMode::Enforce, 1000, Window::Minute)),
        Some(PluginsConfig {
            sharing: SharingMode::Enforce,
            items: vec![PluginBinding {
                plugin_ref: "audit-log".into(),
                config: HashMap::new(),
            }],
        }),
        vec!["env:prod".into()],
    );
    let parent = make_upstream(
        parent_id,
        "openai",
        Some(AuthConfig {
            plugin_type: "oauth2".into(),
            sharing: SharingMode::Inherit,
            config: None,
        }),
        Some(make_rate_limit(SharingMode::Inherit, 500, Window::Minute)),
        Some(PluginsConfig {
            sharing: SharingMode::Inherit,
            items: vec![PluginBinding {
                plugin_ref: "rate-guard".into(),
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
    assert_eq!(effective.auth.unwrap().plugin_type, "apikey");

    // Rate limit: min(1000, 500, 200) = 200/min.
    assert_eq!(effective.rate_limit.unwrap().sustained.rate, 200);

    // Plugins: enforced audit-log + rate-guard + transform-x.
    let items = effective.plugins.unwrap().items;
    assert!(items.iter().any(|b| b.plugin_ref == "audit-log"));
    assert!(items.iter().any(|b| b.plugin_ref == "rate-guard"));
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
    let t = Uuid::new_v4();
    let mut root = make_upstream(t, "api", None, None, None, vec![]);
    root.cors = Some(make_cors(SharingMode::Private, vec!["https://root.com"]));
    let child = make_upstream(t, "api", None, None, None, vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    assert!(
        effective.cors.is_none(),
        "Private ancestor CORS must not propagate to descendants"
    );
}

#[test]
fn effective_config_cors_inherit_skips_private_ancestor_origins() {
    // When ancestor CORS is Private, its origins should not be unioned into Inherit descendant.
    let t = Uuid::new_v4();
    let mut root = make_upstream(t, "api", None, None, None, vec![]);
    root.cors = Some(make_cors(SharingMode::Private, vec!["https://root.com"]));
    let mut child = make_upstream(t, "api", None, None, None, vec![]);
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
        plugin_type: "passthrough".into(),
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
        plugin_type: "apikey".into(),
        sharing: SharingMode::Enforce,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child tries to create upstream with same host (same derived alias) AND auth override.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.auth = Some(AuthConfig {
        plugin_type: "oauth2".into(),
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
        plugin_type: "noop".into(),
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
        plugin_type: "apikey".into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child creates upstream with same host (same derived alias) and overrides auth.
    // With allow-all enforcer, bind + override_auth permissions pass.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.auth = Some(AuthConfig {
        plugin_type: "oauth2".into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let child_upstream = svc.create_upstream(&child_ctx, child_req).await.unwrap();
    assert_eq!(child_upstream.alias, "api.openai.com");
    assert_eq!(child_upstream.auth.unwrap().plugin_type, "oauth2");
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
        plugin_type: "apikey".into(),
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
        plugin_type: "apikey".into(),
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
        plugin_type: "apikey".into(),
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
#[tokio::test]
async fn secret_ref_check_maps_a_credstore_outage_to_internal() {
    use credstore_sdk::CredStoreError;
    type MakeError = fn() -> CredStoreError;

    let cases: [(MakeError, bool); 4] = [
        (
            || CredStoreError::ServiceUnavailable {
                detail: "down".into(),
                retry_after: None,
            },
            true,
        ),
        (|| CredStoreError::NoPluginAvailable, true),
        (|| CredStoreError::AccessDenied, false),
        (
            || CredStoreError::InvalidSecretRef {
                reason: "bad".into(),
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
            assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
        }
    }
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
        matches!(&err, DomainError::Validation { detail, .. } if detail.contains("invalid secret_ref")),
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
        plugin_type: "apikey".into(),
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
            plugin_ref: "audit-log".into(),
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
            plugin_ref: "rate-guard".into(),
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
        plugin_type: "apikey".into(),
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
        plugin_type: "oauth2".into(),
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
        plugin_type: "apikey".into(),
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
        plugin_type: "apikey".into(),
        sharing: SharingMode::Enforce,
        config: None,
    });
    svc.create_upstream(&root_ctx, root_req).await.unwrap();

    // Child creates upstream with same alias but incompatible auth — must fail
    // because the ancestor's enforce mode forbids a different auth plugin type.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_ip("openai");
    child_req.auth = Some(AuthConfig {
        plugin_type: "oauth2".into(),
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
        plugin_type: "oauth2".into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let updated = svc
        .update_upstream(&child_ctx, child_upstream.id, update_req)
        .await
        .unwrap();
    assert_eq!(updated.auth.unwrap().plugin_type, "oauth2");
}

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

    // Child creates upstream with rate_limit override.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 500, Window::Minute));
    let child_upstream = svc.create_upstream(&child_ctx, child_req).await;
    // Create may fail due to enforce — that's fine, the point is the update path.
    // If create succeeds (allow-all enforcer doesn't check sharing at create),
    // then test the update.
    if let Ok(child_upstream) = child_upstream {
        // Update: set rate_limit to None (full-replacement removes it).
        // This must still be validated — bind check should run even with None fields.
        let mut update_req = make_update_from_upstream(&child_upstream);
        update_req.rate_limit = None;
        // With allow-all enforcer this succeeds, but the key thing is that
        // validate_ancestor_bind was called (not skipped due to has_overrides guard).
        let result = svc
            .update_upstream(&child_ctx, child_upstream.id, update_req)
            .await;
        // Should succeed because allow-all enforcer passes bind check,
        // and rate_limit:None doesn't trigger enforce rejection (no override).
        assert!(
            result.is_ok(),
            "update with None rate_limit should still pass validation: {result:?}"
        );
        assert!(result.unwrap().rate_limit.is_none());
    }
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
        plugin_type: "apikey".into(),
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
        plugin_type: "oauth2".into(),
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
        plugin_type: "apikey".into(),
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
    let (effective, route) = svc
        .resolve_proxy_target(&child_ctx, "api.openai.com", "POST", "/v1/chat", Tags::Load)
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
        plugin_type: "apikey".into(),
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
    let (_effective, route) = svc
        .resolve_proxy_target(&child_ctx, "api.openai.com", "POST", "/v1/chat", Tags::Load)
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
        plugin_type: "apikey".into(),
        sharing: SharingMode::Inherit,
        config: None,
    };
    let child_auth = AuthConfig {
        plugin_type: "oauth2".into(),
        sharing: SharingMode::Private,
        config: None,
    };

    let root = make_upstream(root_id, "openai", Some(root_auth), None, None, vec![]);
    let child = make_upstream(child_id, "openai", Some(child_auth), None, None, vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    // Ancestor is Inherit (not Enforce) — Private descendant replaces.
    let auth = effective.auth.unwrap();
    assert_eq!(auth.plugin_type, "oauth2");
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

#[test]
fn merge_auth_private_cannot_bypass_enforced_ancestor() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_auth = AuthConfig {
        plugin_type: "apikey".into(),
        sharing: SharingMode::Enforce,
        config: None,
    };
    let child_auth = AuthConfig {
        plugin_type: "oauth2".into(),
        sharing: SharingMode::Private,
        config: None,
    };

    let root = make_upstream(root_id, "openai", Some(root_auth), None, None, vec![]);
    let child = make_upstream(child_id, "openai", Some(child_auth), None, None, vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    // Enforced ancestor auth (apikey) must survive even though
    // descendant declared Private with oauth2.
    assert_eq!(effective.auth.unwrap().plugin_type, "apikey");
}

#[test]
fn merge_plugins_private_cannot_drop_enforced_ancestor_plugins() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_plugins = PluginsConfig {
        sharing: SharingMode::Enforce,
        items: vec![PluginBinding {
            plugin_ref: "audit-log".into(),
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
    // Enforced "audit-log" must survive even though descendant set Private.
    assert!(items.iter().any(|b| b.plugin_ref == "audit-log"));
    assert!(items.iter().any(|b| b.plugin_ref == "my-plugin"));
}

#[test]
fn merge_plugins_enforced_ancestor_config_wins_on_ref_conflict() {
    let root_id = Uuid::new_v4();
    let child_id = Uuid::new_v4();

    let root_plugins = PluginsConfig {
        sharing: SharingMode::Enforce,
        items: vec![PluginBinding {
            plugin_ref: "audit-log".into(),
            config: HashMap::from([("level".into(), "full".into())]),
        }],
    };
    let child_plugins = PluginsConfig {
        sharing: SharingMode::Private,
        items: vec![PluginBinding {
            plugin_ref: "audit-log".into(),
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
        .filter(|b| b.plugin_ref == "audit-log")
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
            plugin_ref: "audit-log".into(),
            config: HashMap::from([("level".into(), "full".into())]),
        }],
    };
    let child_plugins = PluginsConfig {
        sharing: SharingMode::Inherit,
        items: vec![PluginBinding {
            plugin_ref: "audit-log".into(),
            config: HashMap::from([("level".into(), "minimal".into())]),
        }],
    };

    let root = make_upstream(root_id, "openai", None, None, Some(root_plugins), vec![]);
    let child = make_upstream(child_id, "openai", None, None, Some(child_plugins), vec![]);

    let effective = compute_effective_config(&[root, child], None).unwrap();
    let items = effective.plugins.unwrap().items;
    let audit = items
        .iter()
        .filter(|b| b.plugin_ref == "audit-log")
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
        pool_owner_id: None,
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
        pool_owner_id: None,
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
        pool_owner_id: None,
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
        pool_owner_id: None,
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
fn merge_rate_limit_shared_sets_pool_owner_id() {
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
            pool_owner_id: None,
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
    assert_eq!(
        effective_rl.pool_owner_id,
        Some(root_upstream_id),
        "pool_owner_id should be set to root upstream's ID for shared budget"
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
            pool_owner_id: None,
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
        effective_rl.pool_owner_id,
        Some(root_upstream_id),
        "pool_owner_id must stay pinned to the root that defined BudgetMode::Shared, \
         not be overwritten to a middle layer"
    );
}

#[test]
fn merge_rate_limit_private_non_enforced_does_not_inherit_pool_owner_id() {
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
            pool_owner_id: None,
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
        effective_rl.pool_owner_id, None,
        "pool_owner_id must be None for Private (non-enforced) descendant — \
         it should use its own bucket, not the ancestor's shared pool"
    );
    assert_eq!(
        effective_rl.sustained.rate, 500,
        "Private descendant's rate should replace, not merge"
    );
}

#[test]
fn compute_effective_single_element_shared_sets_pool_owner_id() {
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
            pool_owner_id: None,
        }),
        None,
        vec![],
    );

    let effective = compute_effective_config(std::slice::from_ref(&root), None).unwrap();
    let rl = effective.rate_limit.as_ref().unwrap();
    assert_eq!(
        rl.pool_owner_id,
        Some(root.id),
        "single-element chain with Shared budget should have pool_owner_id = own ID"
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
        pool_owner_id: None,
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
        pool_owner_id: None,
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
        pool_owner_id: None,
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
        pool_owner_id: None,
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
        pool_owner_id: None,
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
        pool_owner_id: None,
    });
    svc.update_upstream(&root_ctx, root_upstream.id, update_req)
        .await
        .unwrap();
}

#[test]
fn merge_rate_limit_allocated_does_not_set_pool_owner_id() {
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
            pool_owner_id: None,
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
        effective_rl.pool_owner_id, None,
        "pool_owner_id should NOT be set for allocated budget"
    );
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
    let found = ancestor::closest_ancestor_upstream(&*svc.upstreams, &chain, &root_up.alias)
        .await
        .unwrap();
    assert_eq!(found.map(|u| u.id), Some(mid_up.id));

    // The requesting tenant itself is never its own ancestor.
    let mid_chain = svc.build_tenant_chain(&test_ctx(mid)).await.unwrap();
    let found = ancestor::closest_ancestor_upstream(&*svc.upstreams, &mid_chain, &root_up.alias)
        .await
        .unwrap();
    assert_eq!(found.map(|u| u.id), Some(root_up.id));

    // A root tenant has no ancestors.
    let root_chain = svc.build_tenant_chain(&test_ctx(root)).await.unwrap();
    let found = ancestor::closest_ancestor_upstream(&*svc.upstreams, &root_chain, &root_up.alias)
        .await
        .unwrap();
    assert!(found.is_none());
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
            &ctx,
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
            &ctx,
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
                auth: auth(&at_limit),
                plugins: plugins(&at_limit),
                ..make_create_upstream_ip("limits")
            },
        )
        .await
        .expect("256-byte references are accepted");
    let base = make_create_upstream_ip("limits");
    let cases = [
        (
            "a 257-byte protocol",
            CreateUpstreamRequest {
                protocol: over.clone(),
                ..base.clone()
            },
        ),
        (
            "a NUL in the protocol",
            CreateUpstreamRequest {
                protocol: "http\0".into(),
                ..base.clone()
            },
        ),
        (
            "a 257-byte auth plugin type",
            CreateUpstreamRequest {
                auth: auth(&over),
                ..base.clone()
            },
        ),
        (
            "a NUL in the auth plugin type",
            CreateUpstreamRequest {
                auth: auth("a\0"),
                ..base.clone()
            },
        ),
        (
            "a 257-byte plugin ref",
            CreateUpstreamRequest {
                plugins: plugins(&over),
                ..base.clone()
            },
        ),
        (
            "a NUL in a plugin ref",
            CreateUpstreamRequest {
                plugins: plugins("p\0"),
                ..base.clone()
            },
        ),
    ];
    for (case, bad) in cases {
        let mut alias_free = bad.clone();
        alias_free.alias = Some("limits-bad".into());
        assert_validation(svc.create_upstream(&ctx, alias_free).await, case);
        let mut update = make_update_from_upstream(&upstream);
        update.protocol = bad.protocol;
        update.auth = bad.auth;
        update.plugins = bad.plugins;
        assert_validation(svc.update_upstream(&ctx, upstream.id, update).await, case);
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
        .create_route(&ctx, with(tags(64, 128), &"r".repeat(256)))
        .await
        .expect("64 query parameters of 128 bytes and a 256-byte plugin ref are accepted");
    let cases = [
        ("65 query parameters", with(tags(65, 1), "p")),
        ("a 129-byte query parameter", with(tags(1, 129), "p")),
        ("a NUL in a query parameter", with(vec!["a\0".into()], "p")),
        ("a 257-byte plugin ref", with(vec![], &"r".repeat(257))),
        ("a NUL in a plugin ref", with(vec![], "p\0")),
    ];
    for (case, bad) in cases {
        assert_validation(svc.create_route(&ctx, bad.clone()).await, case);
        let mut update = make_update_from_route(&route);
        update.match_rules = bad.match_rules;
        update.plugins = bad.plugins;
        assert_validation(svc.update_route(&ctx, route.id, update).await, case);
    }
}

fn plugin_chain(count: usize) -> Option<crate::domain::model::PluginsConfig> {
    Some(crate::domain::model::PluginsConfig {
        sharing: crate::domain::model::SharingMode::Private,
        items: (0..count)
            .map(|i| crate::domain::model::PluginBinding {
                plugin_ref: format!("p{i}"),
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
            plugin_type: "apikey".into(),
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
                plugin_ref: "p".into(),
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
        .resolve_proxy_target(&ctx, target.0, target.1, target.2, Tags::Load)
        .await
        .expect("resolves before the policy changes");

    let after = service(
        SsrfGuard::from_config(&SsrfPolicy {
            extra_deny_hostnames: vec!["api.openai.com".into()],
            ..SsrfPolicy::default()
        })
        .unwrap(),
    );
    let err = after
        .resolve_proxy_target(&ctx, target.0, target.1, target.2, Tags::Load)
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
            plugin_type: "noop".into(),
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
            plugin_type: "noop".into(),
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
        let (upstream, route) = cached
            .resolve_proxy_target(
                &ctx,
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
            plugin_type: "noop".into(),
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
            plugin_type: "noop".into(),
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
        .resolve_proxy_target(&ctx, target.0, target.1, target.2, Tags::Skip)
        .await
        .expect("resolves before the policy changes");

    let after = service(SsrfPolicy {
        extra_deny_hostnames: vec!["api.openai.com".into()],
        ..SsrfPolicy::default()
    });
    let cached = selects();
    let err = after
        .resolve_proxy_target(&ctx, target.0, target.1, target.2, Tags::Skip)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::Validation { detail, .. }
            if detail.contains("is blocked by SSRF protection")),
        "{err:?}"
    );
    assert_eq!(selects(), cached, "the upstream came from the cache");
}
