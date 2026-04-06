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
}
#[cfg(test)]
#[path = "provider_resolver_tests.rs"]
mod tests;
