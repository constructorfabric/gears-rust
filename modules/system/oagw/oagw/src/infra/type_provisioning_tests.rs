use types_registry_sdk::{GtsEntity, RegisterResult, TypesRegistryError};

use super::*;

type ListFn = Box<dyn Fn(ListQuery) -> Result<Vec<GtsEntity>, TypesRegistryError> + Send + Sync>;

/// Mock `TypesRegistryClient` for unit testing.
struct MockRegistry {
    list_fn: ListFn,
}

#[async_trait]
impl TypesRegistryClient for MockRegistry {
    async fn register(
        &self,
        _entities: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, TypesRegistryError> {
        unimplemented!()
    }

    async fn list(&self, query: ListQuery) -> Result<Vec<GtsEntity>, TypesRegistryError> {
        (self.list_fn)(query)
    }

    async fn get(&self, _gts_id: &str) -> Result<GtsEntity, TypesRegistryError> {
        unimplemented!()
    }
}

fn make_upstream_entity(gts_id: &str, content: serde_json::Value) -> GtsEntity {
    GtsEntity::new(Uuid::new_v4(), gts_id, vec![], false, content, None)
}

fn make_route_entity(gts_id: &str, content: serde_json::Value) -> GtsEntity {
    GtsEntity::new(Uuid::new_v4(), gts_id, vec![], false, content, None)
}

fn upstream_content(tenant_id: Uuid) -> serde_json::Value {
    serde_json::json!({
        "tenant_id": tenant_id,
        "server": {
            "endpoints": [{"host": "127.0.0.1", "port": 8080, "scheme": "http"}]
        },
        "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
        "enabled": true,
        "tags": []
    })
}

fn route_content(tenant_id: Uuid, upstream_id: Uuid) -> serde_json::Value {
    serde_json::json!({
        "tenant_id": tenant_id,
        "upstream_id": upstream_id,
        "match": {
            "http": {
                "methods": ["GET"],
                "path": "/api/test"
            }
        },
        "enabled": true,
        "tags": [],
        "priority": 0
    })
}

#[tokio::test]
async fn list_upstreams_returns_parsed_entities() {
    let tenant = Uuid::new_v4();
    let content = upstream_content(tenant);

    let registry = Arc::new(MockRegistry {
        list_fn: Box::new(move |_| {
            Ok(vec![make_upstream_entity(
                "gts.x.core.oagw.upstream.v1~abc123",
                content.clone(),
            )])
        }),
    });
    let svc = TypeProvisioningServiceImpl::new(registry);

    let upstreams = svc.list_upstreams().await.unwrap();
    assert_eq!(upstreams.len(), 1);
    assert_eq!(upstreams[0].tenant_id, tenant);
    assert!(upstreams[0].request.enabled);
}

#[tokio::test]
async fn list_upstreams_skips_invalid_content() {
    let registry = Arc::new(MockRegistry {
        list_fn: Box::new(|_| {
            Ok(vec![make_upstream_entity(
                "gts.x.core.oagw.upstream.v1~bad",
                serde_json::json!({"invalid": true}),
            )])
        }),
    });
    let svc = TypeProvisioningServiceImpl::new(registry);

    let upstreams = svc.list_upstreams().await.unwrap();
    assert!(upstreams.is_empty());
}

#[tokio::test]
async fn list_upstreams_returns_empty_when_none_registered() {
    let registry = Arc::new(MockRegistry {
        list_fn: Box::new(|_| Ok(vec![])),
    });
    let svc = TypeProvisioningServiceImpl::new(registry);

    let upstreams = svc.list_upstreams().await.unwrap();
    assert!(upstreams.is_empty());
}

#[tokio::test]
async fn list_upstreams_propagates_registry_error() {
    let registry = Arc::new(MockRegistry {
        list_fn: Box::new(|_| Err(TypesRegistryError::internal("connection lost"))),
    });
    let svc = TypeProvisioningServiceImpl::new(registry);

    let result = svc.list_upstreams().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn list_routes_returns_parsed_entities() {
    let tenant = Uuid::new_v4();
    let upstream_id = Uuid::new_v4();
    let content = route_content(tenant, upstream_id);

    let registry = Arc::new(MockRegistry {
        list_fn: Box::new(move |_| {
            Ok(vec![make_route_entity(
                "gts.x.core.oagw.route.v1~abc123",
                content.clone(),
            )])
        }),
    });
    let svc = TypeProvisioningServiceImpl::new(registry);

    let routes = svc.list_routes().await.unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].tenant_id, tenant);
    assert_eq!(routes[0].request.upstream_id, upstream_id);
    assert!(routes[0].request.enabled);
}

#[tokio::test]
async fn list_routes_skips_invalid_content() {
    let registry = Arc::new(MockRegistry {
        list_fn: Box::new(|_| {
            Ok(vec![make_route_entity(
                "gts.x.core.oagw.route.v1~bad",
                serde_json::json!({"garbage": true}),
            )])
        }),
    });
    let svc = TypeProvisioningServiceImpl::new(registry);

    let routes = svc.list_routes().await.unwrap();
    assert!(routes.is_empty());
}

#[tokio::test]
async fn list_routes_propagates_registry_error() {
    let registry = Arc::new(MockRegistry {
        list_fn: Box::new(|_| Err(TypesRegistryError::internal("timeout"))),
    });
    let svc = TypeProvisioningServiceImpl::new(registry);

    let result = svc.list_routes().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn list_upstreams_uses_correct_pattern() {
    let registry = Arc::new(MockRegistry {
        list_fn: Box::new(|query| {
            assert_eq!(
                query.pattern.as_deref(),
                Some("gts.x.core.oagw.upstream.v1~*")
            );
            assert_eq!(query.is_type, Some(false));
            Ok(vec![])
        }),
    });
    let svc = TypeProvisioningServiceImpl::new(registry);

    let _ = svc.list_upstreams().await;
}

#[tokio::test]
async fn list_routes_uses_correct_pattern() {
    let registry = Arc::new(MockRegistry {
        list_fn: Box::new(|query| {
            assert_eq!(query.pattern.as_deref(), Some("gts.x.core.oagw.route.v1~*"));
            assert_eq!(query.is_type, Some(false));
            Ok(vec![])
        }),
    });
    let svc = TypeProvisioningServiceImpl::new(registry);

    let _ = svc.list_routes().await;
}

// -----------------------------------------------------------------------
// Payload deserialization tests
// -----------------------------------------------------------------------

#[test]
fn deserialize_valid_upstream_payload() {
    let tenant = Uuid::new_v4();
    let json = serde_json::json!({
        "tenant_id": tenant,
        "server": {
            "endpoints": [
                {"scheme": "https", "host": "api.openai.com", "port": 443},
                {"scheme": "http", "host": "fallback.local", "port": 8080}
            ]
        },
        "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
        "alias": "openai",
        "auth": {
            "type": "apikey",
            "sharing": "private",
            "config": {"header": "authorization", "prefix": "Bearer ", "secret_ref": "cred://key"}
        },
        "headers": {
            "request": {
                "set": {"x-custom": "value"},
                "passthrough": "all"
            }
        },
        "enabled": true,
        "tags": ["prod", "llm"]
    });

    let payload: UpstreamPayload = serde_json::from_value(json).unwrap();
    let provisioned = payload.into_provisioned(None);

    assert_eq!(provisioned.tenant_id, tenant);
    let req = &provisioned.request;
    assert_eq!(req.server.endpoints.len(), 2);
    assert_eq!(req.server.endpoints[0].scheme, domain::Scheme::Https);
    assert_eq!(req.server.endpoints[0].host, "api.openai.com");
    assert_eq!(req.server.endpoints[0].port, 443);
    assert_eq!(req.server.endpoints[1].scheme, domain::Scheme::Http);
    assert_eq!(
        req.protocol,
        "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1"
    );
    assert_eq!(req.alias.as_deref(), Some("openai"));
    assert!(req.enabled);
    assert_eq!(req.tags, vec!["prod", "llm"]);

    let auth = req.auth.as_ref().unwrap();
    assert_eq!(auth.plugin_type, "apikey");
    assert_eq!(auth.sharing, domain::SharingMode::Private);
    let config = auth.config.as_ref().unwrap();
    assert_eq!(config.get("header").unwrap(), "authorization");
    assert_eq!(config.get("secret_ref").unwrap(), "cred://key");

    let headers = req.headers.as_ref().unwrap();
    let rr = headers.request.as_ref().unwrap();
    assert_eq!(rr.set.get("x-custom").unwrap(), "value");
    assert_eq!(rr.passthrough, domain::PassthroughMode::All);
}

#[test]
fn deserialize_valid_route_payload() {
    let tenant = Uuid::new_v4();
    let upstream_id = Uuid::new_v4();
    let json = serde_json::json!({
        "tenant_id": tenant,
        "upstream_id": upstream_id,
        "match": {
            "http": {
                "methods": ["POST", "PUT"],
                "path": "/v1/chat/completions",
                "query_allowlist": ["model"],
                "path_suffix_mode": "disabled"
            }
        },
        "plugins": {
            "sharing": "inherit",
            "items": [{"plugin_ref": "plugin-a"}]
        },
        "rate_limit": {
            "sustained": {"rate": 100, "window": "minute"},
            "burst": {"capacity": 20},
            "scope": "tenant",
            "strategy": "reject",
            "cost": 2
        },
        "tags": ["chat"],
        "priority": 10,
        "enabled": true
    });

    let payload: RoutePayload = serde_json::from_value(json).unwrap();
    let provisioned: ProvisionedRoute = payload.into();

    assert_eq!(provisioned.tenant_id, tenant);
    let req = &provisioned.request;
    assert_eq!(req.upstream_id, upstream_id);
    assert_eq!(req.priority, 10);
    assert!(req.enabled);
    assert_eq!(req.tags, vec!["chat"]);

    let http = req.match_rules.http.as_ref().unwrap();
    assert_eq!(
        http.methods,
        vec![domain::HttpMethod::Post, domain::HttpMethod::Put]
    );
    assert_eq!(http.path, "/v1/chat/completions");
    assert_eq!(http.query_allowlist, vec!["model"]);
    assert_eq!(http.path_suffix_mode, domain::PathSuffixMode::Disabled);

    let plugins = req.plugins.as_ref().unwrap();
    assert_eq!(plugins.sharing, domain::SharingMode::Inherit);
    assert_eq!(plugins.items.len(), 1);
    assert_eq!(plugins.items[0].plugin_ref, "plugin-a");

    let rl = req.rate_limit.as_ref().unwrap();
    assert_eq!(rl.sustained.rate, 100);
    assert_eq!(rl.sustained.window, domain::Window::Minute);
    assert_eq!(rl.burst.as_ref().unwrap().capacity, 20);
    assert_eq!(rl.scope, domain::RateLimitScope::Tenant);
    assert_eq!(rl.strategy, domain::RateLimitStrategy::Reject);
    assert_eq!(rl.cost, 2);
}

#[test]
fn deserialize_missing_field_returns_error() {
    // Missing required "server" field.
    let json = serde_json::json!({
        "tenant_id": Uuid::new_v4(),
        "protocol": "http"
    });
    let result = serde_json::from_value::<UpstreamPayload>(json);
    assert!(result.is_err());
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.contains("server"),
        "error should name the missing field: {msg}"
    );
}

#[test]
fn deserialize_unknown_scheme_returns_error() {
    let json = serde_json::json!({
        "tenant_id": Uuid::new_v4(),
        "server": {
            "endpoints": [{"scheme": "ftp", "host": "files.example.com", "port": 21}]
        },
        "protocol": "http"
    });
    let result = serde_json::from_value::<UpstreamPayload>(json);
    assert!(result.is_err());
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.to_lowercase().contains("ftp")
            || msg.contains("scheme")
            || msg.contains("unknown variant"),
        "error should be actionable about the bad scheme: {msg}"
    );
}
