use std::sync::Arc;

use crate::domain::model::{
    Endpoint, HttpMatch, HttpMethod, MatchRules, PathSuffixMode, Scheme, Server,
};

use super::*;
use crate::domain::test_support::{
    MockCredStoreClient, MockTenantResolverClient, allow_all_enforcer,
};
use crate::infra::storage::{InMemoryRouteRepo, InMemoryUpstreamRepo};
use tenant_resolver_sdk::TenantId;

fn make_service() -> ControlPlaneServiceImpl {
    ControlPlaneServiceImpl::new(
        Arc::new(InMemoryUpstreamRepo::new()),
        Arc::new(InMemoryRouteRepo::new()),
        Arc::new(MockTenantResolverClient::single_tenant()),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::empty()),
    )
}

fn make_service_with_resolver(resolver: MockTenantResolverClient) -> ControlPlaneServiceImpl {
    ControlPlaneServiceImpl::new(
        Arc::new(InMemoryUpstreamRepo::new()),
        Arc::new(InMemoryRouteRepo::new()),
        Arc::new(resolver),
        allow_all_enforcer(),
        Arc::new(MockCredStoreClient::empty()),
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
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: "api.openai.com".into(),
                port: 443,
            }],
        },
        protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
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
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: "10.0.0.1".into(),
                port: 443,
            }],
        },
        protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
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

    // Update alias (allowed for IP-based endpoints)
    let mut update_req = make_update_from_upstream(&u);
    update_req.alias = Some("openai-v2".into());
    let updated = svc.update_upstream(&ctx, u.id, update_req).await.unwrap();
    assert_eq!(updated.alias, "openai-v2");
    assert_eq!(updated.id, u.id);

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
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: "api.openai.com".into(),
                port: 8443,
            }],
        },
        protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
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
        .resolve_alias(&ctx, &chain, "openai", None)
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
        .resolve_alias(&ctx, &chain, "openai", None)
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
        .resolve_alias(&ctx, &chain, "nonexistent", None)
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::NotFound { .. }));
}

#[tokio::test]
async fn route_matching_through_cp() {
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

    let chain = svc.build_tenant_chain(&ctx).await.unwrap();
    let matched = ControlPlaneServiceImpl::find_route_in_chain(
        &*svc.routes,
        &chain,
        u.id,
        "POST",
        "/v1/chat/completions",
    )
    .await
    .unwrap();
    assert_eq!(matched.id, r.id);
}

#[tokio::test]
async fn route_matching_no_match_returns_404() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_ip("openai"))
        .await
        .unwrap();

    let chain = svc.build_tenant_chain(&ctx).await.unwrap();
    let err = ControlPlaneServiceImpl::find_route_in_chain(
        &*svc.routes,
        &chain,
        u.id,
        "GET",
        "/v1/unknown",
    )
    .await
    .unwrap_err();
    assert!(matches!(err, DomainError::NotFound { .. }));
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
async fn delete_upstream_cascades_routes() {
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

    svc.delete_upstream(&ctx, u.id).await.unwrap();

    // Route should be gone.
    assert!(svc.get_route(&ctx, r.id).await.is_err());
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
        .resolve_alias(&child_ctx, &chain, "api.openai.com", None)
        .await
        .unwrap();
    assert_eq!(resolved.id, root_upstream.id);
}

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
        .resolve_alias(&child_ctx, &chain, "api.openai.com", None)
        .await
        .unwrap();
    assert_eq!(resolved.id, child_upstream.id);
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
        .resolve_alias(&child_ctx, &chain, "api.openai.com", None)
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
        .resolve_alias(&child_ctx, &chain, "api.openai.com", None)
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
        .resolve_alias(&child_ctx, &chain, "api.openai.com", None)
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
        .resolve_alias(&child_ctx, &chain, "api.openai.com", None)
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
        .resolve_alias(&child_ctx, &chain, "nonexistent", None)
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
    }
}

fn make_rate_limit(sharing: SharingMode, rate: u32, window: Window) -> RateLimitConfig {
    RateLimitConfig {
        sharing,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate { rate, window },
        burst: None,
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
    }
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
async fn bind_rejects_cors_override_on_private() {
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

    // Child attempts to override CORS → should fail (ancestor CORS is private).
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.cors = Some(make_cors(SharingMode::Inherit, vec!["https://child.com"]));
    let result = svc.create_upstream(&child_ctx, child_req).await;
    assert!(
        result.is_err(),
        "CORS override on private should be rejected"
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
async fn bind_rejects_rate_limit_override_on_private() {
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

    // Child tries to override rate_limit on private ancestor field.
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_hostname();
    child_req.rate_limit = Some(make_rate_limit(SharingMode::Inherit, 50, Window::Minute));
    let err = svc
        .create_upstream(&child_ctx, child_req)
        .await
        .unwrap_err();
    match err {
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("private"),
                "expected private error, got: {detail}"
            );
        }
        _ => panic!("expected Validation error, got: {err:?}"),
    }
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
        DomainError::Validation { detail, .. } => {
            assert!(
                detail.contains("not accessible"),
                "expected 'not accessible' error, got: {detail}"
            );
        }
        _ => panic!("expected Validation error, got: {err:?}"),
    }
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
async fn update_alias_to_ancestor_requires_bind() {
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

    // Child creates upstream with different alias (IP-based).
    let child_ctx = test_ctx(child);
    let child_upstream = svc
        .create_upstream(&child_ctx, make_create_upstream_ip("other"))
        .await
        .unwrap();

    // Child updates alias to match ancestor — with allow-all enforcer this passes.
    let mut update_req = make_update_from_upstream(&child_upstream);
    update_req.alias = Some("openai".into());
    let updated = svc
        .update_upstream(&child_ctx, child_upstream.id, update_req)
        .await
        .unwrap();
    assert_eq!(updated.alias, "openai");
}

#[tokio::test]
async fn update_alias_only_validates_existing_overrides_against_ancestor_enforce() {
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

    // Child creates upstream with a different alias but with auth already set (IP-based).
    let child_ctx = test_ctx(child);
    let mut child_req = make_create_upstream_ip("other");
    child_req.auth = Some(AuthConfig {
        plugin_type: "oauth2".into(),
        sharing: SharingMode::Inherit,
        config: None,
    });
    let child_upstream = svc.create_upstream(&child_ctx, child_req).await.unwrap();

    // Alias-only update to match ancestor — must fail because the child's
    // existing auth override conflicts with the ancestor's enforce mode.
    let mut update_req = make_update_from_upstream(&child_upstream);
    update_req.alias = Some("openai".into());
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
        .resolve_proxy_target(&child_ctx, "api.openai.com", "POST", "/v1/chat")
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
        .resolve_proxy_target(&child_ctx, "api.openai.com", "POST", "/v1/chat")
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
fn route_private_plugins_are_skipped() {
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
    };

    let effective = compute_effective_config(&[u], Some(&route)).unwrap();
    let items = effective.plugins.unwrap().items;
    // Route plugins with Private sharing are skipped — only upstream plugins remain.
    assert_eq!(
        items
            .iter()
            .map(|b| b.plugin_ref.as_str())
            .collect::<Vec<_>>(),
        vec!["upstream-plugin"]
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
    let err = enforce_alias_create(Some("custom-alias"), &eps).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn enforce_alias_create_hostname_tolerates_exact_match() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    }];
    let alias = enforce_alias_create(Some("api.openai.com"), &eps).unwrap();
    assert_eq!(alias, "api.openai.com");
}

#[test]
fn enforce_alias_create_hostname_auto_derives() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    }];
    let alias = enforce_alias_create(None, &eps).unwrap();
    assert_eq!(alias, "api.openai.com");
}

#[test]
fn enforce_alias_create_ip_requires_explicit() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.1".into(),
        port: 443,
    }];
    let err = enforce_alias_create(None, &eps).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn enforce_alias_create_ip_accepts_explicit() {
    let eps = vec![Endpoint {
        scheme: Scheme::Https,
        host: "10.0.1.1".into(),
        port: 443,
    }];
    let alias = enforce_alias_create(Some("my-backend"), &eps).unwrap();
    assert_eq!(alias, "my-backend");
}

#[test]
fn enforce_alias_update_hostname_to_hostname_recomputes() {
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
    let alias = enforce_alias_update(None, &new_eps, "old.vendor.com", &old_eps).unwrap();
    assert_eq!(alias, "new.vendor.com");
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
    let err = enforce_alias_update(None, &new_eps, "api.openai.com", &old_eps).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn enforce_alias_update_hostname_to_ip_with_alias_succeeds() {
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
    let alias =
        enforce_alias_update(Some("my-backend"), &new_eps, "api.openai.com", &old_eps).unwrap();
    assert_eq!(alias, "my-backend");
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
    let alias = enforce_alias_update(None, &new_eps, "my-backend", &old_eps).unwrap();
    assert_eq!(alias, "my-backend");
}

#[test]
fn enforce_alias_update_ip_to_hostname_recomputes() {
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
    let alias = enforce_alias_update(None, &new_eps, "my-backend", &old_eps).unwrap();
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
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: "10.0.0.1".into(),
                port: 443,
            }],
        },
        protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
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
async fn update_endpoints_recomputes_alias() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();
    assert_eq!(u.alias, "api.openai.com");

    // Update endpoints to a different host — alias should recompute.
    let mut update_req = make_update_from_upstream(&u);
    update_req.server = Server {
        endpoints: vec![Endpoint {
            scheme: Scheme::Https,
            host: "api.anthropic.com".into(),
            port: 443,
        }],
    };
    update_req.alias = None; // let alias be re-derived
    let updated = svc.update_upstream(&ctx, u.id, update_req).await.unwrap();
    assert_eq!(updated.alias, "api.anthropic.com");
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
async fn update_hostname_to_ip_with_alias_succeeds() {
    let svc = make_service();
    let tenant = Uuid::new_v4();
    let ctx = test_ctx(tenant);

    let u = svc
        .create_upstream(&ctx, make_create_upstream_hostname())
        .await
        .unwrap();

    // Switch to IP endpoints with explicit alias.
    let mut update_req = make_update_from_upstream(&u);
    update_req.server = Server {
        endpoints: vec![Endpoint {
            scheme: Scheme::Https,
            host: "10.0.0.1".into(),
            port: 443,
        }],
    };
    update_req.alias = Some("my-backend".into());
    let updated = svc.update_upstream(&ctx, u.id, update_req).await.unwrap();
    assert_eq!(updated.alias, "my-backend");
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
        .resolve_alias(&ctx, &chain, "Api.OpenAI.COM", None)
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
        protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
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
        protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
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
        protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
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
        protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
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
        protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
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
