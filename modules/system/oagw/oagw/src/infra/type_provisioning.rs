//! Infrastructure implementation of `TypeProvisioningService` backed by `TypesRegistryClient`.
//!
//! Queries the types-registry for upstream and route GTS instances registered
//! by other modules during `init()`, deserializes their content, and returns
//! domain-level provisioned objects for `post_init()` to insert into repos.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use types_registry_sdk::{ListQuery, TypesRegistryClient};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::gts_helpers::{ROUTE_SCHEMA, UPSTREAM_SCHEMA};
use crate::domain::model as domain;
use crate::domain::type_provisioning::{
    ProvisionedRoute, ProvisionedUpstream, TypeProvisioningService,
};

// ---------------------------------------------------------------------------
// Local serde types for GTS entity deserialization.
//
// These mirror the GTS JSON shape and convert to domain types. They are
// intentionally separate from REST DTOs so each can evolve independently.
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

fn default_port() -> u16 {
    443
}

fn default_cost() -> u32 {
    1
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum Scheme {
    Http,
    #[default]
    Https,
    Wss,
    Wt,
    Grpc,
}

#[derive(Deserialize)]
struct Endpoint {
    #[serde(default)]
    scheme: Scheme,
    host: String,
    #[serde(default = "default_port")]
    port: u16,
}

#[derive(Deserialize)]
struct Server {
    endpoints: Vec<Endpoint>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum SharingMode {
    #[default]
    Private,
    Inherit,
    Enforce,
}

#[derive(Deserialize)]
struct AuthConfig {
    #[serde(rename = "type")]
    plugin_type: String,
    #[serde(default)]
    sharing: SharingMode,
    #[serde(default)]
    config: Option<HashMap<String, String>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum PassthroughMode {
    #[default]
    None,
    Allowlist,
    All,
}

#[derive(Deserialize, Default)]
struct RequestHeaderRules {
    #[serde(default)]
    set: HashMap<String, String>,
    #[serde(default)]
    add: HashMap<String, String>,
    #[serde(default)]
    remove: Vec<String>,
    #[serde(default)]
    passthrough: PassthroughMode,
    #[serde(default)]
    passthrough_allowlist: Vec<String>,
}

#[derive(Deserialize, Default)]
struct ResponseHeaderRules {
    #[serde(default)]
    set: HashMap<String, String>,
    #[serde(default)]
    add: HashMap<String, String>,
    #[serde(default)]
    remove: Vec<String>,
}

#[derive(Deserialize, Default)]
struct HeadersConfig {
    #[serde(default)]
    request: Option<RequestHeaderRules>,
    #[serde(default)]
    response: Option<ResponseHeaderRules>,
}

#[derive(Deserialize)]
struct PluginBinding {
    plugin_ref: String,
    #[serde(default)]
    config: HashMap<String, String>,
}

#[derive(Deserialize, Default)]
struct PluginsConfig {
    #[serde(default)]
    sharing: SharingMode,
    #[serde(default)]
    items: Vec<PluginBinding>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum RateLimitAlgorithm {
    #[default]
    TokenBucket,
    SlidingWindow,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum Window {
    #[default]
    Second,
    Minute,
    Hour,
    Day,
}

#[derive(Deserialize)]
struct SustainedRate {
    rate: u32,
    #[serde(default)]
    window: Window,
}

#[derive(Deserialize)]
struct BurstConfig {
    capacity: u32,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum RateLimitScope {
    Global,
    #[default]
    Tenant,
    User,
    Ip,
    Route,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum RateLimitStrategy {
    #[default]
    Reject,
    Queue,
    Degrade,
}

#[derive(Deserialize)]
struct RateLimitConfig {
    #[serde(default)]
    sharing: SharingMode,
    #[serde(default)]
    algorithm: RateLimitAlgorithm,
    sustained: SustainedRate,
    #[serde(default)]
    burst: Option<BurstConfig>,
    #[serde(default)]
    scope: RateLimitScope,
    #[serde(default)]
    strategy: RateLimitStrategy,
    #[serde(default = "default_cost")]
    cost: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum CorsHttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
    Options,
}

#[derive(Deserialize)]
struct CorsConfig {
    #[serde(default)]
    sharing: SharingMode,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    allowed_origins: Vec<String>,
    #[serde(default)]
    allowed_methods: Vec<CorsHttpMethod>,
    #[serde(default)]
    expose_headers: Vec<String>,
    #[serde(default)]
    allow_credentials: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum PathSuffixMode {
    Disabled,
    #[default]
    Append,
}

#[derive(Deserialize)]
struct HttpMatch {
    methods: Vec<HttpMethod>,
    path: String,
    #[serde(default)]
    query_allowlist: Vec<String>,
    #[serde(default)]
    path_suffix_mode: PathSuffixMode,
}

#[derive(Deserialize)]
struct GrpcMatch {
    service: String,
    method: String,
}

#[derive(Deserialize)]
struct MatchRules {
    #[serde(default)]
    http: Option<HttpMatch>,
    #[serde(default)]
    grpc: Option<GrpcMatch>,
}

/// Intermediate serde struct for deserializing upstream GTS entity content.
#[derive(Deserialize)]
struct UpstreamPayload {
    tenant_id: Uuid,
    server: Server,
    protocol: String,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    auth: Option<AuthConfig>,
    #[serde(default)]
    headers: Option<HeadersConfig>,
    #[serde(default)]
    plugins: Option<PluginsConfig>,
    #[serde(default)]
    rate_limit: Option<RateLimitConfig>,
    #[serde(default)]
    cors: Option<CorsConfig>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "default_true")]
    enabled: bool,
}

/// Intermediate serde struct for deserializing route GTS entity content.
#[derive(Deserialize)]
struct RoutePayload {
    tenant_id: Uuid,
    upstream_id: Uuid,
    #[serde(rename = "match")]
    match_rules: MatchRules,
    #[serde(default)]
    plugins: Option<PluginsConfig>,
    #[serde(default)]
    rate_limit: Option<RateLimitConfig>,
    #[serde(default)]
    cors: Option<CorsConfig>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    priority: i32,
    #[serde(default = "default_true")]
    enabled: bool,
}

// ---------------------------------------------------------------------------
// From conversions: local payload types → domain types
// ---------------------------------------------------------------------------

impl From<Scheme> for domain::Scheme {
    fn from(v: Scheme) -> Self {
        match v {
            Scheme::Http => Self::Http,
            Scheme::Https => Self::Https,
            Scheme::Wss => Self::Wss,
            Scheme::Wt => Self::Wt,
            Scheme::Grpc => Self::Grpc,
        }
    }
}

impl From<Endpoint> for domain::Endpoint {
    fn from(v: Endpoint) -> Self {
        Self {
            scheme: v.scheme.into(),
            host: v.host,
            port: v.port,
        }
    }
}

impl From<Server> for domain::Server {
    fn from(v: Server) -> Self {
        Self {
            endpoints: v.endpoints.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<SharingMode> for domain::SharingMode {
    fn from(v: SharingMode) -> Self {
        match v {
            SharingMode::Private => Self::Private,
            SharingMode::Inherit => Self::Inherit,
            SharingMode::Enforce => Self::Enforce,
        }
    }
}

impl From<AuthConfig> for domain::AuthConfig {
    fn from(v: AuthConfig) -> Self {
        Self {
            plugin_type: v.plugin_type,
            sharing: v.sharing.into(),
            config: v.config,
        }
    }
}

impl From<PassthroughMode> for domain::PassthroughMode {
    fn from(v: PassthroughMode) -> Self {
        match v {
            PassthroughMode::None => Self::None,
            PassthroughMode::Allowlist => Self::Allowlist,
            PassthroughMode::All => Self::All,
        }
    }
}

impl From<RequestHeaderRules> for domain::RequestHeaderRules {
    fn from(v: RequestHeaderRules) -> Self {
        Self {
            set: v.set,
            add: v.add,
            remove: v.remove,
            passthrough: v.passthrough.into(),
            passthrough_allowlist: v.passthrough_allowlist,
        }
    }
}

impl From<ResponseHeaderRules> for domain::ResponseHeaderRules {
    fn from(v: ResponseHeaderRules) -> Self {
        Self {
            set: v.set,
            add: v.add,
            remove: v.remove,
        }
    }
}

impl From<HeadersConfig> for domain::HeadersConfig {
    fn from(v: HeadersConfig) -> Self {
        Self {
            request: v.request.map(Into::into),
            response: v.response.map(Into::into),
        }
    }
}

impl From<RateLimitAlgorithm> for domain::RateLimitAlgorithm {
    fn from(v: RateLimitAlgorithm) -> Self {
        match v {
            RateLimitAlgorithm::TokenBucket => Self::TokenBucket,
            RateLimitAlgorithm::SlidingWindow => Self::SlidingWindow,
        }
    }
}

impl From<Window> for domain::Window {
    fn from(v: Window) -> Self {
        match v {
            Window::Second => Self::Second,
            Window::Minute => Self::Minute,
            Window::Hour => Self::Hour,
            Window::Day => Self::Day,
        }
    }
}

impl From<SustainedRate> for domain::SustainedRate {
    fn from(v: SustainedRate) -> Self {
        Self {
            rate: v.rate,
            window: v.window.into(),
        }
    }
}

impl From<BurstConfig> for domain::BurstConfig {
    fn from(v: BurstConfig) -> Self {
        Self {
            capacity: v.capacity,
        }
    }
}

impl From<RateLimitScope> for domain::RateLimitScope {
    fn from(v: RateLimitScope) -> Self {
        match v {
            RateLimitScope::Global => Self::Global,
            RateLimitScope::Tenant => Self::Tenant,
            RateLimitScope::User => Self::User,
            RateLimitScope::Ip => Self::Ip,
            RateLimitScope::Route => Self::Route,
        }
    }
}

impl From<RateLimitStrategy> for domain::RateLimitStrategy {
    fn from(v: RateLimitStrategy) -> Self {
        match v {
            RateLimitStrategy::Reject => Self::Reject,
            RateLimitStrategy::Queue => Self::Queue,
            RateLimitStrategy::Degrade => Self::Degrade,
        }
    }
}

impl From<RateLimitConfig> for domain::RateLimitConfig {
    fn from(v: RateLimitConfig) -> Self {
        Self {
            sharing: v.sharing.into(),
            algorithm: v.algorithm.into(),
            sustained: v.sustained.into(),
            burst: v.burst.map(Into::into),
            scope: v.scope.into(),
            strategy: v.strategy.into(),
            cost: v.cost,
        }
    }
}

impl From<PluginBinding> for domain::PluginBinding {
    fn from(v: PluginBinding) -> Self {
        Self {
            plugin_ref: v.plugin_ref,
            config: v.config,
        }
    }
}

impl From<PluginsConfig> for domain::PluginsConfig {
    fn from(v: PluginsConfig) -> Self {
        Self {
            sharing: v.sharing.into(),
            items: v.items.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<CorsHttpMethod> for domain::CorsHttpMethod {
    fn from(v: CorsHttpMethod) -> Self {
        match v {
            CorsHttpMethod::Get => Self::Get,
            CorsHttpMethod::Post => Self::Post,
            CorsHttpMethod::Put => Self::Put,
            CorsHttpMethod::Delete => Self::Delete,
            CorsHttpMethod::Patch => Self::Patch,
            CorsHttpMethod::Head => Self::Head,
            CorsHttpMethod::Options => Self::Options,
        }
    }
}

impl From<CorsConfig> for domain::CorsConfig {
    fn from(v: CorsConfig) -> Self {
        Self {
            sharing: v.sharing.into(),
            enabled: v.enabled,
            allowed_origins: v.allowed_origins,
            allowed_methods: v.allowed_methods.into_iter().map(Into::into).collect(),
            expose_headers: v.expose_headers,
            allow_credentials: v.allow_credentials,
        }
    }
}

impl From<HttpMethod> for domain::HttpMethod {
    fn from(v: HttpMethod) -> Self {
        match v {
            HttpMethod::Get => Self::Get,
            HttpMethod::Post => Self::Post,
            HttpMethod::Put => Self::Put,
            HttpMethod::Delete => Self::Delete,
            HttpMethod::Patch => Self::Patch,
        }
    }
}

impl From<PathSuffixMode> for domain::PathSuffixMode {
    fn from(v: PathSuffixMode) -> Self {
        match v {
            PathSuffixMode::Disabled => Self::Disabled,
            PathSuffixMode::Append => Self::Append,
        }
    }
}

impl From<HttpMatch> for domain::HttpMatch {
    fn from(v: HttpMatch) -> Self {
        Self {
            methods: v.methods.into_iter().map(Into::into).collect(),
            path: v.path,
            query_allowlist: v.query_allowlist,
            path_suffix_mode: v.path_suffix_mode.into(),
        }
    }
}

impl From<GrpcMatch> for domain::GrpcMatch {
    fn from(v: GrpcMatch) -> Self {
        Self {
            service: v.service,
            method: v.method,
        }
    }
}

impl From<MatchRules> for domain::MatchRules {
    fn from(v: MatchRules) -> Self {
        Self {
            http: v.http.map(Into::into),
            grpc: v.grpc.map(Into::into),
        }
    }
}

impl UpstreamPayload {
    fn into_provisioned(self, gts_instance_id: Option<Uuid>) -> ProvisionedUpstream {
        ProvisionedUpstream {
            tenant_id: self.tenant_id,
            request: domain::CreateUpstreamRequest {
                server: self.server.into(),
                protocol: self.protocol,
                alias: self.alias,
                auth: self.auth.map(Into::into),
                headers: self.headers.map(Into::into),
                plugins: self.plugins.map(Into::into),
                rate_limit: self.rate_limit.map(Into::into),
                cors: self.cors.map(Into::into),
                tags: self.tags,
                enabled: self.enabled,
            },
            gts_instance_id,
        }
    }
}

impl From<RoutePayload> for ProvisionedRoute {
    fn from(p: RoutePayload) -> Self {
        Self {
            tenant_id: p.tenant_id,
            request: domain::CreateRouteRequest {
                upstream_id: p.upstream_id,
                match_rules: p.match_rules.into(),
                plugins: p.plugins.map(Into::into),
                rate_limit: p.rate_limit.map(Into::into),
                cors: p.cors.map(Into::into),
                tags: p.tags,
                priority: p.priority,
                enabled: p.enabled,
            },
        }
    }
}

/// Extract the instance UUID from a GTS identifier string.
///
/// Given `gts.x.core.oagw.upstream.v1~<hex-uuid>`, returns `Some(<Uuid>)`.
fn extract_gts_instance_uuid(gts_id: &str) -> Option<Uuid> {
    let instance = gts_id.rsplit('~').next()?;
    Uuid::parse_str(instance).ok()
}

/// `TypeProvisioningService` implementation that delegates to `TypesRegistryClient`.
pub struct TypeProvisioningServiceImpl {
    registry: Arc<dyn TypesRegistryClient>,
}

impl TypeProvisioningServiceImpl {
    pub fn new(registry: Arc<dyn TypesRegistryClient>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl TypeProvisioningService for TypeProvisioningServiceImpl {
    async fn list_upstreams(&self) -> Result<Vec<ProvisionedUpstream>, DomainError> {
        let query = ListQuery::new()
            .with_pattern(format!("{UPSTREAM_SCHEMA}*"))
            .with_is_type(false);

        let entities = self
            .registry
            .list(query)
            .await
            .map_err(|e| DomainError::internal(e.to_string()))?;

        let mut result = Vec::with_capacity(entities.len());
        for entity in entities {
            match serde_json::from_value::<UpstreamPayload>(entity.content.clone()) {
                Ok(payload) => {
                    let gts_instance_id = extract_gts_instance_uuid(&entity.gts_id);
                    result.push(payload.into_provisioned(gts_instance_id));
                }
                Err(e) => {
                    tracing::warn!(
                        gts_id = %entity.gts_id,
                        error = %e,
                        "Skipping upstream: failed to deserialize GTS entity content"
                    );
                }
            }
        }

        Ok(result)
    }

    async fn list_routes(&self) -> Result<Vec<ProvisionedRoute>, DomainError> {
        let query = ListQuery::new()
            .with_pattern(format!("{ROUTE_SCHEMA}*"))
            .with_is_type(false);

        let entities = self
            .registry
            .list(query)
            .await
            .map_err(|e| DomainError::internal(e.to_string()))?;

        let mut result = Vec::with_capacity(entities.len());
        for entity in entities {
            match serde_json::from_value::<RoutePayload>(entity.content.clone()) {
                Ok(payload) => {
                    result.push(payload.into());
                }
                Err(e) => {
                    tracing::warn!(
                        gts_id = %entity.gts_id,
                        error = %e,
                        "Skipping route: failed to deserialize GTS entity content"
                    );
                }
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
#[path = "type_provisioning_tests.rs"]
mod type_provisioning_tests;
