//! Infrastructure implementation of `TypeProvisioningService` over the
//! platform registry API (`PlatformTypesRegistryApi`).
//!
//! Lists the active upstream and route GTS instances in the managed
//! catalogue, deserializes their content, and returns domain-level
//! provisioned objects for the registry reconcile. A row's identifier is the
//! registry UUID of its instance (`GtsId::to_uuid`).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use gts::GtsId;
use serde::Deserialize;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    GtsIdPattern, Instance, ListEntitiesRequest, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt,
};
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
    /// Defaults to the scheme's port (80 for `http`, 443 otherwise).
    #[serde(default)]
    port: Option<u16>,
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

/// Defaults as in the REST DTO (ADR-0005); unknown fields are rejected, so
/// a misspelled one does not fall back to its default.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CircuitBreakerConfig {
    #[serde(default)]
    sharing: SharingMode,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "default_failure_threshold")]
    failure_threshold: u32,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u32,
    #[serde(default = "default_success_threshold")]
    success_threshold: u32,
}

fn default_failure_threshold() -> u32 {
    oagw_sdk::CircuitBreakerConfig::DEFAULT_FAILURE_THRESHOLD
}

fn default_timeout_seconds() -> u32 {
    oagw_sdk::CircuitBreakerConfig::DEFAULT_TIMEOUT_SECONDS
}

fn default_success_threshold() -> u32 {
    oagw_sdk::CircuitBreakerConfig::DEFAULT_SUCCESS_THRESHOLD
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
    Head,
    Options,
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
    circuit_breaker: Option<CircuitBreakerConfig>,
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
        let scheme = domain::Scheme::from(v.scheme);
        Self {
            scheme,
            host: v.host,
            port: v.port.unwrap_or_else(|| scheme.default_port()),
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
            shared_pool: None,
            limit_owner_id: None,
        }
    }
}

impl From<CircuitBreakerConfig> for domain::CircuitBreakerConfig {
    fn from(v: CircuitBreakerConfig) -> Self {
        Self {
            sharing: v.sharing.into(),
            enabled: v.enabled,
            failure_threshold: v.failure_threshold,
            timeout_seconds: v.timeout_seconds,
            success_threshold: v.success_threshold,
            owner_id: None,
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
            HttpMethod::Head => Self::Head,
            HttpMethod::Options => Self::Options,
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
                circuit_breaker: self.circuit_breaker.map(Into::into),
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
        let upstream_id = resolve_upstream_reference(&self.upstream_id).ok_or_else(|| {
            DomainError::validation(format!(
                "Route '{gts_id}': upstream_id '{}' is neither an upstream instance identifier \
                 nor a UUID",
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

/// The upstream row a route's `upstream_id` names: an upstream instance
/// identifier (`gts.cf.core.oagw.upstream.v1~…`) names the row whose id is its
/// registry UUID (`GtsId::to_uuid`) — for an OAGW resource identifier, whose
/// last segment is a UUID, that is the row UUID itself; a bare UUID names the
/// row with that id.
fn resolve_upstream_reference(reference: &str) -> Option<Uuid> {
    if reference.starts_with(UPSTREAM_SCHEMA) {
        return GtsId::try_new(reference)
            .ok()
            .filter(|id| !id.is_type())
            .map(|id| id.to_uuid());
    }
    Uuid::parse_str(reference).ok()
}

/// Page size of the listings: the registry's maximum.
const PAGE_LIMIT: u32 = 100;

/// `TypeProvisioningService` over the platform registry API.
pub struct TypeProvisioningServiceImpl {
    registry: Arc<dyn PlatformTypesRegistryApi>,
}

impl TypeProvisioningServiceImpl {
    /// Create the service reading instances through the given platform registry client.
    pub fn new(registry: Arc<dyn PlatformTypesRegistryApi>) -> Self {
        Self { registry }
    }

    /// Every active instance under `type_id`, with its content. A failed page
    /// fails the whole listing: absence authorizes removal, so a partial
    /// listing must never reach the reconcile.
    async fn list_active(&self, type_id: &str) -> Result<Vec<Instance>, DomainError> {
        let pattern = GtsIdPattern::try_new(&format!("{type_id}*"))
            .map_err(|e| DomainError::internal(format!("listing pattern for {type_id}: {e}")))?;
        let query = ListEntitiesRequest::matching(pattern).with_limit(PAGE_LIMIT);
        self.registry
            .list_instances(&PlatformSecurityContext::outbound_marker(), query)
            .await
            .map_err(|e| DomainError::internal(e.to_string()))
    }
}

/// The content of `instance`, deserialized as `T`; `what` names the kind in
/// the error.
fn payload<T: serde::de::DeserializeOwned>(
    instance: &Instance,
    what: &str,
) -> Result<T, DomainError> {
    let content = instance.content.clone().ok_or_else(|| {
        DomainError::validation(format!(
            "{what} '{}': the registry returned no content",
            instance.id
        ))
    })?;
    serde_json::from_value(content).map_err(|e| {
        DomainError::validation(format!(
            "{what} '{}': failed to deserialize GTS instance object: {e}",
            instance.id
        ))
    })
}

#[async_trait]
impl TypeProvisioningService for TypeProvisioningServiceImpl {
    async fn list_upstreams(&self) -> Result<Vec<ProvisionedUpstream>, DomainError> {
        self.list_active(UPSTREAM_SCHEMA)
            .await?
            .iter()
            .map(|instance| {
                let payload: UpstreamPayload = payload(instance, "Upstream")?;
                Ok(payload.into_provisioned(Some(instance.uuid)))
            })
            .collect()
    }

    async fn list_routes(&self) -> Result<Vec<ProvisionedRoute>, DomainError> {
        self.list_active(ROUTE_SCHEMA)
            .await?
            .iter()
            .map(|instance| {
                let payload: RoutePayload = payload(instance, "Route")?;
                payload.into_provisioned(instance.id.as_ref(), instance.uuid)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use oagw_sdk::HTTP_PROTOCOL_ID;
    use toolkit_canonical_errors::CanonicalError;
    use toolkit_gts::gts_id;
    use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry, Seed};

    use super::*;

    const UPSTREAM: &str = gts_id!("cf.core.oagw.upstream.v1~cf.test.oagw.models.v1");
    const ROUTE: &str = gts_id!("cf.core.oagw.route.v1~cf.test.oagw.models.v1");

    fn registry_uuid(id: &str) -> Uuid {
        GtsId::try_new(id).unwrap().to_uuid()
    }

    fn service(registry: &Arc<MockTypesRegistry>) -> TypeProvisioningServiceImpl {
        TypeProvisioningServiceImpl::new(Arc::clone(registry) as Arc<dyn PlatformTypesRegistryApi>)
    }

    fn upstream_content(tenant_id: Uuid) -> serde_json::Value {
        serde_json::json!({
            "tenant_id": tenant_id,
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": 8080, "scheme": "http"}]
            },
            "protocol": HTTP_PROTOCOL_ID,
            "enabled": true,
            "tags": []
        })
    }

    fn route_content(tenant_id: Uuid, upstream_id: &str) -> serde_json::Value {
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

    /// A row's id is the registry UUID of its named instance.
    #[tokio::test]
    async fn list_upstreams_returns_parsed_entities() {
        let tenant = Uuid::new_v4();
        let registry = Arc::new(MockTypesRegistry::new());
        registry.seed(UPSTREAM, upstream_content(tenant));

        let upstreams = service(&registry).list_upstreams().await.unwrap();

        assert_eq!(upstreams.len(), 1);
        assert_eq!(upstreams[0].tenant_id, Some(tenant));
        assert_eq!(upstreams[0].request.id, Some(registry_uuid(UPSTREAM)));
        assert!(upstreams[0].request.enabled);
    }

    #[tokio::test]
    async fn list_upstreams_without_tenant_id_returns_none() {
        let registry = Arc::new(MockTypesRegistry::new());
        let mut content = upstream_content(Uuid::new_v4());
        content.as_object_mut().unwrap().remove("tenant_id");
        registry.seed(UPSTREAM, content);

        let upstreams = service(&registry).list_upstreams().await.unwrap();

        assert_eq!(upstreams.len(), 1);
        assert_eq!(upstreams[0].tenant_id, None);
    }

    /// Only instances under the upstream type are upstreams; the route and the
    /// type itself are not listed.
    #[tokio::test]
    async fn list_upstreams_lists_only_upstream_instances() {
        let registry = Arc::new(MockTypesRegistry::new());
        registry.seed(UPSTREAM, upstream_content(Uuid::new_v4()));
        registry.seed(ROUTE, route_content(Uuid::new_v4(), UPSTREAM));

        let upstreams = service(&registry).list_upstreams().await.unwrap();
        let routes = service(&registry).list_routes().await.unwrap();

        let ids = |ids: Vec<Option<Uuid>>| ids;
        assert_eq!(
            ids(upstreams.iter().map(|u| u.request.id).collect()),
            vec![Some(registry_uuid(UPSTREAM))]
        );
        assert_eq!(
            ids(routes.iter().map(|r| r.request.id).collect()),
            vec![Some(registry_uuid(ROUTE))]
        );
    }

    /// ADR-0018: PERS-PROV-008. A deleted instance is not part of the desired
    /// set, so the reconcile removes its row.
    #[tokio::test]
    async fn deleted_instances_are_not_listed() {
        let registry = Arc::new(MockTypesRegistry::new());
        registry.seed_entity(Seed::new(UPSTREAM, upstream_content(Uuid::new_v4())).deleted());
        registry.seed_entity(Seed::new(ROUTE, route_content(Uuid::new_v4(), UPSTREAM)).deleted());

        assert!(
            service(&registry)
                .list_upstreams()
                .await
                .unwrap()
                .is_empty()
        );
        assert!(service(&registry).list_routes().await.unwrap().is_empty());
    }

    /// ADR-0018: PERS-PROV-018. Unreadable content fails the whole listing.
    #[tokio::test]
    async fn list_upstreams_rejects_invalid_content() {
        let registry = Arc::new(MockTypesRegistry::new());
        registry.seed(UPSTREAM, serde_json::json!({"invalid": true}));

        let err = service(&registry).list_upstreams().await.unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("failed to deserialize"), "got: {msg}");
    }

    #[tokio::test]
    async fn list_upstreams_returns_empty_when_none_registered() {
        let registry = Arc::new(MockTypesRegistry::new());

        assert!(
            service(&registry)
                .list_upstreams()
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn list_upstreams_propagates_registry_error() {
        let registry = Arc::new(MockTypesRegistry::new());
        registry.seed(UPSTREAM, upstream_content(Uuid::new_v4()));
        registry.inject(
            Fault::on(Call::List).fail(CanonicalError::internal("connection lost").create()),
        );

        assert!(service(&registry).list_upstreams().await.is_err());
    }

    /// The listing reads every page, and a failed later page fails it whole:
    /// a prefix must never reach the reconcile, whose absence means removal.
    #[tokio::test]
    async fn listings_read_every_page_and_fail_whole_on_a_failed_page() {
        let registry = Arc::new(MockTypesRegistry::new());
        let count = usize::try_from(PAGE_LIMIT).unwrap() + 1;
        for n in 0..count {
            registry.seed(
                &format!("{UPSTREAM_SCHEMA}cf.test.oagw.page_{n}.v1"),
                upstream_content(Uuid::new_v4()),
            );
        }

        let all = service(&registry).list_upstreams().await.unwrap();
        assert_eq!(all.len(), count);
        assert_eq!(registry.calls(Call::List), 2, "two pages");

        registry.inject(
            Fault::on(Call::List)
                .nth(2)
                .fail(CanonicalError::internal("page lost").create()),
        );
        assert!(service(&registry).list_upstreams().await.is_err());
    }

    #[tokio::test]
    async fn list_routes_returns_parsed_entities() {
        let tenant = Uuid::new_v4();
        let registry = Arc::new(MockTypesRegistry::new());
        registry.seed(ROUTE, route_content(tenant, UPSTREAM));

        let routes = service(&registry).list_routes().await.unwrap();

        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].tenant_id, Some(tenant));
        assert_eq!(routes[0].request.id, Some(registry_uuid(ROUTE)));
        assert_eq!(routes[0].request.upstream_id, registry_uuid(UPSTREAM));
        assert!(routes[0].request.enabled);
    }

    /// A route names its upstream by the upstream instance identifier, by an
    /// OAGW resource identifier, or by a bare row UUID.
    #[tokio::test]
    async fn route_upstream_references_resolve_to_the_upstream_row() {
        let row = Uuid::new_v4();
        for (reference, expected) in [
            (UPSTREAM.to_owned(), registry_uuid(UPSTREAM)),
            (format!("{UPSTREAM_SCHEMA}{row}"), row),
            (row.to_string(), row),
        ] {
            let registry = Arc::new(MockTypesRegistry::new());
            registry.seed(ROUTE, route_content(Uuid::new_v4(), &reference));

            let routes = service(&registry).list_routes().await.unwrap();

            assert_eq!(routes[0].request.upstream_id, expected, "{reference}");
        }
    }

    /// A reference that is no upstream instance identifier nor a UUID fails
    /// the listing.
    #[tokio::test]
    async fn route_with_an_unresolvable_upstream_reference_fails_the_listing() {
        for reference in [
            "not-an-id",
            gts_id!("cf.core.oagw.route.v1~cf.test.oagw.models.v1"),
            gts_id!("cf.core.oagw.upstream.v1~"),
        ] {
            let registry = Arc::new(MockTypesRegistry::new());
            registry.seed(ROUTE, route_content(Uuid::new_v4(), reference));

            let err = service(&registry).list_routes().await.unwrap_err();

            assert!(
                err.to_string().contains("upstream_id"),
                "{reference}: {err}"
            );
        }
    }

    #[tokio::test]
    async fn list_routes_rejects_invalid_content() {
        let registry = Arc::new(MockTypesRegistry::new());
        registry.seed(ROUTE, serde_json::json!({"garbage": true}));

        let err = service(&registry).list_routes().await.unwrap_err();

        assert!(err.to_string().contains("failed to deserialize"), "{err}");
    }

    #[tokio::test]
    async fn list_routes_propagates_registry_error() {
        let registry = Arc::new(MockTypesRegistry::new());
        registry.inject(Fault::on(Call::List).fail(CanonicalError::internal("timeout").create()));

        assert!(service(&registry).list_routes().await.is_err());
    }

    /// The listing is made with the platform's own outbound context.
    #[tokio::test]
    async fn listings_use_the_platform_context() {
        let registry = Arc::new(MockTypesRegistry::new());

        service(&registry).list_upstreams().await.unwrap();

        assert_eq!(registry.platform_calls(Call::List), 1);
    }

    // -----------------------------------------------------------------------
    // Payload deserialization tests
    // -----------------------------------------------------------------------

    /// An omitted port is the scheme's default: 80 for `http`, 443 for the
    /// others (#4979).
    #[test]
    fn omitted_port_defaults_per_scheme() {
        let json = serde_json::json!({
            "server": {
                "endpoints": [
                    {"scheme": "http", "host": "a.example.com"},
                    {"scheme": "https", "host": "b.example.com"},
                    {"scheme": "wss", "host": "c.example.com"},
                    {"host": "d.example.com"}
                ]
            },
            "protocol": HTTP_PROTOCOL_ID,
        });
        let payload: UpstreamPayload = serde_json::from_value(json).unwrap();
        let ports: Vec<u16> = payload
            .into_provisioned(None)
            .request
            .server
            .endpoints
            .iter()
            .map(|e| e.port)
            .collect();
        assert_eq!(ports, [80, 443, 443, 443]);
    }

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
            "protocol": HTTP_PROTOCOL_ID,
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
        assert_eq!(req.protocol, HTTP_PROTOCOL_ID);
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
        assert_eq!(req.circuit_breaker, None);
    }

    #[test]
    fn deserialize_upstream_circuit_breaker_with_defaults() {
        let json = serde_json::json!({
            "server": {"endpoints": [{"host": "api.example.com"}]},
            "protocol": HTTP_PROTOCOL_ID,
            "circuit_breaker": {"sharing": "enforce", "failure_threshold": 2}
        });
        let payload: UpstreamPayload = serde_json::from_value(json).unwrap();
        let provisioned = payload.into_provisioned(None);
        assert_eq!(
            provisioned.request.circuit_breaker,
            Some(domain::CircuitBreakerConfig {
                sharing: domain::SharingMode::Enforce,
                enabled: true,
                failure_threshold: 2,
                timeout_seconds: 30,
                success_threshold: 3,
                owner_id: None,
            })
        );
    }

    /// A misspelled field is an error, not a silent default.
    #[test]
    fn upstream_circuit_breaker_rejects_unknown_fields() {
        let json = serde_json::json!({
            "server": {"endpoints": [{"host": "api.example.com"}]},
            "protocol": HTTP_PROTOCOL_ID,
            "circuit_breaker": {"failure_treshold": 1}
        });
        let err = serde_json::from_value::<UpstreamPayload>(json)
            .err()
            .map(|e| e.to_string());
        assert!(
            err.as_deref()
                .is_some_and(|e| e.contains("unknown field `failure_treshold`")),
            "{err:?}"
        );
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
                    "methods": ["POST", "PUT", "HEAD", "OPTIONS"],
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
            .into_provisioned(
                gts_id!("cf.core.oagw.route.v1~cf.core.oagw.test.v1"),
                route_uuid,
            )
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
            vec![
                domain::HttpMethod::Post,
                domain::HttpMethod::Put,
                domain::HttpMethod::Head,
                domain::HttpMethod::Options,
            ]
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
            .into_provisioned(
                gts_id!("cf.core.oagw.route.v1~cf.core.oagw.test.v1"),
                route_uuid,
            )
            .unwrap();
        assert_eq!(provisioned.tenant_id, None);
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
