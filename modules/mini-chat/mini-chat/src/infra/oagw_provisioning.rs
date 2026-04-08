//! OAGW upstream and route registration for configured LLM providers.
//!
//! Called once during `Module::init()` to ensure every provider entry
//! has a corresponding OAGW upstream (with auth config) and route.
//!
//! After each successful `create_upstream`, the OAGW-assigned alias is
//! stamped onto [`ProviderEntry::upstream_alias`] (or
//! [`ProviderTenantOverride::upstream_alias`]) so the rest of mini-chat uses
//! the authoritative alias from OAGW rather than deriving one locally.

use std::collections::HashMap;
use std::sync::Arc;

use oagw_sdk::ServiceGatewayClientV1;
use tracing::{info, warn};

use crate::config::ProviderEntry;

/// Register OAGW upstreams and routes for each configured provider.
///
/// On success the **OAGW-assigned alias** is written into
/// [`ProviderEntry::upstream_alias`] (root) and
/// [`ProviderTenantOverride::upstream_alias`] (per-tenant).
///
/// The caller is responsible for obtaining a valid `SecurityContext`
/// (typically via S2S client credentials exchange).
pub async fn register_oagw_upstreams(
    gateway: &Arc<dyn ServiceGatewayClientV1>,
    ctx: &modkit_security::SecurityContext,
    providers: &mut HashMap<String, ProviderEntry>,
) -> anyhow::Result<()> {
    for (provider_id, entry) in providers.iter_mut() {
        // Register root upstream + route. Fail hard — without upstreams the
        // module cannot proxy LLM requests.
        let upstream = create_upstream(gateway, ctx, provider_id, entry)
            .await
            .ok_or_else(|| {
                anyhow::anyhow!("OAGW upstream registration failed for provider '{provider_id}'")
            })?;
        entry.upstream_alias = Some(upstream.alias.clone());
        register_route(gateway, ctx, provider_id, entry, &upstream)
            .await
            .map_err(|e| {
                anyhow::anyhow!("OAGW route registration failed for provider '{provider_id}': {e}")
            })?;

        // Register tenant-specific upstreams (share the same route/api_path).
        let tenant_ids: Vec<String> = entry.tenant_overrides.keys().cloned().collect();
        for tenant_id in &tenant_ids {
            let tenant_override = &entry.tenant_overrides[tenant_id];
            if tenant_override.host.is_none() && tenant_override.upstream_alias.is_none() {
                anyhow::bail!(
                    "provider '{provider_id}': tenant override '{tenant_id}' \
                     has no host and no upstream_alias - \
                     cannot create distinct upstream"
                );
            }

            let label = format!("{provider_id}[tenant={tenant_id}]");
            let alias = create_tenant_upstream(gateway, ctx, &label, entry, tenant_id)
                .await
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "OAGW tenant upstream registration failed for provider '{provider_id}', tenant '{tenant_id}'"
                    )
                })?;
            if let Some(tenant_override) = entry.tenant_overrides.get_mut(tenant_id) {
                tenant_override.upstream_alias = Some(alias);
            }
        }
    }

    Ok(())
}

/// Create an OAGW upstream for a single provider entry.
///
/// Only passes `upstream_alias` to OAGW when explicitly configured
/// (required for IP-based hosts). For hostname-based hosts OAGW
/// auto-derives the alias.
///
/// Returns `None` (with a warning log) if registration fails.
fn endpoint_for(entry: &ProviderEntry) -> oagw_sdk::Endpoint {
    use oagw_sdk::{Endpoint, Scheme};
    let scheme = if entry.use_http {
        Scheme::Http
    } else {
        Scheme::Https
    };
    let port = entry.port.unwrap_or(if entry.use_http { 80 } else { 443 });
    Endpoint {
        scheme,
        host: entry.host.clone(),
        port,
    }
}

async fn create_upstream(
    gateway: &Arc<dyn ServiceGatewayClientV1>,
    ctx: &modkit_security::SecurityContext,
    provider_id: &str,
    entry: &ProviderEntry,
) -> Option<oagw_sdk::Upstream> {
    use oagw_sdk::{AuthConfig, CreateUpstreamRequest, Server};

    let server = Server {
        endpoints: vec![endpoint_for(entry)],
    };

    let mut builder =
        CreateUpstreamRequest::builder(server, "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1")
            .enabled(true);

    // Only pass alias when explicitly configured (IP-based hosts).
    if let Some(alias) = &entry.upstream_alias {
        builder = builder.alias(alias);
    }

    if let (Some(plugin_type), Some(config)) = (&entry.auth_plugin_type, &entry.auth_config) {
        builder = builder.auth(AuthConfig {
            plugin_type: plugin_type.clone(),
            sharing: oagw_sdk::SharingMode::Inherit,
            config: Some(config.clone()),
        });
    }

    match gateway.create_upstream(ctx.clone(), builder.build()).await {
        Ok(u) => {
            info!(
                provider_id,
                alias = %u.alias,
                upstream_id = %u.id,
                "OAGW upstream registered"
            );
            Some(u)
        }
        Err(e) => {
            warn!(
                provider_id,
                error = %e,
                "OAGW upstream registration failed (may already exist)"
            );
            None
        }
    }
}

/// Create an OAGW upstream for a tenant-specific override.
///
/// Uses [`ProviderEntry::effective_host_for_tenant`] and the tenant's auth
/// config. Only passes `upstream_alias` when the tenant override explicitly
/// sets one (required for IP-based hosts). For hostname-based hosts OAGW
/// auto-derives the alias.
///
/// Returns the OAGW-assigned alias on success, `None` on failure.
async fn create_tenant_upstream(
    gateway: &Arc<dyn ServiceGatewayClientV1>,
    ctx: &modkit_security::SecurityContext,
    label: &str,
    entry: &ProviderEntry,
    tenant_id: &str,
) -> Option<String> {
    use oagw_sdk::{AuthConfig, CreateUpstreamRequest, Server};

    let host = entry.effective_host_for_tenant(tenant_id);

    // Inherit scheme/port from root entry (tenant override only changes host/auth).
    let mut ep = endpoint_for(entry);
    host.clone_into(&mut ep.host);

    let server = Server {
        endpoints: vec![ep],
    };

    let mut builder =
        CreateUpstreamRequest::builder(server, "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1")
            .enabled(true);

    // Only pass alias when the tenant override explicitly sets one (IP-based hosts).
    if let Some(alias) = entry
        .tenant_overrides
        .get(tenant_id)
        .and_then(|o| o.upstream_alias.as_deref())
    {
        builder = builder.alias(alias);
    }

    if let (Some(plugin_type), Some(config)) = (
        entry.effective_auth_plugin_type_for_tenant(tenant_id),
        entry.effective_auth_config_for_tenant(tenant_id),
    ) {
        builder = builder.auth(AuthConfig {
            plugin_type: plugin_type.to_owned(),
            sharing: oagw_sdk::SharingMode::Inherit,
            config: Some(config.clone()),
        });
    }

    match gateway.create_upstream(ctx.clone(), builder.build()).await {
        Ok(u) => {
            info!(
                label,
                alias = %u.alias,
                upstream_id = %u.id,
                "OAGW tenant upstream registered"
            );
            Some(u.alias)
        }
        Err(e) => {
            warn!(
                label,
                error = %e,
                "OAGW tenant upstream registration failed (may already exist)"
            );
            None
        }
    }
}

/// Derive route match rules from `api_path` and register the OAGW route.
///
/// Tenant-specific upstreams do NOT need separate routes — OAGW's route
/// resolution falls back to ancestor upstream IDs, so tenant upstreams
/// inherit routes from the root upstream automatically.
async fn register_route(
    gateway: &Arc<dyn ServiceGatewayClientV1>,
    ctx: &modkit_security::SecurityContext,
    provider_id: &str,
    entry: &ProviderEntry,
    upstream: &oagw_sdk::Upstream,
) -> anyhow::Result<()> {
    use oagw_sdk::{CreateRouteRequest, HttpMatch, HttpMethod, MatchRules};

    let (route_prefix, suffix_mode) = derive_route_match(&entry.api_path);
    let query_allowlist = extract_query_allowlist(&entry.api_path);

    let match_rules = MatchRules {
        http: Some(HttpMatch {
            methods: vec![HttpMethod::Post],
            path: route_prefix.clone(),
            query_allowlist,
            path_suffix_mode: suffix_mode,
        }),
        grpc: None,
    };

    let route = gateway
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(upstream.id, match_rules)
                .enabled(true)
                .build(),
        )
        .await?;

    info!(
        provider_id,
        route_id = %route.id,
        route_path = %route_prefix,
        "OAGW route registered"
    );

    // Register RAG-related routes (Files API, Vector Stores API) on the same upstream.
    register_rag_routes(gateway, ctx, provider_id, entry, upstream).await?;

    Ok(())
}

/// RAG route definitions: method, path suffix (appended to RAG prefix), suffix mode.
///
/// Note: POST `vector_stores` uses suffix=true to cover both the create
/// endpoint (exact path) and the add-file-to-VS endpoint ({id}/files).
/// Having two routes with the same method+path but different suffix modes
/// causes OAGW to pick the first registered one, blocking the suffix path.
const RAG_ROUTES: &[(&str, &str, bool)] = &[
    // POST {prefix}/files — upload file to provider
    ("POST", "/files", false),
    // DELETE {prefix}/files/{file_id} — delete provider file
    ("DELETE", "/files", true),
    // POST {prefix}/vector_stores — create vector store (exact)
    // POST {prefix}/vector_stores/{id}/files — add file to vector store (suffix)
    // Single route with suffix=true handles both paths.
    ("POST", "/vector_stores", true),
    // DELETE {prefix}/vector_stores/{vs_id}/files/{file_id} — remove file from vector store
    ("DELETE", "/vector_stores", true),
];

/// Register OAGW routes for RAG operations (Files API, Vector Stores API).
#[allow(clippy::cognitive_complexity)]
async fn register_rag_routes(
    gateway: &Arc<dyn ServiceGatewayClientV1>,
    ctx: &modkit_security::SecurityContext,
    provider_id: &str,
    entry: &ProviderEntry,
    upstream: &oagw_sdk::Upstream,
) -> anyhow::Result<()> {
    use oagw_sdk::{CreateRouteRequest, HttpMatch, HttpMethod, MatchRules, PathSuffixMode};

    // Derive RAG path prefix from storage_kind:
    // Azure → /openai (+ api-version query param), OpenAi → /v1
    let (prefix, query_allowlist) = match entry.storage_kind {
        crate::config::StorageKind::Azure => ("/openai", vec!["api-version".to_owned()]),
        crate::config::StorageKind::OpenAi => ("/v1", vec![]),
    };

    for &(method_str, path_suffix, append_suffix) in RAG_ROUTES {
        let method = match method_str {
            "POST" => HttpMethod::Post,
            "DELETE" => HttpMethod::Delete,
            _ => continue,
        };

        let suffix_mode = if append_suffix {
            PathSuffixMode::Append
        } else {
            PathSuffixMode::Disabled
        };

        let full_path = format!("{prefix}{path_suffix}");

        let match_rules = MatchRules {
            http: Some(HttpMatch {
                methods: vec![method],
                path: full_path.clone(),
                query_allowlist: query_allowlist.clone(),
                path_suffix_mode: suffix_mode,
            }),
            grpc: None,
        };

        match gateway
            .create_route(
                ctx.clone(),
                CreateRouteRequest::builder(upstream.id, match_rules)
                    .enabled(true)
                    .build(),
            )
            .await
        {
            Ok(route) => {
                info!(
                    provider_id,
                    route_id = %route.id,
                    route_path = %full_path,
                    method = method_str,
                    "OAGW RAG route registered"
                );
            }
            Err(e) => {
                warn!(
                    provider_id,
                    error = %e,
                    route_path = %full_path,
                    method = method_str,
                    "OAGW RAG route registration failed (may already exist)"
                );
            }
        }
    }

    Ok(())
}

/// Derive route prefix and suffix mode from an `api_path` template.
///
/// Strips query string, replaces `{model}` with `*`, and returns
/// `(prefix, suffix_mode)` for OAGW route matching.
fn derive_route_match(api_path: &str) -> (String, oagw_sdk::PathSuffixMode) {
    let route_path = api_path
        .split('?')
        .next()
        .unwrap_or(api_path)
        .replace("{model}", "*");

    let route_prefix = if let Some(pos) = route_path.find('*') {
        route_path[..pos].trim_end_matches('/').to_owned()
    } else {
        route_path.clone()
    };

    let suffix_mode = if route_path.contains('*') {
        oagw_sdk::PathSuffixMode::Append
    } else {
        oagw_sdk::PathSuffixMode::Disabled
    };

    (route_prefix, suffix_mode)
}

/// Extract query parameter names from an `api_path` template's query string.
fn extract_query_allowlist(api_path: &str) -> Vec<String> {
    api_path
        .split('?')
        .nth(1)
        .map(|qs| {
            qs.split('&')
                .filter_map(|pair| pair.split('=').next().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_simple_path() {
        let (prefix, mode) = derive_route_match("/v1/responses");
        assert_eq!(prefix, "/v1/responses");
        assert!(matches!(mode, oagw_sdk::PathSuffixMode::Disabled));
    }

    #[test]
    fn derive_path_with_model_placeholder() {
        let (prefix, mode) =
            derive_route_match("/openai/deployments/{model}/responses?api-version=2025-03-01");
        assert_eq!(prefix, "/openai/deployments");
        assert!(matches!(mode, oagw_sdk::PathSuffixMode::Append));
    }

    #[test]
    fn derive_azure_openai_path() {
        let (prefix, mode) = derive_route_match("/openai/v1/responses");
        assert_eq!(prefix, "/openai/v1/responses");
        assert!(matches!(mode, oagw_sdk::PathSuffixMode::Disabled));
    }

    #[test]
    fn extract_empty_query() {
        assert!(extract_query_allowlist("/v1/responses").is_empty());
    }

    #[test]
    fn extract_single_query_param() {
        let params =
            extract_query_allowlist("/openai/deployments/{model}/responses?api-version=2025-03-01");
        assert_eq!(params, vec!["api-version"]);
    }

    #[test]
    fn extract_multiple_query_params() {
        let params = extract_query_allowlist("/path?foo=1&bar=2&baz=3");
        assert_eq!(params, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn derive_trailing_wildcard_strips_trailing_slash() {
        let (prefix, mode) = derive_route_match("/v1/models/*/completions");
        assert_eq!(prefix, "/v1/models");
        assert!(matches!(mode, oagw_sdk::PathSuffixMode::Append));
    }

    #[test]
    fn derive_root_path() {
        let (prefix, mode) = derive_route_match("/");
        assert_eq!(prefix, "/");
        assert!(matches!(mode, oagw_sdk::PathSuffixMode::Disabled));
    }

    #[test]
    fn derive_query_string_stripped_before_matching() {
        // Query params should not affect route prefix or suffix mode.
        let (prefix, mode) = derive_route_match("/v1/responses?stream=true");
        assert_eq!(prefix, "/v1/responses");
        assert!(matches!(mode, oagw_sdk::PathSuffixMode::Disabled));
    }

    #[test]
    fn extract_query_params_with_empty_values() {
        let params = extract_query_allowlist("/path?key=&other=val");
        assert_eq!(params, vec!["key", "other"]);
    }
}
