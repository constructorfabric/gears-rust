use std::collections::HashMap;

use modkit_macros::domain_model;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared enums
// ---------------------------------------------------------------------------

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SharingMode {
    #[default]
    Private,
    Inherit,
    Enforce,
}

// ---------------------------------------------------------------------------
// Endpoint / Server
// ---------------------------------------------------------------------------

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scheme {
    Http,
    #[default]
    Https,
    Wss,
    Wt,
    Grpc,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub scheme: Scheme,
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    /// Whether this endpoint's port is the standard port for its scheme.
    ///
    /// Standard ports (omitted from derived aliases):
    /// - HTTP: 80
    /// - HTTPS / WSS / WT / gRPC: 443
    #[must_use]
    pub fn is_standard_port(&self) -> bool {
        match self.scheme {
            Scheme::Http => self.port == 80,
            Scheme::Https | Scheme::Wss | Scheme::Wt | Scheme::Grpc => self.port == 443,
        }
    }

    /// The normalized host: brackets stripped (IPv6), lowercased, trailing dots stripped.
    #[must_use]
    pub fn normalized_host(&self) -> String {
        let h = self
            .host
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(&self.host);
        h.to_ascii_lowercase().trim_end_matches('.').to_string()
    }

    /// Whether this endpoint's host is an IP address (v4 or v6).
    #[must_use]
    pub fn is_ip(&self) -> bool {
        self.normalized_host().parse::<std::net::IpAddr>().is_ok()
    }

    /// Single-endpoint alias contribution: `host` if standard port, `host:port` otherwise.
    #[must_use]
    pub fn alias_contribution(&self) -> String {
        let host = self.normalized_host();
        if self.is_standard_port() {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    pub endpoints: Vec<Endpoint>,
}

// ---------------------------------------------------------------------------
// AuthConfig
// ---------------------------------------------------------------------------

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct AuthConfig {
    pub plugin_type: String,
    pub sharing: SharingMode,
    pub config: Option<HashMap<String, String>>,
}

// ---------------------------------------------------------------------------
// HeadersConfig
// ---------------------------------------------------------------------------

#[domain_model]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HeadersConfig {
    pub request: Option<RequestHeaderRules>,
    pub response: Option<ResponseHeaderRules>,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RequestHeaderRules {
    pub set: HashMap<String, String>,
    pub add: HashMap<String, String>,
    pub remove: Vec<String>,
    pub passthrough: PassthroughMode,
    pub passthrough_allowlist: Vec<String>,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResponseHeaderRules {
    pub set: HashMap<String, String>,
    pub add: HashMap<String, String>,
    pub remove: Vec<String>,
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PassthroughMode {
    #[default]
    None,
    Allowlist,
    All,
}

// ---------------------------------------------------------------------------
// RateLimitConfig
// ---------------------------------------------------------------------------

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitConfig {
    pub sharing: SharingMode,
    pub algorithm: RateLimitAlgorithm,
    pub sustained: SustainedRate,
    pub burst: Option<BurstConfig>,
    pub budget: Option<BudgetConfig>,
    pub scope: RateLimitScope,
    pub strategy: RateLimitStrategy,
    pub cost: u32,
    pub response_headers: bool,
    /// Upstream ID of the shared-pool owner. Populated during hierarchical merge
    /// when `budget.mode == Shared` — causes all children to share one token
    /// bucket keyed to the pool owner. Not user-facing (never serialized).
    pub pool_owner_id: Option<Uuid>,
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateLimitAlgorithm {
    #[default]
    TokenBucket,
    SlidingWindow,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SustainedRate {
    pub rate: u32,
    pub window: Window,
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Window {
    #[default]
    Second,
    Minute,
    Hour,
    Day,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurstConfig {
    pub capacity: u32,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetConfig {
    pub mode: BudgetMode,
    /// Total budget capacity. Required for `Allocated` and `Shared` modes.
    pub total: Option<u32>,
    /// Over-provisioning ratio (1.0–2.0, default 1.0). Only for `Allocated` mode.
    pub overcommit_ratio: Option<f64>,
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BudgetMode {
    /// No budget tracking (default for leaf tenants).
    #[default]
    Unlimited,
    /// Parent allocates fixed budget to children.
    Allocated,
    /// Children share parent's budget (first-come-first-served).
    Shared,
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateLimitScope {
    Global,
    #[default]
    Tenant,
    User,
    Ip,
    Route,
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateLimitStrategy {
    #[default]
    Reject,
    Queue,
    Degrade,
}

// ---------------------------------------------------------------------------
// CorsConfig
// ---------------------------------------------------------------------------

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CorsHttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
    Head,
    Options,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct CorsConfig {
    pub sharing: SharingMode,
    pub enabled: bool,
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<CorsHttpMethod>,
    pub expose_headers: Vec<String>,
    pub allow_credentials: bool,
}

// ---------------------------------------------------------------------------
// PluginBinding / PluginsConfig
// ---------------------------------------------------------------------------

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct PluginBinding {
    pub plugin_ref: String,
    pub config: HashMap<String, String>,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PluginsConfig {
    pub sharing: SharingMode,
    pub items: Vec<PluginBinding>,
}

// ---------------------------------------------------------------------------
// Route matching
// ---------------------------------------------------------------------------

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathSuffixMode {
    Disabled,
    #[default]
    Append,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct HttpMatch {
    pub methods: Vec<HttpMethod>,
    pub path: String,
    pub query_allowlist: Vec<String>,
    pub path_suffix_mode: PathSuffixMode,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct GrpcMatch {
    pub service: String,
    pub method: String,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct MatchRules {
    pub http: Option<HttpMatch>,
    pub grpc: Option<GrpcMatch>,
}

// ---------------------------------------------------------------------------
// Domain entities
// ---------------------------------------------------------------------------

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub upstream_id: Uuid,
    pub match_rules: MatchRules,
    pub plugins: Option<PluginsConfig>,
    pub rate_limit: Option<RateLimitConfig>,
    pub cors: Option<CorsConfig>,
    pub tags: Vec<String>,
    pub priority: i32,
    pub enabled: bool,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Upstream {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub alias: String,
    pub server: Server,
    pub protocol: String,
    pub enabled: bool,
    pub auth: Option<AuthConfig>,
    pub headers: Option<HeadersConfig>,
    pub plugins: Option<PluginsConfig>,
    pub rate_limit: Option<RateLimitConfig>,
    pub cors: Option<CorsConfig>,
    pub tags: Vec<String>,
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListQuery {
    pub top: u32,
    pub skip: u32,
}

impl Default for ListQuery {
    fn default() -> Self {
        Self { top: 50, skip: 0 }
    }
}

// ---------------------------------------------------------------------------
// Request types (public fields, no builder)
// ---------------------------------------------------------------------------

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct CreateUpstreamRequest {
    pub server: Server,
    pub protocol: String,
    pub alias: Option<String>,
    pub auth: Option<AuthConfig>,
    pub headers: Option<HeadersConfig>,
    pub plugins: Option<PluginsConfig>,
    pub rate_limit: Option<RateLimitConfig>,
    pub cors: Option<CorsConfig>,
    pub tags: Vec<String>,
    pub enabled: bool,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateUpstreamRequest {
    pub server: Server,
    pub protocol: String,
    pub alias: Option<String>,
    pub auth: Option<AuthConfig>,
    pub headers: Option<HeadersConfig>,
    pub plugins: Option<PluginsConfig>,
    pub rate_limit: Option<RateLimitConfig>,
    pub cors: Option<CorsConfig>,
    pub tags: Vec<String>,
    pub enabled: bool,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct CreateRouteRequest {
    pub upstream_id: Uuid,
    pub match_rules: MatchRules,
    pub plugins: Option<PluginsConfig>,
    pub rate_limit: Option<RateLimitConfig>,
    pub cors: Option<CorsConfig>,
    pub tags: Vec<String>,
    pub priority: i32,
    pub enabled: bool,
}

#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateRouteRequest {
    pub match_rules: MatchRules,
    pub plugins: Option<PluginsConfig>,
    pub rate_limit: Option<RateLimitConfig>,
    pub cors: Option<CorsConfig>,
    pub tags: Vec<String>,
    pub priority: i32,
    pub enabled: bool,
}
