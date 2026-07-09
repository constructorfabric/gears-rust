//! Infrastructure implementation of `TypeProvisioningService` backed by `TypesRegistryClient`.
//!
//! Queries the types-registry for upstream and route GTS instances registered
//! by other gears during `init()`, deserializes their content, and returns
//! domain-level provisioned objects for `post_init()` to insert into repos.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use types_registry_sdk::{InstanceQuery, TypesRegistryClient};
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
    budget: Option<BudgetConfig>,
    #[serde(default)]
    scope: RateLimitScope,
    #[serde(default)]
    strategy: RateLimitStrategy,
    #[serde(default = "default_cost")]
    cost: u32,
    #[serde(default = "default_true")]
    response_headers: bool,
}

#[derive(Deserialize)]
struct BudgetConfig {
    #[serde(default)]
    mode: BudgetMode,
    #[serde(default)]
    total: Option<u32>,
    #[serde(default)]
    overcommit_ratio: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum BudgetMode {
    #[default]
    Unlimited,
    Allocated,
    Shared,
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
    #[serde(default)]
    tenant_id: Option<Uuid>,
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
    #[serde(default)]
    tenant_id: Option<Uuid>,
    upstream_id: String,
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
            budget: v.budget.map(Into::into),
            scope: v.scope.into(),
            strategy: v.strategy.into(),
            cost: v.cost,
            response_headers: v.response_headers,
            pool_owner_id: None,
        }
    }
}

impl From<BudgetConfig> for domain::BudgetConfig {
    fn from(v: BudgetConfig) -> Self {
        Self {
            mode: v.mode.into(),
            total: v.total,
            overcommit_ratio: v.overcommit_ratio,
        }
    }
}

impl From<BudgetMode> for domain::BudgetMode {
    fn from(v: BudgetMode) -> Self {
        match v {
            BudgetMode::Unlimited => Self::Unlimited,
            BudgetMode::Allocated => Self::Allocated,
            BudgetMode::Shared => Self::Shared,
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
                id: gts_instance_id,
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
        }
    }
}

impl RoutePayload {
    fn into_provisioned(
        self,
        gts_id: &str,
        gts_instance_id: Uuid,
    ) -> Result<ProvisionedRoute, DomainError> {
        // Accept both full GTS identifier and bare UUID for upstream_id.
        let upstream_id = extract_gts_instance_uuid(&self.upstream_id)
            .or_else(|| Uuid::parse_str(&self.upstream_id).ok())
            .ok_or_else(|| {
                DomainError::validation(format!(
                    "Route '{gts_id}': upstream_id '{}' is not a valid UUID or GTS identifier",
                    self.upstream_id
                ))
            })?;
        Ok(ProvisionedRoute {
            tenant_id: self.tenant_id,
            request: domain::CreateRouteRequest {
                id: Some(gts_instance_id),
                upstream_id,
                match_rules: self.match_rules.into(),
                plugins: self.plugins.map(Into::into),
                rate_limit: self.rate_limit.map(Into::into),
                cors: self.cors.map(Into::into),
                tags: self.tags,
                priority: self.priority,
                enabled: self.enabled,
            },
        })
    }
}

/// Extract the instance UUID from a GTS identifier string.
///
/// Given `gts.cf.core.oagw.upstream.v1~<hex-uuid>`, returns `Some(<Uuid>)`.
fn extract_gts_instance_uuid(gts_id: &str) -> Option<Uuid> {
    let instance = gts_id.rsplit('~').next()?;
    Uuid::parse_str(instance).ok()
}

/// Like `extract_gts_instance_uuid` but returns an error suitable for
/// failing startup when the instance segment is not a valid UUID.
fn require_gts_instance_uuid(gts_id: &str) -> Result<Uuid, DomainError> {
    extract_gts_instance_uuid(gts_id).ok_or_else(|| {
        DomainError::validation(format!(
            "GTS entity '{gts_id}' has a non-UUID instance segment; \
             OAGW requires upstream and route $id values to end with a UUID \
             (e.g. gts.cf.core.oagw.upstream.v1~<uuid>)"
        ))
    })
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
        let query = InstanceQuery::new().with_pattern(format!("{UPSTREAM_SCHEMA}*"));

        let instances = self
            .registry
            .list_instances(query)
            .await
            .map_err(|e| DomainError::internal(e.to_string()))?;

        let mut result = Vec::with_capacity(instances.len());
        for instance in instances {
            let gts_instance_id = require_gts_instance_uuid(instance.id.as_ref())?;
            match serde_json::from_value::<UpstreamPayload>(instance.object.clone()) {
                Ok(payload) => {
                    result.push(payload.into_provisioned(Some(gts_instance_id)));
                }
                Err(e) => {
                    return Err(DomainError::validation(format!(
                        "Upstream '{}': failed to deserialize GTS instance object: {e}",
                        instance.id
                    )));
                }
            }
        }

        Ok(result)
    }

    async fn list_routes(&self) -> Result<Vec<ProvisionedRoute>, DomainError> {
        let query = InstanceQuery::new().with_pattern(format!("{ROUTE_SCHEMA}*"));

        let instances = self
            .registry
            .list_instances(query)
            .await
            .map_err(|e| DomainError::internal(e.to_string()))?;

        let mut result = Vec::with_capacity(instances.len());
        for instance in instances {
            let gts_instance_id = require_gts_instance_uuid(instance.id.as_ref())?;
            match serde_json::from_value::<RoutePayload>(instance.object.clone()) {
                Ok(payload) => {
                    result.push(payload.into_provisioned(instance.id.as_ref(), gts_instance_id)?);
                }
                Err(e) => {
                    return Err(DomainError::validation(format!(
                        "Route '{}': failed to deserialize GTS instance object: {e}",
                        instance.id
                    )));
                }
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use types_registry_sdk::{
        GtsInstance,
        testing::{MockTypesRegistryClient, internal, make_test_instance},
    };

    use super::*;

    fn make_upstream_instance(gts_id: &str, object: serde_json::Value) -> GtsInstance {
        make_test_instance(gts_id, object)
    }

    fn make_route_instance(gts_id: &str, object: serde_json::Value) -> GtsInstance {
        make_test_instance(gts_id, object)
    }

    fn upstream_content(tenant_id: Uuid) -> serde_json::Value {
        serde_json::json!({
            "tenant_id": tenant_id,
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": 8080, "scheme": "http"}]
            },
            "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
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
        let instance_id = Uuid::new_v4();
        let content = upstream_content(tenant);
        let gts_id = format!("gts.cf.core.oagw.upstream.v1~{instance_id}");

        let registry = Arc::new(
            MockTypesRegistryClient::new()
                .with_instances([make_upstream_instance(&gts_id, content)]),
        );
        let svc = TypeProvisioningServiceImpl::new(registry);

        let upstreams = svc.list_upstreams().await.unwrap();
        assert_eq!(upstreams.len(), 1);
        assert_eq!(upstreams[0].tenant_id, Some(tenant));
        assert_eq!(upstreams[0].request.id, Some(instance_id));
        assert!(upstreams[0].request.enabled);
    }

    #[tokio::test]
    async fn list_upstreams_rejects_non_uuid_instance_id() {
        let registry =
            Arc::new(
                MockTypesRegistryClient::new().with_instances([make_upstream_instance(
                    "gts.cf.core.oagw.upstream.v1~cf.core.oagw.test.v1",
                    upstream_content(Uuid::new_v4()),
                )]),
            );
        let svc = TypeProvisioningServiceImpl::new(registry);

        let err = svc.list_upstreams().await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("non-UUID instance segment"),
            "expected non-UUID error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn list_upstreams_rejects_invalid_content() {
        let instance_id = Uuid::new_v4();
        let gts_id = format!("gts.cf.core.oagw.upstream.v1~{instance_id}");
        let registry =
            Arc::new(
                MockTypesRegistryClient::new().with_instances([make_upstream_instance(
                    &gts_id,
                    serde_json::json!({"invalid": true}),
                )]),
            );
        let svc = TypeProvisioningServiceImpl::new(registry);

        let err = svc.list_upstreams().await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("failed to deserialize"),
            "expected deserialization error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn list_upstreams_returns_empty_when_none_registered() {
        let registry = Arc::new(MockTypesRegistryClient::new());
        let svc = TypeProvisioningServiceImpl::new(registry);

        let upstreams = svc.list_upstreams().await.unwrap();
        assert!(upstreams.is_empty());
    }

    #[tokio::test]
    async fn list_upstreams_propagates_registry_error() {
        let registry =
            Arc::new(MockTypesRegistryClient::new().with_list_error(internal("connection lost")));
        let svc = TypeProvisioningServiceImpl::new(registry);

        let result = svc.list_upstreams().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_routes_returns_parsed_entities() {
        let tenant = Uuid::new_v4();
        let upstream_id = Uuid::new_v4();
        let route_instance_id = Uuid::new_v4();
        let content = route_content(tenant, upstream_id);
        let gts_id = format!("gts.cf.core.oagw.route.v1~{route_instance_id}");

        let registry = Arc::new(
            MockTypesRegistryClient::new().with_instances([make_route_instance(&gts_id, content)]),
        );
        let svc = TypeProvisioningServiceImpl::new(registry);

        let routes = svc.list_routes().await.unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].tenant_id, Some(tenant));
        assert_eq!(routes[0].request.id, Some(route_instance_id));
        assert_eq!(routes[0].request.upstream_id, upstream_id);
        assert!(routes[0].request.enabled);
    }

    #[tokio::test]
    async fn list_routes_rejects_non_uuid_instance_id() {
        let registry =
            Arc::new(
                MockTypesRegistryClient::new().with_instances([make_route_instance(
                    "gts.cf.core.oagw.route.v1~cf.core.oagw.test.v1",
                    route_content(Uuid::new_v4(), Uuid::new_v4()),
                )]),
            );
        let svc = TypeProvisioningServiceImpl::new(registry);

        let err = svc.list_routes().await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("non-UUID instance segment"),
            "expected non-UUID error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn list_routes_rejects_invalid_content() {
        let instance_id = Uuid::new_v4();
        let gts_id = format!("gts.cf.core.oagw.route.v1~{instance_id}");
        let registry =
            Arc::new(
                MockTypesRegistryClient::new().with_instances([make_route_instance(
                    &gts_id,
                    serde_json::json!({"garbage": true}),
                )]),
            );
        let svc = TypeProvisioningServiceImpl::new(registry);

        let err = svc.list_routes().await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("failed to deserialize"),
            "expected deserialization error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn list_routes_propagates_registry_error() {
        let registry =
            Arc::new(MockTypesRegistryClient::new().with_list_error(internal("timeout")));
        let svc = TypeProvisioningServiceImpl::new(registry);

        let result = svc.list_routes().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_upstreams_uses_correct_pattern() {
        let registry = Arc::new(MockTypesRegistryClient::new());
        let svc = TypeProvisioningServiceImpl::new(registry.clone());

        let _ = svc.list_upstreams().await;
        let queries = registry.received_instance_queries();
        assert_eq!(queries.len(), 1);
        assert_eq!(
            queries[0].pattern.as_deref(),
            Some("gts.cf.core.oagw.upstream.v1~*")
        );
    }

    #[tokio::test]
    async fn list_routes_uses_correct_pattern() {
        let registry = Arc::new(MockTypesRegistryClient::new());
        let svc = TypeProvisioningServiceImpl::new(registry.clone());

        let _ = svc.list_routes().await;
        let queries = registry.received_instance_queries();
        assert_eq!(queries.len(), 1);
        assert_eq!(
            queries[0].pattern.as_deref(),
            Some("gts.cf.core.oagw.route.v1~*")
        );
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
            "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
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

        assert_eq!(provisioned.tenant_id, Some(tenant));
        let req = &provisioned.request;
        assert_eq!(req.server.endpoints.len(), 2);
        assert_eq!(req.server.endpoints[0].scheme, domain::Scheme::Https);
        assert_eq!(req.server.endpoints[0].host, "api.openai.com");
        assert_eq!(req.server.endpoints[0].port, 443);
        assert_eq!(req.server.endpoints[1].scheme, domain::Scheme::Http);
        assert_eq!(
            req.protocol,
            "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1"
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
        let route_uuid = Uuid::new_v4();
        let provisioned = payload
            .into_provisioned("gts.cf.core.oagw.route.v1~cf.core.oagw.test.v1", route_uuid)
            .expect("upstream_id should parse");

        assert_eq!(provisioned.tenant_id, Some(tenant));
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
    fn deserialize_upstream_without_tenant_id_produces_none() {
        let json = serde_json::json!({
            "server": {
                "endpoints": [{"host": "api.example.com", "port": 443, "scheme": "https"}]
            },
            "protocol": "http"
        });
        let payload: UpstreamPayload = serde_json::from_value(json).unwrap();
        let provisioned = payload.into_provisioned(None);
        assert_eq!(provisioned.tenant_id, None);
    }

    #[test]
    fn deserialize_route_without_tenant_id_produces_none() {
        let upstream_id = Uuid::new_v4();
        let json = serde_json::json!({
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
        });
        let payload: RoutePayload = serde_json::from_value(json).unwrap();
        let route_uuid = Uuid::new_v4();
        let provisioned = payload
            .into_provisioned("gts.cf.core.oagw.route.v1~cf.core.oagw.test.v1", route_uuid)
            .unwrap();
        assert_eq!(provisioned.tenant_id, None);
    }

    #[tokio::test]
    async fn list_upstreams_without_tenant_id_returns_none() {
        let instance_id = Uuid::new_v4();
        let content = serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": 8080, "scheme": "http"}]
            },
            "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
            "enabled": true,
            "tags": []
        });
        let gts_id = format!("gts.cf.core.oagw.upstream.v1~{instance_id}");

        let registry = Arc::new(
            MockTypesRegistryClient::new()
                .with_instances([make_upstream_instance(&gts_id, content)]),
        );
        let svc = TypeProvisioningServiceImpl::new(registry);

        let upstreams = svc.list_upstreams().await.unwrap();
        assert_eq!(upstreams.len(), 1);
        assert_eq!(upstreams[0].tenant_id, None);
        assert_eq!(upstreams[0].request.id, Some(instance_id));
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
}
