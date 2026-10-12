use std::collections::HashMap;

use toolkit_macros::domain_model;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared enums
// ---------------------------------------------------------------------------

/// How a configuration is shared down the tenant hierarchy.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SharingMode {
    /// Not visible to descendant tenants.
    #[default]
    Private,
    /// Descendants inherit this configuration unless they override it.
    Inherit,
    /// Descendants inherit this configuration and cannot override it.
    Enforce,
}

// ---------------------------------------------------------------------------
// Endpoint / Server
// ---------------------------------------------------------------------------

/// Transport scheme of an upstream endpoint.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scheme {
    /// Plain HTTP.
    Http,
    /// HTTP over TLS (default).
    #[default]
    Https,
    /// WebSocket over TLS.
    Wss,
    /// WebTransport.
    Wt,
    /// gRPC.
    Grpc,
}

/// A single upstream endpoint (scheme + host + port).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Transport scheme.
    pub scheme: Scheme,
    /// Hostname or IP literal (IPv6 may be bracketed).
    pub host: String,
    /// TCP port.
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

/// Set of endpoints an upstream balances across.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    /// Endpoints of the upstream; load-balanced when more than one.
    pub endpoints: Vec<Endpoint>,
}

// ---------------------------------------------------------------------------
// AuthConfig
// ---------------------------------------------------------------------------

/// Authentication plugin configuration of an upstream.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct AuthConfig {
    /// GTS identifier of the auth plugin type.
    pub plugin_type: String,
    /// How the auth configuration is shared with descendant tenants.
    pub sharing: SharingMode,
    /// Plugin-specific configuration (flat key-value pairs; schema varies by plugin).
    pub config: Option<HashMap<String, String>>,
}

// ---------------------------------------------------------------------------
// HeadersConfig
// ---------------------------------------------------------------------------

/// Header transformation rules for proxied requests and responses.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HeadersConfig {
    /// Rules for outbound requests.
    pub request: Option<RequestHeaderRules>,
    /// Rules for upstream responses.
    pub response: Option<ResponseHeaderRules>,
}

/// Header transformation rules applied to outbound (upstream) requests.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RequestHeaderRules {
    /// Headers to set (overwrite if present).
    pub set: HashMap<String, String>,
    /// Headers to add (append, duplicates allowed).
    pub add: HashMap<String, String>,
    /// Header names to remove from the inbound request.
    pub remove: Vec<String>,
    /// Which inbound headers are forwarded to the upstream.
    pub passthrough: PassthroughMode,
    /// Header names forwarded when `passthrough` is `Allowlist`.
    pub passthrough_allowlist: Vec<String>,
}

/// Header transformation rules applied to upstream responses.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResponseHeaderRules {
    /// Headers to set (overwrite if present).
    pub set: HashMap<String, String>,
    /// Headers to add (append, duplicates allowed).
    pub add: HashMap<String, String>,
    /// Header names to remove from the upstream response.
    pub remove: Vec<String>,
}

/// Controls which inbound headers are forwarded to the upstream.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PassthroughMode {
    /// Forward no inbound headers (default).
    #[default]
    None,
    /// Forward only the inbound headers named in the allowlist.
    Allowlist,
    /// Forward all inbound headers except hop-by-hop ones.
    All,
}

// ---------------------------------------------------------------------------
// RateLimitConfig
// ---------------------------------------------------------------------------

/// Rate-limiting configuration of an upstream or route.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitConfig {
    /// How the limit is shared with descendant tenants.
    pub sharing: SharingMode,
    /// Enforcement algorithm.
    pub algorithm: RateLimitAlgorithm,
    /// Sustained rate.
    pub sustained: SustainedRate,
    /// Optional burst capacity; defaults to the sustained rate when `None`.
    pub burst: Option<BurstConfig>,
    /// Optional hierarchical budget configuration.
    pub budget: Option<BudgetConfig>,
    /// Identity the bucket is keyed by.
    pub scope: RateLimitScope,
    /// Behavior when the limit is exceeded.
    pub strategy: RateLimitStrategy,
    /// Tokens consumed per request.
    pub cost: u32,
    /// Whether to emit `X-RateLimit-*` response headers.
    pub response_headers: bool,
    /// Upstream ID of the shared-pool owner. Populated during hierarchical merge
    /// when `budget.mode == Shared` — causes all children to share one token
    /// bucket keyed to the pool owner. Not user-facing (never serialized).
    pub pool_owner_id: Option<Uuid>,
}

/// Algorithm used to enforce a rate limit.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateLimitAlgorithm {
    /// Token bucket with continuous refill (default).
    #[default]
    TokenBucket,
    /// Sliding-window counter.
    SlidingWindow,
}

/// Sustained request rate: `rate` tokens replenished per `window`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SustainedRate {
    /// Tokens replenished per window.
    pub rate: u32,
    /// Window length.
    pub window: Window,
}

/// Time window over which the sustained rate is measured.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Window {
    /// One second.
    #[default]
    Second,
    /// One minute.
    Minute,
    /// One hour.
    Hour,
    /// One day.
    Day,
}

/// Burst capacity configuration.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurstConfig {
    /// Maximum burst size.
    pub capacity: u32,
}

/// Budget allocation for hierarchical rate-limit management.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetConfig {
    /// Budget distribution mode.
    pub mode: BudgetMode,
    /// Total budget capacity. Required for `Allocated` and `Shared` modes.
    pub total: Option<u32>,
    /// Over-provisioning ratio (1.0–2.0, default 1.0). Only for `Allocated` mode.
    pub overcommit_ratio: Option<f64>,
}

/// How a rate-limit budget is distributed across child tenants.
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

/// Which identity a rate-limit bucket is keyed by.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateLimitScope {
    /// One bucket shared by all callers.
    Global,
    /// One bucket per tenant (default).
    #[default]
    Tenant,
    /// One bucket per calling subject.
    User,
    /// One bucket per client IP address.
    Ip,
    /// One bucket per route.
    Route,
}

/// What to do with requests that exceed the rate limit.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateLimitStrategy {
    /// Reject excess requests with a rate-limit error (default).
    #[default]
    Reject,
    /// Queue excess requests until capacity is available.
    Queue,
    /// Let excess requests through in a degraded mode.
    Degrade,
}

// ---------------------------------------------------------------------------
// CorsConfig
// ---------------------------------------------------------------------------

/// HTTP methods that can be allowed by a CORS configuration.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CorsHttpMethod {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
    /// `PUT`.
    Put,
    /// `DELETE`.
    Delete,
    /// `PATCH`.
    Patch,
    /// `HEAD`.
    Head,
    /// `OPTIONS`.
    Options,
}

/// Cross-Origin Resource Sharing (CORS) configuration.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct CorsConfig {
    /// How the CORS configuration is shared with descendant tenants.
    pub sharing: SharingMode,
    /// Whether CORS handling is active.
    pub enabled: bool,
    /// Origins allowed to call the upstream.
    pub allowed_origins: Vec<String>,
    /// Methods allowed in cross-origin requests.
    pub allowed_methods: Vec<CorsHttpMethod>,
    /// Response headers exposed to the browser.
    pub expose_headers: Vec<String>,
    /// Whether credentials are allowed in cross-origin requests.
    pub allow_credentials: bool,
}

// ---------------------------------------------------------------------------
// PluginBinding / PluginsConfig
// ---------------------------------------------------------------------------

/// A single plugin binding: plugin reference plus optional per-plugin config.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct PluginBinding {
    /// GTS identifier (built-in) or UUID (custom) of the plugin.
    pub plugin_ref: String,
    /// Per-binding plugin configuration (flat key-value pairs).
    pub config: HashMap<String, String>,
}

/// Plugin chain configuration.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PluginsConfig {
    /// How the plugin chain is shared with descendant tenants.
    pub sharing: SharingMode,
    /// Plugin bindings in execution order.
    pub items: Vec<PluginBinding>,
}

// ---------------------------------------------------------------------------
// Route matching
// ---------------------------------------------------------------------------

/// HTTP methods supported by route matching.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpMethod {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
    /// `PUT`.
    Put,
    /// `DELETE`.
    Delete,
    /// `PATCH`.
    Patch,
}

/// How the path suffix of the proxy URL is handled.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathSuffixMode {
    /// Requests with a path suffix beyond the route path are rejected.
    Disabled,
    /// The path suffix is appended to the upstream path (default).
    #[default]
    Append,
}

/// HTTP-protocol match rules of a route.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct HttpMatch {
    /// Allowed HTTP methods; at least one is required.
    pub methods: Vec<HttpMethod>,
    /// Path prefix (must start with `/`).
    pub path: String,
    /// Allowed query parameters; empty allows none.
    pub query_allowlist: Vec<String>,
    /// How the path suffix of the proxy URL is handled.
    pub path_suffix_mode: PathSuffixMode,
}

/// gRPC-protocol match rules of a route.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct GrpcMatch {
    /// Fully qualified gRPC service name.
    pub service: String,
    /// gRPC method name.
    pub method: String,
}

/// Protocol-scoped matching rules; exactly one of `http` or `grpc` is set.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct MatchRules {
    /// HTTP match rules.
    pub http: Option<HttpMatch>,
    /// gRPC match rules.
    pub grpc: Option<GrpcMatch>,
}

// ---------------------------------------------------------------------------
// Domain entities
// ---------------------------------------------------------------------------

/// A route mapping inbound requests to an upstream.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    /// Route ID.
    pub id: Uuid,
    /// Owning tenant.
    pub tenant_id: Uuid,
    /// Upstream the route forwards to.
    pub upstream_id: Uuid,
    /// Request matching rules.
    pub match_rules: MatchRules,
    /// Route-level plugin chain.
    pub plugins: Option<PluginsConfig>,
    /// Route-level rate limit.
    pub rate_limit: Option<RateLimitConfig>,
    /// Route-level CORS configuration.
    pub cors: Option<CorsConfig>,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Match priority; higher wins when several routes match.
    pub priority: i32,
    /// Whether the route is active.
    pub enabled: bool,
}

/// An external upstream service configuration.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct Upstream {
    /// Upstream ID.
    pub id: Uuid,
    /// Owning tenant.
    pub tenant_id: Uuid,
    /// Tenant-unique alias used to address the upstream in proxy URLs.
    pub alias: String,
    /// Endpoints of the upstream.
    pub server: Server,
    /// Protocol GTS identifier.
    pub protocol: String,
    /// Whether the upstream accepts traffic.
    pub enabled: bool,
    /// Authentication configuration.
    pub auth: Option<AuthConfig>,
    /// Header transformation rules.
    pub headers: Option<HeadersConfig>,
    /// Upstream-level plugin chain.
    pub plugins: Option<PluginsConfig>,
    /// Upstream-level rate limit.
    pub rate_limit: Option<RateLimitConfig>,
    /// Upstream-level CORS configuration.
    pub cors: Option<CorsConfig>,
    /// Free-form tags.
    pub tags: Vec<String>,
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

/// Pagination parameters for list queries.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListQuery {
    /// Maximum number of items to return.
    pub top: u32,
    /// Number of items to skip.
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

/// Request for creating an upstream.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct CreateUpstreamRequest {
    /// Optional pre-assigned ID. When `Some`, the service uses this UUID
    /// instead of generating a random one. Used by type-provisioning to
    /// preserve GTS instance UUIDs.
    pub id: Option<Uuid>,
    /// Endpoints of the upstream.
    pub server: Server,
    /// Protocol GTS identifier.
    pub protocol: String,
    /// Explicit alias; derived from the endpoints when `None`.
    pub alias: Option<String>,
    /// Authentication configuration.
    pub auth: Option<AuthConfig>,
    /// Header transformation rules.
    pub headers: Option<HeadersConfig>,
    /// Upstream-level plugin chain.
    pub plugins: Option<PluginsConfig>,
    /// Upstream-level rate limit.
    pub rate_limit: Option<RateLimitConfig>,
    /// Upstream-level CORS configuration.
    pub cors: Option<CorsConfig>,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Whether the upstream accepts traffic.
    pub enabled: bool,
}

/// Request for replacing an upstream (PUT semantics).
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateUpstreamRequest {
    /// Endpoints of the upstream.
    pub server: Server,
    /// Protocol GTS identifier.
    pub protocol: String,
    /// Explicit alias; derived from the endpoints when `None`.
    pub alias: Option<String>,
    /// Authentication configuration.
    pub auth: Option<AuthConfig>,
    /// Header transformation rules.
    pub headers: Option<HeadersConfig>,
    /// Upstream-level plugin chain.
    pub plugins: Option<PluginsConfig>,
    /// Upstream-level rate limit.
    pub rate_limit: Option<RateLimitConfig>,
    /// Upstream-level CORS configuration.
    pub cors: Option<CorsConfig>,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Whether the upstream accepts traffic.
    pub enabled: bool,
}

/// Request for creating a route.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct CreateRouteRequest {
    /// Optional pre-assigned ID. When `Some`, the service uses this UUID
    /// instead of generating a random one. Used by type-provisioning to
    /// preserve GTS instance UUIDs.
    pub id: Option<Uuid>,
    /// Upstream the route forwards to.
    pub upstream_id: Uuid,
    /// Request matching rules.
    pub match_rules: MatchRules,
    /// Route-level plugin chain.
    pub plugins: Option<PluginsConfig>,
    /// Route-level rate limit.
    pub rate_limit: Option<RateLimitConfig>,
    /// Route-level CORS configuration.
    pub cors: Option<CorsConfig>,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Match priority; higher wins when several routes match.
    pub priority: i32,
    /// Whether the route is active.
    pub enabled: bool,
}

/// Request for replacing a route (PUT semantics).
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateRouteRequest {
    /// Request matching rules.
    pub match_rules: MatchRules,
    /// Route-level plugin chain.
    pub plugins: Option<PluginsConfig>,
    /// Route-level rate limit.
    pub rate_limit: Option<RateLimitConfig>,
    /// Route-level CORS configuration.
    pub cors: Option<CorsConfig>,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Match priority; higher wins when several routes match.
    pub priority: i32,
    /// Whether the route is active.
    pub enabled: bool,
}
