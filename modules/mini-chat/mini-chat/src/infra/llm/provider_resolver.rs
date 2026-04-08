//! Runtime resolution of LLM provider adapter + OAGW upstream alias.
//!
//! Built once at module startup from `MiniChatConfig.providers` after
//! OAGW upstream registration has stamped `upstream_alias` on each
//! [`ProviderEntry`] and [`ProviderTenantOverride`].
//!
//! Used per turn to resolve which adapter and OAGW alias to use
//! based on the model's `provider_id`.

use std::collections::HashMap;
use std::sync::Arc;

use oagw_sdk::ServiceGatewayClientV1;

use super::providers::{ProviderKind, create_provider};
use super::{LlmProvider, LlmProviderError};
use crate::config::ProviderEntry;
#[cfg(test)]
use crate::config::StorageKind;

/// Result of resolving a `provider_id`.
pub struct ResolvedProvider<'a> {
    pub adapter: Arc<dyn LlmProvider>,
    pub upstream_alias: &'a str,
    /// API path template (may contain `{model}` placeholder).
    pub api_path: &'a str,
}

/// Resolves `(provider adapter, upstream alias)` from a `provider_id`.
///
/// Upstream aliases are read from [`ProviderEntry::upstream_alias`] and
/// [`ProviderTenantOverride::upstream_alias`], which are set by OAGW at startup.
pub struct ProviderResolver {
    /// One adapter per distinct `ProviderKind`.
    adapters: HashMap<ProviderKind, Arc<dyn LlmProvider>>,
    /// `provider_id` → `ProviderEntry` from config (with `upstream_alias` set).
    registry: HashMap<String, ProviderEntry>,
}

impl ProviderResolver {
    /// Build from config + OAGW gateway. Creates one adapter per distinct
    /// `ProviderKind` (not per `provider_id`).
    ///
    /// `providers` must have been passed through
    /// [`register_oagw_upstreams`](crate::infra::oagw_provisioning::register_oagw_upstreams)
    /// first so that `upstream_alias` is populated.
    pub fn new(
        gateway: &Arc<dyn ServiceGatewayClientV1>,
        providers: HashMap<String, ProviderEntry>,
    ) -> Self {
        let mut adapters = HashMap::new();
        for entry in providers.values() {
            adapters
                .entry(entry.kind)
                .or_insert_with(|| create_provider(Arc::clone(gateway), entry.kind));
        }
        Self {
            adapters,
            registry: providers,
        }
    }

    /// Resolve the provider adapter, upstream alias, and API path template
    /// for a `provider_id`.
    ///
    /// When `tenant_id` is provided and the tenant override has an
    /// `upstream_alias`, that alias is returned. Otherwise, the root
    /// `upstream_alias` is used.
    pub fn resolve(
        &self,
        provider_id: &str,
        tenant_id: Option<&str>,
    ) -> Result<ResolvedProvider<'_>, LlmProviderError> {
        let entry =
            self.registry
                .get(provider_id)
                .ok_or_else(|| LlmProviderError::ProviderError {
                    code: "configuration_error".to_owned(),
                    message: format!("unknown provider_id: {provider_id}"),
                    raw_detail: None,
                })?;

        let adapter =
            self.adapters
                .get(&entry.kind)
                .ok_or_else(|| LlmProviderError::ProviderError {
                    code: "configuration_error".to_owned(),
                    message: format!("no adapter for kind {:?}", entry.kind),
                    raw_detail: None,
                })?;

        // Tenant-specific upstream_alias first, then root upstream_alias.
        let upstream_alias = tenant_id
            .and_then(|tid| {
                entry
                    .tenant_overrides
                    .get(tid)
                    .and_then(|ovr| ovr.upstream_alias.as_deref())
            })
            .or(entry.upstream_alias.as_deref())
            .ok_or_else(|| LlmProviderError::ProviderError {
                code: "configuration_error".to_owned(),
                message: format!("no OAGW alias registered for provider '{provider_id}'"),
                raw_detail: None,
            })?;

        Ok(ResolvedProvider {
            adapter: Arc::clone(adapter),
            upstream_alias,
            api_path: &entry.api_path,
        })
    }

    /// Derive the `storage_backend` label from a provider ID.
    ///
    /// Returns `ProviderEntry.storage_backend` when configured, otherwise
    /// falls back to the `provider_id` as-is. Stored on each attachment row
    /// so cleanup workers know which provider API to target.
    #[must_use]
    pub fn resolve_storage_backend(&self, provider_id: &str) -> String {
        self.registry
            .get(provider_id)
            .and_then(|entry| entry.storage_backend.clone())
            .unwrap_or_else(|| provider_id.to_owned())
    }

    /// Resolve the upstream alias for a given provider and tenant.
    ///
    /// Returns `None` if no upstream alias is registered for the provider.
    #[must_use]
    pub fn upstream_alias_for(&self, provider_id: &str, tenant_id: Option<&str>) -> Option<&str> {
        let entry = self.registry.get(provider_id)?;
        tenant_id
            .and_then(|tid| {
                entry
                    .tenant_overrides
                    .get(tid)
                    .and_then(|ovr| ovr.upstream_alias.as_deref())
            })
            .or(entry.upstream_alias.as_deref())
    }

    /// Whether the provider supports `file_search` filters (metadata filtering).
    ///
    /// Azure `OpenAI` does NOT support filters — `FilteredByAttachmentIds` must
    /// be degraded to `UnrestrictedChatSearch` for Azure providers.
    /// Configured via `ProviderEntry.supports_file_search_filters` (default `true`).
    #[must_use]
    pub fn supports_file_search_filters(&self, provider_id: &str) -> bool {
        self.registry
            .get(provider_id)
            .is_some_and(|entry| entry.supports_file_search_filters)
    }

    /// All registered provider entries (for startup validation / logging).
    #[must_use]
    pub fn entries(&self) -> &HashMap<String, ProviderEntry> {
        &self.registry
    }

    /// Create a resolver with a single pre-built provider adapter.
    /// Used in tests to wrap a mock `LlmProvider` without needing a gateway.
    #[cfg(test)]
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
                storage_kind: StorageKind::OpenAi,
                api_version: None,
                tenant_overrides: HashMap::new(),
            },
        );
        Self { adapters, registry }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oagw_sdk::error::ServiceGatewayError;

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
            _: uuid::Uuid,
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
}
