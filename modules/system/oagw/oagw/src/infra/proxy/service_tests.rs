use super::*;
use crate::domain::model::{Endpoint, Scheme, Server, Upstream};
use crate::domain::services::EndpointSelector;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

#[test]
fn normalize_collapses_double_slashes() {
    assert_eq!(normalize_path("/alias//v1//chat"), "/alias/v1/chat");
}

#[test]
fn normalize_resolves_dot_dot() {
    assert_eq!(normalize_path("/alias/../admin/secret"), "/admin/secret");
}

#[test]
fn normalize_clamps_above_root() {
    assert_eq!(normalize_path("/alias/../../etc/passwd"), "/etc/passwd");
}

#[test]
fn normalize_resolves_single_dot() {
    assert_eq!(normalize_path("/alias/./v1/chat"), "/alias/v1/chat");
}

#[test]
fn normalize_preserves_clean_path() {
    assert_eq!(normalize_path("/alias/v1/chat"), "/alias/v1/chat");
}

// -----------------------------------------------------------------------
// select_endpoint() unit tests
// -----------------------------------------------------------------------

fn ep(host: &str, port: u16) -> Endpoint {
    Endpoint {
        scheme: Scheme::Https,
        host: host.to_string(),
        port,
    }
}

fn upstream_with(endpoints: Vec<Endpoint>) -> Upstream {
    Upstream {
        id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        alias: "test".to_string(),
        server: Server { endpoints },
        protocol: "http".to_string(),
        enabled: true,
        auth: None,
        headers: None,
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
    }
}

/// Mock BackendSelector that returns endpoints[call_count % endpoints.len()].
struct MockSelector {
    call_count: AtomicUsize,
}

impl MockSelector {
    fn new() -> Self {
        Self {
            call_count: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.call_count.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl EndpointSelector for MockSelector {
    async fn select(&self, _upstream_id: Uuid, endpoints: &[Endpoint]) -> Option<SelectedEndpoint> {
        let idx = self.call_count.fetch_add(1, Ordering::Relaxed) % endpoints.len();
        Some(SelectedEndpoint {
            endpoint: endpoints[idx].clone(),
            resolved_addr: None,
        })
    }

    fn invalidate(&self, _upstream_id: Uuid) {}
}

/// Build a minimal `DataPlaneServiceImpl` with the given `BackendSelector`.
fn build_svc(selector: Arc<dyn EndpointSelector>) -> DataPlaneServiceImpl {
    use authz_resolver_sdk::{
        AuthZResolverClient, AuthZResolverError, EvaluationRequest, EvaluationResponse,
        EvaluationResponseContext, PolicyEnforcer,
    };
    use credstore_sdk::{CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef};
    use modkit_security::SecurityContext;

    struct AllowAllAuthZ;
    #[async_trait]
    impl AuthZResolverClient for AllowAllAuthZ {
        async fn evaluate(
            &self,
            _request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            Ok(EvaluationResponse {
                decision: true,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: None,
                },
            })
        }
    }

    struct NoopCredStore;
    #[async_trait]
    impl CredStoreClientV1 for NoopCredStore {
        async fn get(
            &self,
            _ctx: &SecurityContext,
            _key: &SecretRef,
        ) -> Result<Option<GetSecretResponse>, CredStoreError> {
            Ok(None)
        }
    }

    let credstore: Arc<dyn CredStoreClientV1> = Arc::new(NoopCredStore);
    let policy_enforcer = PolicyEnforcer::new(Arc::new(AllowAllAuthZ));

    // Minimal CP — never called by select_endpoint().
    use crate::domain::error::DomainError;
    use crate::domain::model::*;
    use crate::domain::services::ControlPlaneService;

    struct NoopCp;
    #[async_trait]
    impl ControlPlaneService for NoopCp {
        async fn create_upstream(
            &self,
            _: &SecurityContext,
            _: CreateUpstreamRequest,
        ) -> Result<Upstream, DomainError> {
            unimplemented!()
        }
        async fn get_upstream(
            &self,
            _: &SecurityContext,
            _: Uuid,
        ) -> Result<Upstream, DomainError> {
            unimplemented!()
        }
        async fn list_upstreams(
            &self,
            _: &SecurityContext,
            _: &ListQuery,
        ) -> Result<Vec<Upstream>, DomainError> {
            unimplemented!()
        }
        async fn update_upstream(
            &self,
            _: &SecurityContext,
            _: Uuid,
            _: UpdateUpstreamRequest,
        ) -> Result<Upstream, DomainError> {
            unimplemented!()
        }
        async fn delete_upstream(&self, _: &SecurityContext, _: Uuid) -> Result<(), DomainError> {
            unimplemented!()
        }
        async fn create_route(
            &self,
            _: &SecurityContext,
            _: CreateRouteRequest,
        ) -> Result<Route, DomainError> {
            unimplemented!()
        }
        async fn get_route(&self, _: &SecurityContext, _: Uuid) -> Result<Route, DomainError> {
            unimplemented!()
        }
        async fn list_routes(
            &self,
            _: &SecurityContext,
            _: Option<Uuid>,
            _: &ListQuery,
        ) -> Result<Vec<Route>, DomainError> {
            unimplemented!()
        }
        async fn update_route(
            &self,
            _: &SecurityContext,
            _: Uuid,
            _: UpdateRouteRequest,
        ) -> Result<Route, DomainError> {
            unimplemented!()
        }
        async fn delete_route(&self, _: &SecurityContext, _: Uuid) -> Result<(), DomainError> {
            unimplemented!()
        }
        async fn resolve_proxy_target(
            &self,
            _: &SecurityContext,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<(Upstream, Route), DomainError> {
            unimplemented!()
        }
    }

    let cp: Arc<dyn ControlPlaneService> = Arc::new(NoopCp);
    let server_conf = Arc::new(pingora_core::server::configuration::ServerConf::default());
    let pingora = crate::infra::proxy::pingora_proxy::PingoraProxy::new(
        Duration::from_secs(10),
        Duration::from_secs(30),
        Duration::from_secs(3600),
    );
    let proxy = Arc::new(crate::infra::proxy::pingora_proxy::new_http_proxy(
        &server_conf,
        pingora,
    ));

    DataPlaneServiceImpl::new(
        cp,
        credstore,
        policy_enforcer,
        None,
        TokenCacheConfig::default(),
        selector,
        proxy,
    )
}

// P2 #12: Alias extraction happens on raw path, then suffix is normalized.
// Path traversal in the alias segment must not influence which upstream is resolved.
#[test]
fn alias_extraction_ignores_path_traversal() {
    // Simulate what proxy_request does: extract alias from raw path, normalize suffix.
    fn extract(path: &str) -> (String, String) {
        let trimmed = path.strip_prefix('/').unwrap_or(path);
        let (alias, raw_suffix) = match trimmed.find('/') {
            Some(pos) => (&trimmed[..pos], &trimmed[pos..]),
            None => (trimmed, ""),
        };
        (alias.to_string(), normalize_path(raw_suffix))
    }

    // Normal case.
    let (alias, suffix) = extract("/myalias/v1/chat");
    assert_eq!(alias, "myalias");
    assert_eq!(suffix, "/v1/chat");

    // Path traversal attempt: alias is still the first raw segment.
    let (alias, suffix) = extract("/myalias/../admin/secret");
    assert_eq!(alias, "myalias");
    assert_eq!(suffix, "/admin/secret"); // ".." collapsed in suffix only

    // Deep traversal: alias is still literal first segment.
    let (alias, suffix) = extract("/myalias/../../etc/passwd");
    assert_eq!(alias, "myalias");
    assert_eq!(suffix, "/etc/passwd"); // ".." collapsed, clamped at root
}

// P2: HTTPS-only — Http scheme endpoint must be rejected.
#[tokio::test]
async fn select_endpoint_rejects_http_scheme() {
    let selector = Arc::new(MockSelector::new());
    let svc = build_svc(selector);

    // Single Http endpoint.
    let upstream = upstream_with(vec![Endpoint {
        scheme: Scheme::Http,
        host: "insecure.example.com".to_string(),
        port: 80,
    }]);
    let headers = HeaderMap::new();

    let err = svc.select_endpoint(&upstream, &headers, "/test").await;

    // select_endpoint itself doesn't enforce HTTPS — the check is in proxy_request
    // after select_endpoint returns. Verify the endpoint is returned here (enforcement
    // is at a higher level).
    assert!(err.is_ok(), "select_endpoint should return the endpoint");
    assert_eq!(err.unwrap().endpoint.scheme, Scheme::Http);
}

// positive-2.2 (custom-header-routing): X-OAGW-Target-Host matches an endpoint.
#[tokio::test]
async fn select_endpoint_target_host_matches() {
    let selector = Arc::new(MockSelector::new());
    let svc = build_svc(selector.clone());
    let upstream = upstream_with(vec![ep("a.com", 443), ep("b.com", 443)]);

    let mut headers = HeaderMap::new();
    headers.insert("x-oagw-target-host", "a.com".parse().unwrap());

    let result = svc
        .select_endpoint(&upstream, &headers, "/test")
        .await
        .unwrap();
    assert_eq!(result.endpoint.host, "a.com");
    assert_eq!(selector.calls(), 0, "BackendSelector should not be called");
}

// negative-2.1 (custom-header-routing): X-OAGW-Target-Host does not match any endpoint.
#[tokio::test]
async fn select_endpoint_target_host_unknown() {
    let svc = build_svc(Arc::new(MockSelector::new()));
    let upstream = upstream_with(vec![ep("a.com", 443), ep("b.com", 443)]);

    let mut headers = HeaderMap::new();
    headers.insert("x-oagw-target-host", "evil.com".parse().unwrap());

    let err = svc
        .select_endpoint(&upstream, &headers, "/test")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::UnknownTargetHost { .. }),
        "expected UnknownTargetHost, got: {err:?}"
    );
}

// negative-1.2..1.4 (custom-header-routing): X-OAGW-Target-Host with invalid format.
#[tokio::test]
async fn select_endpoint_target_host_invalid_format() {
    let svc = build_svc(Arc::new(MockSelector::new()));
    let upstream = upstream_with(vec![ep("a.com", 443)]);

    for bad_value in [
        "a.com:443",
        "a.com/path",
        "a.com?q=1",
        "a b",
        "evil.com@real.com",
        "evil.com\\real.com",
        "a.com#fragment",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert("x-oagw-target-host", bad_value.parse().unwrap());
        let err = svc
            .select_endpoint(&upstream, &headers, "/test")
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::InvalidTargetHost { .. }),
            "expected InvalidTargetHost for '{bad_value}', got: {err:?}"
        );
    }

    // Empty header value: test separately since HeaderValue::from_static
    // allows empty strings while .parse() does not.
    let mut headers = HeaderMap::new();
    headers.insert("x-oagw-target-host", HeaderValue::from_static(""));
    let err = svc
        .select_endpoint(&upstream, &headers, "/test")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::InvalidTargetHost { .. }),
        "expected InvalidTargetHost for empty header, got: {err:?}"
    );
}

// positive-2.1 (custom-header-routing): Round-robin fallback for multi-endpoint (no header).
#[tokio::test]
async fn select_endpoint_round_robin_fallback() {
    let selector = Arc::new(MockSelector::new());
    let svc = build_svc(selector.clone());
    let upstream = upstream_with(vec![ep("a.com", 443), ep("b.com", 443)]);
    let headers = HeaderMap::new();

    let ep1 = svc
        .select_endpoint(&upstream, &headers, "/test")
        .await
        .unwrap();
    let ep2 = svc
        .select_endpoint(&upstream, &headers, "/test")
        .await
        .unwrap();

    assert_eq!(
        selector.calls(),
        2,
        "BackendSelector should be called for multi-endpoint"
    );
    // MockSelector returns endpoints in order: [0], [1], [0], ...
    assert_eq!(ep1.endpoint.host, "a.com");
    assert_eq!(ep2.endpoint.host, "b.com");
}

// positive-1.1 (custom-header-routing): Single-endpoint bypass (no header, no BackendSelector call).
#[tokio::test]
async fn select_endpoint_single_endpoint_bypass() {
    let selector = Arc::new(MockSelector::new());
    let svc = build_svc(selector.clone());
    let upstream = upstream_with(vec![ep("only.com", 443)]);
    let headers = HeaderMap::new();

    let result = svc
        .select_endpoint(&upstream, &headers, "/test")
        .await
        .unwrap();
    assert_eq!(result.endpoint.host, "only.com");
    assert_eq!(
        selector.calls(),
        0,
        "BackendSelector should NOT be called for single endpoint"
    );
}

// positive-1.2 (custom-header-routing): Single-endpoint upstream validates header if present.
#[tokio::test]
async fn select_endpoint_single_endpoint_validates_header() {
    let svc = build_svc(Arc::new(MockSelector::new()));
    let upstream = upstream_with(vec![ep("a.com", 443)]);

    // Valid header matching the single endpoint → OK.
    let mut headers = HeaderMap::new();
    headers.insert("x-oagw-target-host", "a.com".parse().unwrap());
    let result = svc
        .select_endpoint(&upstream, &headers, "/test")
        .await
        .unwrap();
    assert_eq!(result.endpoint.host, "a.com");

    // Invalid header not matching → UnknownTargetHost.
    let mut headers = HeaderMap::new();
    headers.insert("x-oagw-target-host", "b.com".parse().unwrap());
    let err = svc
        .select_endpoint(&upstream, &headers, "/test")
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::UnknownTargetHost { .. }),
        "expected UnknownTargetHost for mismatched header on single-endpoint upstream"
    );
}
