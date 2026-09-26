use std::sync::Arc;

use async_trait::async_trait;
use oagw_sdk::api::ServiceGatewayClientV1;
use oagw_sdk::body::Body;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{ControlPlaneService, DataPlaneService};
use crate::domain::model;

/// Facade that implements the public `ServiceGatewayClientV1` trait by
/// delegating to the internal CP and DP services.
#[domain_model]
pub(crate) struct ServiceGatewayClientV1Facade {
    cp: Arc<dyn ControlPlaneService>,
    dp: Arc<dyn DataPlaneService>,
}

impl ServiceGatewayClientV1Facade {
    pub(crate) fn new(cp: Arc<dyn ControlPlaneService>, dp: Arc<dyn DataPlaneService>) -> Self {
        Self { cp, dp }
    }
}

#[async_trait]
impl ServiceGatewayClientV1 for ServiceGatewayClientV1Facade {
    async fn create_upstream(
        &self,
        ctx: SecurityContext,
        req: oagw_sdk::CreateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        let internal_req = sdk_create_upstream_to_domain(req);
        self.cp
            .create_upstream(&ctx, internal_req)
            .await
            .map(upstream_to_sdk)
            .map_err(CanonicalError::from)
    }

    async fn get_upstream(
        &self,
        ctx: SecurityContext,
        id: Uuid,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        self.cp
            .get_upstream(&ctx, id)
            .await
            .map(upstream_to_sdk)
            .map_err(CanonicalError::from)
    }

    async fn list_upstreams(
        &self,
        ctx: SecurityContext,
        query: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Upstream>, CanonicalError> {
        let q = model::ListQuery {
            top: query.top,
            skip: query.skip,
        };
        self.cp
            .list_upstreams(&ctx, &q)
            .await
            .map(|v| v.into_iter().map(upstream_to_sdk).collect())
            .map_err(CanonicalError::from)
    }

    async fn update_upstream(
        &self,
        ctx: SecurityContext,
        id: Uuid,
        req: oagw_sdk::UpdateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        let internal_req = sdk_update_upstream_to_domain(req);
        self.cp
            .update_upstream(&ctx, id, internal_req)
            .await
            .map(upstream_to_sdk)
            .map_err(CanonicalError::from)
    }

    async fn delete_upstream(&self, ctx: SecurityContext, id: Uuid) -> Result<(), CanonicalError> {
        // Unlike the REST path, this facade holds no backend-selector handle and
        // cannot invalidate it: after delete `resolve_proxy_target` fails and a
        // re-created upstream gets a fresh selector under a new id, so only a
        // same-id re-create would observe stale backend health.
        let deleted_route_ids = self
            .cp
            .delete_upstream(&ctx, id)
            .await
            .map_err(CanonicalError::from)?;
        self.dp
            .remove_rate_limit_keys_for_upstream_cascade(id, &deleted_route_ids);
        Ok(())
    }

    async fn create_route(
        &self,
        ctx: SecurityContext,
        req: oagw_sdk::CreateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        let internal_req = sdk_create_route_to_domain(req);
        self.cp
            .create_route(&ctx, internal_req)
            .await
            .map(route_to_sdk)
            .map_err(CanonicalError::from)
    }

    async fn get_route(
        &self,
        ctx: SecurityContext,
        id: Uuid,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        self.cp
            .get_route(&ctx, id)
            .await
            .map(route_to_sdk)
            .map_err(CanonicalError::from)
    }

    async fn list_routes(
        &self,
        ctx: SecurityContext,
        upstream_id: Option<Uuid>,
        query: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Route>, CanonicalError> {
        let q = model::ListQuery {
            top: query.top,
            skip: query.skip,
        };
        self.cp
            .list_routes(&ctx, upstream_id, &q)
            .await
            .map(|v| v.into_iter().map(route_to_sdk).collect())
            .map_err(CanonicalError::from)
    }

    async fn update_route(
        &self,
        ctx: SecurityContext,
        id: Uuid,
        req: oagw_sdk::UpdateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        let internal_req = sdk_update_route_to_domain(req);
        self.cp
            .update_route(&ctx, id, internal_req)
            .await
            .map(route_to_sdk)
            .map_err(CanonicalError::from)
    }

    async fn delete_route(&self, ctx: SecurityContext, id: Uuid) -> Result<(), CanonicalError> {
        self.cp
            .delete_route(&ctx, id)
            .await
            .map_err(CanonicalError::from)?;
        self.dp.remove_rate_limit_keys_for_route(id);
        Ok(())
    }

    async fn resolve_proxy_target(
        &self,
        ctx: SecurityContext,
        alias: &str,
        method: &str,
        path: &str,
    ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), CanonicalError> {
        self.cp
            .resolve_proxy_target(&ctx, alias, method, path)
            .await
            .map(|(u, r)| (upstream_to_sdk(u), route_to_sdk(r)))
            .map_err(CanonicalError::from)
    }

    async fn proxy_request(
        &self,
        ctx: SecurityContext,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, CanonicalError> {
        self.dp
            .proxy_request(ctx, req)
            .await
            .map_err(CanonicalError::from)
    }
}

// ---------------------------------------------------------------------------
// SDK request → domain request conversions (using SDK getters for private fields)
//
// Domain errors are converted into the canonical `CanonicalError` by the impl
// crate's own `From<DomainError> for CanonicalError` in
// `crate::api::rest::error` — that is the **single** AIP-193 mapping. SDK
// consumers project the canonical error into the typed
// `ServiceGatewayError` view at the call site via
// `.map_err(ServiceGatewayError::from)`. See ADR 0005.
// ---------------------------------------------------------------------------

fn sdk_create_upstream_to_domain(
    req: oagw_sdk::CreateUpstreamRequest,
) -> model::CreateUpstreamRequest {
    model::CreateUpstreamRequest {
        id: None,
        server: server_to_domain(req.server().clone()),
        protocol: req.protocol().to_string(),
        alias: req.alias().map(|s| s.to_string()),
        auth: req.auth().cloned().map(auth_config_to_domain),
        headers: req.headers().cloned().map(headers_config_to_domain),
        plugins: req.plugins().cloned().map(plugins_config_to_domain),
        rate_limit: req.rate_limit().cloned().map(rate_limit_config_to_domain),
        cors: req.cors().cloned().map(cors_config_to_domain),
        tags: req.tags().to_vec(),
        enabled: req.enabled(),
    }
}

fn sdk_update_upstream_to_domain(
    req: oagw_sdk::UpdateUpstreamRequest,
) -> model::UpdateUpstreamRequest {
    model::UpdateUpstreamRequest {
        server: server_to_domain(req.server().clone()),
        protocol: req.protocol().to_string(),
        alias: req.alias().map(|s| s.to_string()),
        auth: req.auth().cloned().map(auth_config_to_domain),
        headers: req.headers().cloned().map(headers_config_to_domain),
        plugins: req.plugins().cloned().map(plugins_config_to_domain),
        rate_limit: req.rate_limit().cloned().map(rate_limit_config_to_domain),
        cors: req.cors().cloned().map(cors_config_to_domain),
        tags: req.tags().to_vec(),
        enabled: req.enabled(),
    }
}

fn sdk_create_route_to_domain(req: oagw_sdk::CreateRouteRequest) -> model::CreateRouteRequest {
    model::CreateRouteRequest {
        id: None,
        upstream_id: req.upstream_id(),
        match_rules: match_rules_to_domain(req.match_rules().clone()),
        plugins: req.plugins().cloned().map(plugins_config_to_domain),
        rate_limit: req.rate_limit().cloned().map(rate_limit_config_to_domain),
        cors: req.cors().cloned().map(cors_config_to_domain),
        tags: req.tags().to_vec(),
        priority: req.priority(),
        enabled: req.enabled(),
    }
}

fn sdk_update_route_to_domain(req: oagw_sdk::UpdateRouteRequest) -> model::UpdateRouteRequest {
    model::UpdateRouteRequest {
        match_rules: match_rules_to_domain(req.match_rules().clone()),
        plugins: req.plugins().cloned().map(plugins_config_to_domain),
        rate_limit: req.rate_limit().cloned().map(rate_limit_config_to_domain),
        cors: req.cors().cloned().map(cors_config_to_domain),
        tags: req.tags().to_vec(),
        priority: req.priority(),
        enabled: req.enabled(),
    }
}

// ---------------------------------------------------------------------------
// SDK value types → domain value types
// ---------------------------------------------------------------------------

fn sharing_mode_to_domain(v: oagw_sdk::SharingMode) -> model::SharingMode {
    match v {
        oagw_sdk::SharingMode::Private => model::SharingMode::Private,
        oagw_sdk::SharingMode::Inherit => model::SharingMode::Inherit,
        oagw_sdk::SharingMode::Enforce => model::SharingMode::Enforce,
    }
}

fn scheme_to_domain(v: oagw_sdk::Scheme) -> model::Scheme {
    match v {
        oagw_sdk::Scheme::Http => model::Scheme::Http,
        oagw_sdk::Scheme::Https => model::Scheme::Https,
        oagw_sdk::Scheme::Wss => model::Scheme::Wss,
        oagw_sdk::Scheme::Wt => model::Scheme::Wt,
        oagw_sdk::Scheme::Grpc => model::Scheme::Grpc,
    }
}

fn endpoint_to_domain(v: oagw_sdk::Endpoint) -> model::Endpoint {
    model::Endpoint {
        scheme: scheme_to_domain(v.scheme),
        host: v.host,
        port: v.port,
    }
}

fn server_to_domain(v: oagw_sdk::Server) -> model::Server {
    model::Server {
        endpoints: v.endpoints.into_iter().map(endpoint_to_domain).collect(),
    }
}

fn auth_config_to_domain(v: oagw_sdk::AuthConfig) -> model::AuthConfig {
    model::AuthConfig {
        plugin_type: v.plugin_type,
        sharing: sharing_mode_to_domain(v.sharing),
        config: v.config,
    }
}

fn passthrough_mode_to_domain(v: oagw_sdk::PassthroughMode) -> model::PassthroughMode {
    match v {
        oagw_sdk::PassthroughMode::None => model::PassthroughMode::None,
        oagw_sdk::PassthroughMode::Allowlist => model::PassthroughMode::Allowlist,
        oagw_sdk::PassthroughMode::All => model::PassthroughMode::All,
    }
}

fn request_header_rules_to_domain(v: oagw_sdk::RequestHeaderRules) -> model::RequestHeaderRules {
    model::RequestHeaderRules {
        set: v.set,
        add: v.add,
        remove: v.remove,
        passthrough: passthrough_mode_to_domain(v.passthrough),
        passthrough_allowlist: v.passthrough_allowlist,
    }
}

fn response_header_rules_to_domain(v: oagw_sdk::ResponseHeaderRules) -> model::ResponseHeaderRules {
    model::ResponseHeaderRules {
        set: v.set,
        add: v.add,
        remove: v.remove,
    }
}

fn headers_config_to_domain(v: oagw_sdk::HeadersConfig) -> model::HeadersConfig {
    model::HeadersConfig {
        request: v.request.map(request_header_rules_to_domain),
        response: v.response.map(response_header_rules_to_domain),
    }
}

fn window_to_domain(v: oagw_sdk::Window) -> model::Window {
    match v {
        oagw_sdk::Window::Second => model::Window::Second,
        oagw_sdk::Window::Minute => model::Window::Minute,
        oagw_sdk::Window::Hour => model::Window::Hour,
        oagw_sdk::Window::Day => model::Window::Day,
    }
}

fn rate_limit_config_to_domain(v: oagw_sdk::RateLimitConfig) -> model::RateLimitConfig {
    model::RateLimitConfig {
        sharing: sharing_mode_to_domain(v.sharing),
        algorithm: match v.algorithm {
            oagw_sdk::RateLimitAlgorithm::TokenBucket => model::RateLimitAlgorithm::TokenBucket,
            oagw_sdk::RateLimitAlgorithm::SlidingWindow => model::RateLimitAlgorithm::SlidingWindow,
        },
        sustained: model::SustainedRate {
            rate: v.sustained.rate,
            window: window_to_domain(v.sustained.window),
        },
        burst: v.burst.map(|b| model::BurstConfig {
            capacity: b.capacity,
        }),
        budget: v.budget.map(|b| model::BudgetConfig {
            mode: match b.mode {
                oagw_sdk::BudgetMode::Unlimited => model::BudgetMode::Unlimited,
                oagw_sdk::BudgetMode::Allocated => model::BudgetMode::Allocated,
                oagw_sdk::BudgetMode::Shared => model::BudgetMode::Shared,
            },
            total: b.total,
            overcommit_ratio: b.overcommit_ratio,
        }),
        scope: match v.scope {
            oagw_sdk::RateLimitScope::Global => model::RateLimitScope::Global,
            oagw_sdk::RateLimitScope::Tenant => model::RateLimitScope::Tenant,
            oagw_sdk::RateLimitScope::User => model::RateLimitScope::User,
            oagw_sdk::RateLimitScope::Ip => model::RateLimitScope::Ip,
            oagw_sdk::RateLimitScope::Route => model::RateLimitScope::Route,
        },
        strategy: match v.strategy {
            oagw_sdk::RateLimitStrategy::Reject => model::RateLimitStrategy::Reject,
            oagw_sdk::RateLimitStrategy::Queue => model::RateLimitStrategy::Queue,
            oagw_sdk::RateLimitStrategy::Degrade => model::RateLimitStrategy::Degrade,
        },
        cost: v.cost,
        response_headers: v.response_headers,
        pool_owner_id: None,
    }
}

fn plugin_binding_to_domain(v: oagw_sdk::PluginBinding) -> model::PluginBinding {
    model::PluginBinding {
        plugin_ref: v.plugin_ref,
        config: v.config,
    }
}

fn plugins_config_to_domain(v: oagw_sdk::PluginsConfig) -> model::PluginsConfig {
    model::PluginsConfig {
        sharing: sharing_mode_to_domain(v.sharing),
        items: v.items.into_iter().map(plugin_binding_to_domain).collect(),
    }
}

fn cors_http_method_to_domain(v: oagw_sdk::CorsHttpMethod) -> model::CorsHttpMethod {
    match v {
        oagw_sdk::CorsHttpMethod::Get => model::CorsHttpMethod::Get,
        oagw_sdk::CorsHttpMethod::Post => model::CorsHttpMethod::Post,
        oagw_sdk::CorsHttpMethod::Put => model::CorsHttpMethod::Put,
        oagw_sdk::CorsHttpMethod::Delete => model::CorsHttpMethod::Delete,
        oagw_sdk::CorsHttpMethod::Patch => model::CorsHttpMethod::Patch,
        oagw_sdk::CorsHttpMethod::Head => model::CorsHttpMethod::Head,
        oagw_sdk::CorsHttpMethod::Options => model::CorsHttpMethod::Options,
    }
}

fn cors_config_to_domain(v: oagw_sdk::CorsConfig) -> model::CorsConfig {
    model::CorsConfig {
        sharing: sharing_mode_to_domain(v.sharing),
        enabled: v.enabled,
        allowed_origins: v.allowed_origins,
        allowed_methods: v
            .allowed_methods
            .into_iter()
            .map(cors_http_method_to_domain)
            .collect(),
        expose_headers: v.expose_headers,
        allow_credentials: v.allow_credentials,
    }
}

fn http_method_to_domain(v: oagw_sdk::HttpMethod) -> model::HttpMethod {
    match v {
        oagw_sdk::HttpMethod::Get => model::HttpMethod::Get,
        oagw_sdk::HttpMethod::Post => model::HttpMethod::Post,
        oagw_sdk::HttpMethod::Put => model::HttpMethod::Put,
        oagw_sdk::HttpMethod::Delete => model::HttpMethod::Delete,
        oagw_sdk::HttpMethod::Patch => model::HttpMethod::Patch,
    }
}

fn http_match_to_domain(v: oagw_sdk::HttpMatch) -> model::HttpMatch {
    model::HttpMatch {
        methods: v.methods.into_iter().map(http_method_to_domain).collect(),
        path: v.path,
        query_allowlist: v.query_allowlist,
        path_suffix_mode: match v.path_suffix_mode {
            oagw_sdk::PathSuffixMode::Disabled => model::PathSuffixMode::Disabled,
            oagw_sdk::PathSuffixMode::Append => model::PathSuffixMode::Append,
        },
    }
}

fn grpc_match_to_domain(v: oagw_sdk::GrpcMatch) -> model::GrpcMatch {
    model::GrpcMatch {
        service: v.service,
        method: v.method,
    }
}

fn match_rules_to_domain(v: oagw_sdk::MatchRules) -> model::MatchRules {
    model::MatchRules {
        http: v.http.map(http_match_to_domain),
        grpc: v.grpc.map(grpc_match_to_domain),
    }
}

// ---------------------------------------------------------------------------
// domain value types → SDK value types
// ---------------------------------------------------------------------------

fn sharing_mode_to_sdk(v: model::SharingMode) -> oagw_sdk::SharingMode {
    match v {
        model::SharingMode::Private => oagw_sdk::SharingMode::Private,
        model::SharingMode::Inherit => oagw_sdk::SharingMode::Inherit,
        model::SharingMode::Enforce => oagw_sdk::SharingMode::Enforce,
    }
}

fn scheme_to_sdk(v: model::Scheme) -> oagw_sdk::Scheme {
    match v {
        model::Scheme::Http => oagw_sdk::Scheme::Http,
        model::Scheme::Https => oagw_sdk::Scheme::Https,
        model::Scheme::Wss => oagw_sdk::Scheme::Wss,
        model::Scheme::Wt => oagw_sdk::Scheme::Wt,
        model::Scheme::Grpc => oagw_sdk::Scheme::Grpc,
    }
}

fn upstream_to_sdk(u: model::Upstream) -> oagw_sdk::Upstream {
    oagw_sdk::Upstream {
        id: u.id,
        tenant_id: u.tenant_id,
        alias: u.alias,
        server: oagw_sdk::Server {
            endpoints: u
                .server
                .endpoints
                .into_iter()
                .map(|e| oagw_sdk::Endpoint {
                    scheme: scheme_to_sdk(e.scheme),
                    host: e.host,
                    port: e.port,
                })
                .collect(),
        },
        protocol: u.protocol,
        enabled: u.enabled,
        auth: u.auth.map(|a| oagw_sdk::AuthConfig {
            plugin_type: a.plugin_type,
            sharing: sharing_mode_to_sdk(a.sharing),
            config: a.config,
        }),
        headers: u.headers.map(|h| oagw_sdk::HeadersConfig {
            request: h.request.map(|r| oagw_sdk::RequestHeaderRules {
                set: r.set,
                add: r.add,
                remove: r.remove,
                passthrough: match r.passthrough {
                    model::PassthroughMode::None => oagw_sdk::PassthroughMode::None,
                    model::PassthroughMode::Allowlist => oagw_sdk::PassthroughMode::Allowlist,
                    model::PassthroughMode::All => oagw_sdk::PassthroughMode::All,
                },
                passthrough_allowlist: r.passthrough_allowlist,
            }),
            response: h.response.map(|r| oagw_sdk::ResponseHeaderRules {
                set: r.set,
                add: r.add,
                remove: r.remove,
            }),
        }),
        plugins: u.plugins.map(|p| oagw_sdk::PluginsConfig {
            sharing: sharing_mode_to_sdk(p.sharing),
            items: p
                .items
                .into_iter()
                .map(|b| oagw_sdk::PluginBinding {
                    plugin_ref: b.plugin_ref,
                    config: b.config,
                })
                .collect(),
        }),
        rate_limit: u.rate_limit.map(rate_limit_config_to_sdk),
        cors: u.cors.map(cors_config_to_sdk),
        tags: u.tags,
    }
}

fn route_to_sdk(r: model::Route) -> oagw_sdk::Route {
    oagw_sdk::Route {
        id: r.id,
        tenant_id: r.tenant_id,
        upstream_id: r.upstream_id,
        match_rules: oagw_sdk::MatchRules {
            http: r.match_rules.http.map(|h| oagw_sdk::HttpMatch {
                methods: h
                    .methods
                    .into_iter()
                    .map(|m| match m {
                        model::HttpMethod::Get => oagw_sdk::HttpMethod::Get,
                        model::HttpMethod::Post => oagw_sdk::HttpMethod::Post,
                        model::HttpMethod::Put => oagw_sdk::HttpMethod::Put,
                        model::HttpMethod::Delete => oagw_sdk::HttpMethod::Delete,
                        model::HttpMethod::Patch => oagw_sdk::HttpMethod::Patch,
                    })
                    .collect(),
                path: h.path,
                query_allowlist: h.query_allowlist,
                path_suffix_mode: match h.path_suffix_mode {
                    model::PathSuffixMode::Disabled => oagw_sdk::PathSuffixMode::Disabled,
                    model::PathSuffixMode::Append => oagw_sdk::PathSuffixMode::Append,
                },
            }),
            grpc: r.match_rules.grpc.map(|g| oagw_sdk::GrpcMatch {
                service: g.service,
                method: g.method,
            }),
        },
        plugins: r.plugins.map(|p| oagw_sdk::PluginsConfig {
            sharing: sharing_mode_to_sdk(p.sharing),
            items: p
                .items
                .into_iter()
                .map(|b| oagw_sdk::PluginBinding {
                    plugin_ref: b.plugin_ref,
                    config: b.config,
                })
                .collect(),
        }),
        rate_limit: r.rate_limit.map(rate_limit_config_to_sdk),
        cors: r.cors.map(cors_config_to_sdk),
        tags: r.tags,
        priority: r.priority,
        enabled: r.enabled,
    }
}

fn cors_http_method_to_sdk(v: model::CorsHttpMethod) -> oagw_sdk::CorsHttpMethod {
    match v {
        model::CorsHttpMethod::Get => oagw_sdk::CorsHttpMethod::Get,
        model::CorsHttpMethod::Post => oagw_sdk::CorsHttpMethod::Post,
        model::CorsHttpMethod::Put => oagw_sdk::CorsHttpMethod::Put,
        model::CorsHttpMethod::Delete => oagw_sdk::CorsHttpMethod::Delete,
        model::CorsHttpMethod::Patch => oagw_sdk::CorsHttpMethod::Patch,
        model::CorsHttpMethod::Head => oagw_sdk::CorsHttpMethod::Head,
        model::CorsHttpMethod::Options => oagw_sdk::CorsHttpMethod::Options,
    }
}

fn cors_config_to_sdk(v: model::CorsConfig) -> oagw_sdk::CorsConfig {
    oagw_sdk::CorsConfig {
        sharing: sharing_mode_to_sdk(v.sharing),
        enabled: v.enabled,
        allowed_origins: v.allowed_origins,
        allowed_methods: v
            .allowed_methods
            .into_iter()
            .map(cors_http_method_to_sdk)
            .collect(),
        expose_headers: v.expose_headers,
        allow_credentials: v.allow_credentials,
    }
}

fn rate_limit_config_to_sdk(v: model::RateLimitConfig) -> oagw_sdk::RateLimitConfig {
    oagw_sdk::RateLimitConfig {
        sharing: sharing_mode_to_sdk(v.sharing),
        algorithm: match v.algorithm {
            model::RateLimitAlgorithm::TokenBucket => oagw_sdk::RateLimitAlgorithm::TokenBucket,
            model::RateLimitAlgorithm::SlidingWindow => oagw_sdk::RateLimitAlgorithm::SlidingWindow,
        },
        sustained: oagw_sdk::SustainedRate {
            rate: v.sustained.rate,
            window: match v.sustained.window {
                model::Window::Second => oagw_sdk::Window::Second,
                model::Window::Minute => oagw_sdk::Window::Minute,
                model::Window::Hour => oagw_sdk::Window::Hour,
                model::Window::Day => oagw_sdk::Window::Day,
            },
        },
        burst: v.burst.map(|b| oagw_sdk::BurstConfig {
            capacity: b.capacity,
        }),
        budget: v.budget.map(|b| oagw_sdk::BudgetConfig {
            mode: match b.mode {
                model::BudgetMode::Unlimited => oagw_sdk::BudgetMode::Unlimited,
                model::BudgetMode::Allocated => oagw_sdk::BudgetMode::Allocated,
                model::BudgetMode::Shared => oagw_sdk::BudgetMode::Shared,
            },
            total: b.total,
            overcommit_ratio: b.overcommit_ratio,
        }),
        scope: match v.scope {
            model::RateLimitScope::Global => oagw_sdk::RateLimitScope::Global,
            model::RateLimitScope::Tenant => oagw_sdk::RateLimitScope::Tenant,
            model::RateLimitScope::User => oagw_sdk::RateLimitScope::User,
            model::RateLimitScope::Ip => oagw_sdk::RateLimitScope::Ip,
            model::RateLimitScope::Route => oagw_sdk::RateLimitScope::Route,
        },
        strategy: match v.strategy {
            model::RateLimitStrategy::Reject => oagw_sdk::RateLimitStrategy::Reject,
            model::RateLimitStrategy::Queue => oagw_sdk::RateLimitStrategy::Queue,
            model::RateLimitStrategy::Degrade => oagw_sdk::RateLimitStrategy::Degrade,
        },
        cost: v.cost,
        response_headers: v.response_headers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::DomainError;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // -- Facade rate-limit cleanup (#4) -------------------------------------

    fn test_ctx() -> SecurityContext {
        SecurityContext::builder()
            .subject_tenant_id(Uuid::new_v4())
            .subject_id(Uuid::new_v4())
            .build()
            .expect("test security context")
    }

    /// CP stub: `delete_upstream` reports the cascade-deleted route ids and
    /// `delete_route` succeeds — unless `fail` is set, in which case both return
    /// an error. All other methods are unused.
    struct StubCp {
        cascade_route_ids: Vec<Uuid>,
        fail: bool,
    }

    #[async_trait]
    impl ControlPlaneService for StubCp {
        async fn create_upstream(
            &self,
            _: &SecurityContext,
            _: model::CreateUpstreamRequest,
        ) -> Result<model::Upstream, DomainError> {
            unimplemented!()
        }
        async fn get_upstream(
            &self,
            _: &SecurityContext,
            _: Uuid,
        ) -> Result<model::Upstream, DomainError> {
            unimplemented!()
        }
        async fn list_upstreams(
            &self,
            _: &SecurityContext,
            _: &model::ListQuery,
        ) -> Result<Vec<model::Upstream>, DomainError> {
            unimplemented!()
        }
        async fn update_upstream(
            &self,
            _: &SecurityContext,
            _: Uuid,
            _: model::UpdateUpstreamRequest,
        ) -> Result<model::Upstream, DomainError> {
            unimplemented!()
        }
        async fn delete_upstream(
            &self,
            _: &SecurityContext,
            id: Uuid,
        ) -> Result<Vec<Uuid>, DomainError> {
            if self.fail {
                return Err(DomainError::NotFound {
                    entity: "upstream",
                    id,
                });
            }
            Ok(self.cascade_route_ids.clone())
        }
        async fn create_route(
            &self,
            _: &SecurityContext,
            _: model::CreateRouteRequest,
        ) -> Result<model::Route, DomainError> {
            unimplemented!()
        }
        async fn get_route(
            &self,
            _: &SecurityContext,
            _: Uuid,
        ) -> Result<model::Route, DomainError> {
            unimplemented!()
        }
        async fn list_routes(
            &self,
            _: &SecurityContext,
            _: Option<Uuid>,
            _: &model::ListQuery,
        ) -> Result<Vec<model::Route>, DomainError> {
            unimplemented!()
        }
        async fn update_route(
            &self,
            _: &SecurityContext,
            _: Uuid,
            _: model::UpdateRouteRequest,
        ) -> Result<model::Route, DomainError> {
            unimplemented!()
        }
        async fn delete_route(&self, _: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
            if self.fail {
                return Err(DomainError::NotFound {
                    entity: "route",
                    id,
                });
            }
            Ok(())
        }
        async fn resolve_proxy_target(
            &self,
            _: &SecurityContext,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<(model::Upstream, model::Route), DomainError> {
            unimplemented!()
        }
    }

    /// DP stub that records rate-limit key cleanup calls.
    #[derive(Default)]
    struct RecordingDp {
        route_purges: Mutex<Vec<Uuid>>,
        /// (upstream_id, cascaded route ids) per cascade cleanup call.
        cascade_calls: Mutex<Vec<(Uuid, Vec<Uuid>)>>,
    }

    #[async_trait]
    impl DataPlaneService for RecordingDp {
        async fn proxy_request(
            &self,
            _: SecurityContext,
            _: http::Request<Body>,
        ) -> Result<http::Response<Body>, DomainError> {
            unimplemented!()
        }
        fn remove_rate_limit_keys_for_upstream(&self, _upstream_id: Uuid) {}
        fn remove_rate_limit_keys_for_route(&self, route_id: Uuid) {
            self.route_purges.lock().unwrap().push(route_id);
        }
        fn remove_rate_limit_keys_for_upstream_cascade(
            &self,
            upstream_id: Uuid,
            route_ids: &[Uuid],
        ) {
            self.cascade_calls
                .lock()
                .unwrap()
                .push((upstream_id, route_ids.to_vec()));
        }
    }

    #[tokio::test]
    async fn delete_upstream_purges_upstream_and_cascaded_route_keys() {
        let route_a = Uuid::new_v4();
        let route_b = Uuid::new_v4();
        let cp = Arc::new(StubCp {
            cascade_route_ids: vec![route_a, route_b],
            fail: false,
        });
        let dp = Arc::new(RecordingDp::default());
        let facade = ServiceGatewayClientV1Facade::new(cp, dp.clone());

        let upstream_id = Uuid::new_v4();
        facade
            .delete_upstream(test_ctx(), upstream_id)
            .await
            .unwrap();

        // One single-pass cascade cleanup covering the upstream + both routes.
        assert_eq!(
            *dp.cascade_calls.lock().unwrap(),
            vec![(upstream_id, vec![route_a, route_b])]
        );
    }

    #[tokio::test]
    async fn delete_route_purges_route_keys() {
        let cp = Arc::new(StubCp {
            cascade_route_ids: vec![],
            fail: false,
        });
        let dp = Arc::new(RecordingDp::default());
        let facade = ServiceGatewayClientV1Facade::new(cp, dp.clone());

        let route_id = Uuid::new_v4();
        facade.delete_route(test_ctx(), route_id).await.unwrap();

        assert_eq!(*dp.route_purges.lock().unwrap(), vec![route_id]);
        assert!(dp.cascade_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_upstream_failure_propagates_and_skips_cleanup() {
        let cp = Arc::new(StubCp {
            cascade_route_ids: vec![Uuid::new_v4()],
            fail: true,
        });
        let dp = Arc::new(RecordingDp::default());
        let facade = ServiceGatewayClientV1Facade::new(cp, dp.clone());

        let result = facade.delete_upstream(test_ctx(), Uuid::new_v4()).await;

        assert!(result.is_err(), "CP delete failure must propagate");
        // Cleanup runs only after a successful delete.
        assert!(dp.cascade_calls.lock().unwrap().is_empty());
        assert!(dp.route_purges.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_route_failure_propagates_and_skips_cleanup() {
        let cp = Arc::new(StubCp {
            cascade_route_ids: vec![],
            fail: true,
        });
        let dp = Arc::new(RecordingDp::default());
        let facade = ServiceGatewayClientV1Facade::new(cp, dp.clone());

        let result = facade.delete_route(test_ctx(), Uuid::new_v4()).await;

        assert!(result.is_err(), "CP delete failure must propagate");
        assert!(dp.route_purges.lock().unwrap().is_empty());
    }

    #[test]
    fn auth_config_hashmap_round_trips() {
        let mut config = HashMap::new();
        config.insert("header".into(), "authorization".into());
        config.insert("prefix".into(), "Bearer ".into());
        let sdk_auth = oagw_sdk::AuthConfig {
            plugin_type: "test-plugin".into(),
            sharing: oagw_sdk::SharingMode::Private,
            config: Some(config.clone()),
        };
        let domain_auth = auth_config_to_domain(sdk_auth);
        assert_eq!(domain_auth.plugin_type, "test-plugin");
        assert_eq!(domain_auth.sharing, model::SharingMode::Private);
        assert_eq!(domain_auth.config.unwrap(), config);
    }

    #[test]
    fn auth_config_none_stays_none() {
        let sdk_auth = oagw_sdk::AuthConfig {
            plugin_type: "noop".into(),
            sharing: oagw_sdk::SharingMode::Inherit,
            config: None,
        };
        let domain_auth = auth_config_to_domain(sdk_auth);
        assert!(domain_auth.config.is_none());
        assert_eq!(domain_auth.sharing, model::SharingMode::Inherit);
    }

    #[test]
    fn upstream_to_sdk_converts_auth_config_back() {
        let mut config = HashMap::new();
        config.insert("header".into(), "x-api-key".into());
        config.insert("secret_ref".into(), "cred://key".into());

        let domain_upstream = model::Upstream {
            id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            alias: "test".into(),
            server: model::Server {
                endpoints: vec![model::Endpoint {
                    scheme: model::Scheme::Https,
                    host: "example.com".into(),
                    port: 443,
                }],
            },
            protocol: "http".into(),
            enabled: true,
            auth: Some(model::AuthConfig {
                plugin_type: "apikey".into(),
                sharing: model::SharingMode::Private,
                config: Some(config),
            }),
            headers: None,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
        };

        let sdk = upstream_to_sdk(domain_upstream);
        let auth = sdk.auth.unwrap();
        assert_eq!(auth.plugin_type, "apikey");
        let config = auth.config.unwrap();
        assert_eq!(config.get("header").unwrap(), "x-api-key");
        assert_eq!(config.get("secret_ref").unwrap(), "cred://key");
    }

    #[test]
    fn domain_not_found_maps_to_canonical_not_found_with_resource_scope() {
        // The impl crate emits CanonicalError directly. Verify the
        // resource_type is set to the upstream GTS scope so SDK
        // consumers can dispatch on it by matching against
        // oagw_sdk::gts::UPSTREAM_SCHEMA.
        let domain_err = DomainError::NotFound {
            entity: "upstream",
            id: Uuid::nil(),
        };
        let canonical: CanonicalError = domain_err.into();
        assert!(matches!(canonical, CanonicalError::NotFound { .. }));
        assert_eq!(
            canonical.resource_type(),
            Some(oagw_sdk::gts::UPSTREAM_SCHEMA),
        );
    }

    #[test]
    fn domain_rate_limit_maps_to_canonical_resource_exhausted() {
        // Note: the canonical ResourceExhausted context type doesn't carry
        // retry_after_seconds today — the retry hint only lands in the
        // HTTP Retry-After header. In-process callers see it via the wire
        // headers; canonical-body callers do not.
        let domain_err = DomainError::RateLimitExceeded {
            detail: "too fast".into(),
            instance: "/api".into(),
            retry_after_secs: Some(30),
            limit: None,
            remaining: None,
            reset_epoch: None,
        };
        let canonical: CanonicalError = domain_err.into();
        assert!(matches!(
            canonical,
            CanonicalError::ResourceExhausted { .. }
        ));
    }

    #[test]
    fn domain_timeout_maps_to_canonical_deadline_exceeded() {
        let domain_err = DomainError::RequestTimeout {
            detail: "timed out".into(),
            instance: "/slow".into(),
        };
        let canonical: CanonicalError = domain_err.into();
        assert!(matches!(canonical, CanonicalError::DeadlineExceeded { .. }));
    }

    #[test]
    fn sharing_mode_round_trip() {
        for (sdk_val, expected_domain) in [
            (oagw_sdk::SharingMode::Private, model::SharingMode::Private),
            (oagw_sdk::SharingMode::Inherit, model::SharingMode::Inherit),
            (oagw_sdk::SharingMode::Enforce, model::SharingMode::Enforce),
        ] {
            let domain = sharing_mode_to_domain(sdk_val);
            assert_eq!(domain, expected_domain);
            let back = sharing_mode_to_sdk(domain);
            assert_eq!(back, sdk_val);
        }
    }

    #[test]
    fn scheme_round_trip() {
        for (sdk_val, expected_domain) in [
            (oagw_sdk::Scheme::Http, model::Scheme::Http),
            (oagw_sdk::Scheme::Https, model::Scheme::Https),
            (oagw_sdk::Scheme::Wss, model::Scheme::Wss),
            (oagw_sdk::Scheme::Wt, model::Scheme::Wt),
            (oagw_sdk::Scheme::Grpc, model::Scheme::Grpc),
        ] {
            let domain = scheme_to_domain(sdk_val);
            assert_eq!(domain, expected_domain);
            let back = scheme_to_sdk(domain);
            assert_eq!(back, sdk_val);
        }
    }
}
