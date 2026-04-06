use super::*;
use crate::config::StorageKind;
use oagw_sdk::error::ServiceGatewayError;

impl ProviderResolver {
    pub fn single_provider(provider: Arc<dyn LlmProvider>) -> Self {
        let kind = ProviderKind::OpenAiResponses;
        let mut adapters = HashMap::new();
        adapters.insert(kind, provider);
        let mut registry = HashMap::new();
        registry.insert(
            "openai".to_owned(),
            ProviderEntry {
                kind,
                upstream_alias: Some("test-host".to_owned()),
                host: "test-host".to_owned(),
                port: None,
                use_http: false,
                api_path: "/v1/responses".to_owned(),
                auth_plugin_type: None,
                auth_config: None,
                storage_backend: None,
                supports_file_search_filters: true,
                storage_kind: crate::config::StorageKind::OpenAi,
                api_version: None,
                tenant_overrides: HashMap::new(),
            },
        );
        Self { adapters, registry }
    }
}

/// Minimal no-op gateway for tests that only need `Arc<dyn ServiceGatewayClientV1>`.
struct NullGateway;

#[async_trait::async_trait]
impl ServiceGatewayClientV1 for NullGateway {
    async fn create_upstream(
        &self,
        _: modkit_security::SecurityContext,
        _: oagw_sdk::CreateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, ServiceGatewayError> {
        unimplemented!()
    }
    async fn get_upstream(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
    ) -> Result<oagw_sdk::Upstream, ServiceGatewayError> {
        unimplemented!()
    }
    async fn list_upstreams(
        &self,
        _: modkit_security::SecurityContext,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Upstream>, ServiceGatewayError> {
        unimplemented!()
    }
    async fn update_upstream(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
        _: oagw_sdk::UpdateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, ServiceGatewayError> {
        unimplemented!()
    }
    async fn delete_upstream(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
    ) -> Result<(), ServiceGatewayError> {
        unimplemented!()
    }
    async fn create_route(
        &self,
        _: modkit_security::SecurityContext,
        _: oagw_sdk::CreateRouteRequest,
    ) -> Result<oagw_sdk::Route, ServiceGatewayError> {
        unimplemented!()
    }
    async fn get_route(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
    ) -> Result<oagw_sdk::Route, ServiceGatewayError> {
        unimplemented!()
    }
    async fn list_routes(
        &self,
        _: modkit_security::SecurityContext,
        _: Option<uuid::Uuid>,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Route>, ServiceGatewayError> {
        unimplemented!()
    }
    async fn update_route(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
        _: oagw_sdk::UpdateRouteRequest,
    ) -> Result<oagw_sdk::Route, ServiceGatewayError> {
        unimplemented!()
    }
    async fn delete_route(
        &self,
        _: modkit_security::SecurityContext,
        _: uuid::Uuid,
    ) -> Result<(), ServiceGatewayError> {
        unimplemented!()
    }
    async fn resolve_proxy_target(
        &self,
        _: modkit_security::SecurityContext,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), ServiceGatewayError> {
        unimplemented!()
    }
    async fn proxy_request(
        &self,
        _: modkit_security::SecurityContext,
        _: http::Request<oagw_sdk::Body>,
    ) -> Result<http::Response<oagw_sdk::Body>, ServiceGatewayError> {
        unimplemented!()
    }
}

fn null_gw() -> Arc<dyn ServiceGatewayClientV1> {
    Arc::new(NullGateway)
}

fn mock_providers() -> HashMap<String, ProviderEntry> {
    let mut m = HashMap::new();
    m.insert(
        "openai".to_owned(),
        ProviderEntry {
            kind: ProviderKind::OpenAiResponses,
            upstream_alias: Some("api.openai.com".to_owned()),
            host: "api.openai.com".to_owned(),
            port: None,
            use_http: false,
            api_path: "/v1/responses".to_owned(),
            auth_plugin_type: None,
            auth_config: None,
            storage_backend: None,
            supports_file_search_filters: true,
            storage_kind: StorageKind::OpenAi,
            api_version: None,
            tenant_overrides: HashMap::new(),
        },
    );
    m.insert(
        "azure_openai".to_owned(),
        ProviderEntry {
            kind: ProviderKind::OpenAiResponses,
            upstream_alias: Some("my-azure.openai.azure.com".to_owned()),
            host: "my-azure.openai.azure.com".to_owned(),
            port: None,
            use_http: false,
            api_path: "/openai/v1/responses".to_owned(),
            auth_plugin_type: None,
            auth_config: None,
            storage_backend: Some("azure".to_owned()),
            supports_file_search_filters: false,
            storage_kind: StorageKind::Azure,
            api_version: Some("2024-10-21".to_owned()),
            tenant_overrides: HashMap::new(),
        },
    );
    m
}

#[test]
fn resolve_openai() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers());
    let r = resolver.resolve("openai", None).unwrap();
    assert_eq!(r.upstream_alias, "api.openai.com");
    assert_eq!(r.api_path, "/v1/responses");
}

#[test]
fn resolve_azure() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers());
    let r = resolver.resolve("azure_openai", None).unwrap();
    assert_eq!(r.upstream_alias, "my-azure.openai.azure.com");
    assert_eq!(r.api_path, "/openai/v1/responses");
}

#[test]
fn resolve_unknown_fails() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers());
    let result = resolver.resolve("anthropic", None);
    assert!(result.is_err());
}

#[test]
fn same_kind_shares_adapter() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers());
    let r1 = resolver.resolve("openai", None).unwrap();
    let r2 = resolver.resolve("azure_openai", None).unwrap();
    assert!(Arc::ptr_eq(&r1.adapter, &r2.adapter));
}

fn mock_providers_with_tenant_overrides() -> HashMap<String, ProviderEntry> {
    use crate::config::ProviderTenantOverride;
    let mut m = HashMap::new();
    m.insert(
        "azure_openai".to_owned(),
        ProviderEntry {
            kind: ProviderKind::OpenAiResponses,
            upstream_alias: Some("default.openai.azure.com".to_owned()),
            host: "default.openai.azure.com".to_owned(),
            port: None,
            use_http: false,
            api_path: "/openai/v1/responses".to_owned(),
            auth_plugin_type: None,
            auth_config: None,
            storage_backend: None,
            supports_file_search_filters: true,
            storage_kind: StorageKind::Azure,
            api_version: Some("2024-10-21".to_owned()),
            tenant_overrides: {
                let mut t = HashMap::new();
                t.insert(
                    "tenant-a".to_owned(),
                    ProviderTenantOverride {
                        host: Some("tenant-a.openai.azure.com".to_owned()),
                        upstream_alias: Some("tenant-a.openai.azure.com".to_owned()),
                        auth_plugin_type: None,
                        auth_config: None,
                    },
                );
                t.insert(
                    "tenant-b".to_owned(),
                    ProviderTenantOverride {
                        host: None,
                        // No upstream_alias — auth-only override, falls back to root.
                        upstream_alias: None,
                        auth_plugin_type: Some("custom-plugin".to_owned()),
                        auth_config: None,
                    },
                );
                t
            },
        },
    );
    m
}

#[test]
fn resolve_with_tenant_override_host() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers_with_tenant_overrides());
    let r = resolver.resolve("azure_openai", Some("tenant-a")).unwrap();
    assert_eq!(r.upstream_alias, "tenant-a.openai.azure.com");
    assert_eq!(r.api_path, "/openai/v1/responses");
}

#[test]
fn resolve_with_tenant_override_no_host_falls_back() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers_with_tenant_overrides());
    // tenant-b has auth override but no host override → no separate
    // upstream was created, so resolver falls back to root alias.
    let r = resolver.resolve("azure_openai", Some("tenant-b")).unwrap();
    assert_eq!(r.upstream_alias, "default.openai.azure.com");
}

#[test]
fn resolve_with_unknown_tenant_falls_back_to_root() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers_with_tenant_overrides());
    let r = resolver
        .resolve("azure_openai", Some("unknown-tenant"))
        .unwrap();
    assert_eq!(r.upstream_alias, "default.openai.azure.com");
}

#[test]
fn resolve_with_none_tenant_uses_root() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers_with_tenant_overrides());
    let r = resolver.resolve("azure_openai", None).unwrap();
    assert_eq!(r.upstream_alias, "default.openai.azure.com");
}

// ── P5-K6: Azure degrades filtered to unrestricted ──

#[test]
fn openai_supports_file_search_filters() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers());
    assert!(resolver.supports_file_search_filters("openai"));
}

#[test]
fn azure_does_not_support_file_search_filters() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers());
    assert!(!resolver.supports_file_search_filters("azure_openai"));
}

#[test]
fn unknown_provider_does_not_support_filters() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers());
    assert!(!resolver.supports_file_search_filters("nonexistent"));
}

// ── WS1: Config-driven resolve_storage_backend ──

#[test]
fn resolve_storage_backend_uses_config_field() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers());
    // azure_openai has storage_backend: Some("azure") in mock_providers
    assert_eq!(resolver.resolve_storage_backend("azure_openai"), "azure");
}

#[test]
fn resolve_storage_backend_falls_back_to_provider_id() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers());
    // openai has storage_backend: None → falls back to "openai"
    assert_eq!(resolver.resolve_storage_backend("openai"), "openai");
}

#[test]
fn resolve_storage_backend_unknown_provider_returns_id() {
    let resolver = ProviderResolver::new(&null_gw(), mock_providers());
    // Unknown provider not in registry → falls back to the provided string
    assert_eq!(resolver.resolve_storage_backend("unknown"), "unknown");
}

// ── WS1: Config-driven supports_file_search_filters ──

#[test]
fn supports_file_search_filters_uses_config_field_not_host() {
    // Create a provider with an Azure-like host but filters enabled
    let mut m = HashMap::new();
    m.insert(
        "custom_azure".to_owned(),
        ProviderEntry {
            kind: ProviderKind::OpenAiResponses,
            upstream_alias: Some("custom.azure.com".to_owned()),
            host: "custom.azure.com".to_owned(),
            port: None,
            use_http: false,
            api_path: "/v1/responses".to_owned(),
            auth_plugin_type: None,
            auth_config: None,
            storage_backend: None,
            supports_file_search_filters: true,
            storage_kind: StorageKind::Azure,
            api_version: Some("2024-10-21".to_owned()),
            tenant_overrides: HashMap::new(),
        },
    );
    let resolver = ProviderResolver::new(&null_gw(), m);
    // Despite .azure.com host, config says true
    assert!(resolver.supports_file_search_filters("custom_azure"));
}

// ── WS1: Deserialization backward compatibility ──

#[test]
fn provider_entry_deserialize_omitted_fields_default_correctly() {
    let json = serde_json::json!({
        "kind": "openai_responses",
        "storage_kind": "openai",
        "host": "api.openai.com",
        "api_path": "/v1/responses"
    });
    let entry: ProviderEntry = serde_json::from_value(json).unwrap();
    assert!(entry.storage_backend.is_none());
    assert!(entry.supports_file_search_filters);
    assert_eq!(entry.storage_kind, StorageKind::OpenAi);
}

#[test]
fn provider_entry_deserialize_explicit_values() {
    let json = serde_json::json!({
        "kind": "openai_responses",
        "storage_kind": "azure",
        "host": "my-azure.openai.azure.com",
        "api_path": "/v1/responses",
        "storage_backend": "azure",
        "supports_file_search_filters": false
    });
    let entry: ProviderEntry = serde_json::from_value(json).unwrap();
    assert_eq!(entry.storage_backend.as_deref(), Some("azure"));
    assert!(!entry.supports_file_search_filters);
    assert_eq!(entry.storage_kind, StorageKind::Azure);
}

#[test]
fn provider_entry_deserialize_missing_storage_kind_rejected() {
    let json = serde_json::json!({
        "kind": "openai_responses",
        "host": "api.openai.com"
    });
    let result: Result<ProviderEntry, _> = serde_json::from_value(json);
    assert!(result.is_err(), "missing storage_kind should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("storage_kind"),
        "error should mention storage_kind: {err}"
    );
}
