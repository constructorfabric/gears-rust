use std::collections::HashMap;

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::plugin::credentials::{CREDENTIAL_KEYS, CredentialRef};

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

impl Scheme {
    /// The port an endpoint of this scheme uses when none is given:
    /// 80 for `http`, 443 for the TLS schemes.
    #[must_use]
    pub fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https | Self::Wss | Self::Wt | Self::Grpc => 443,
        }
    }
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
        self.port == self.scheme.default_port()
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

impl AuthConfig {
    /// The credential references this config holds: the value of every
    /// [`CREDENTIAL_KEYS`] key present, in that order, whatever the plugin.
    /// Other keys are not references and are not resolved.
    pub fn credential_refs(&self) -> impl Iterator<Item = CredentialRef<'_>> {
        let config = self.config.as_ref();
        CREDENTIAL_KEYS.iter().filter_map(move |key| {
            let value = config?.get(key.key)?;
            Some(CredentialRef {
                field: key.field,
                value,
            })
        })
    }
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
    /// The shared budget this limit draws on, set by the hierarchical merge
    /// when an upstream in the chain has `budget.mode == Shared`. Not
    /// user-facing (never serialized).
    pub shared_pool: Option<SharedPool>,
    /// Whose limit this is: the upstream whose level set the effective
    /// config. Set by the hierarchical merge and never stored; the proxy adds
    /// it to the upstream bucket key when it is not the selected upstream, so
    /// tenants resolving different limits on one upstream get separate
    /// buckets.
    pub limit_owner_id: Option<Uuid>,
}

/// A `budget.mode = shared` pool: every tenant whose limit is merged from the
/// owner draws on one bucket of `total` requests per `window`, next to its
/// own scoped bucket.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedPool {
    /// Upstream that defines the budget.
    pub owner_id: Uuid,
    /// `budget.total` of the owner.
    pub total: u32,
    /// The owner's `sustained.window`.
    pub window: Window,
    /// The owner's algorithm.
    pub algorithm: RateLimitAlgorithm,
}

impl SharedPool {
    /// The pool `rate_limit` defines, owned by `owner_id`, if its budget is
    /// shared.
    #[must_use]
    pub fn defined_by(owner_id: Uuid, rate_limit: &RateLimitConfig) -> Option<Self> {
        let budget = rate_limit.budget.as_ref()?;
        if budget.mode != BudgetMode::Shared {
            return None;
        }
        Some(Self {
            owner_id,
            total: budget.total?,
            window: rate_limit.sustained.window,
            algorithm: rate_limit.algorithm,
        })
    }
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
// CircuitBreakerConfig
// ---------------------------------------------------------------------------

/// Circuit breaker of an upstream (`cpt-cf-oagw-adr-circuit-breaker`). Each
/// replica keeps its own breaker state.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CircuitBreakerConfig {
    /// How the configuration is shared with descendant tenants.
    pub sharing: SharingMode,
    /// Whether requests go through the breaker.
    pub enabled: bool,
    /// Consecutive failures that open the circuit.
    pub failure_threshold: u32,
    /// Seconds the circuit stays open before probes go through.
    pub timeout_seconds: u32,
    /// Probes let through at once after the timeout; all must succeed to
    /// close the circuit.
    pub success_threshold: u32,
    /// The upstream whose level set the effective config. Set by the
    /// hierarchical merge and never stored; tenants that resolve different
    /// configs on one upstream get separate breakers.
    pub owner_id: Option<Uuid>,
}

impl CircuitBreakerConfig {
    /// Largest `failure_threshold`.
    pub const MAX_FAILURE_THRESHOLD: u32 = 1_000;
    /// Largest `success_threshold`, which is also the number of concurrent
    /// half-open probes. `timeout_seconds` is capped by
    /// [`MAX_PROXY_TIMEOUT_SECS`](crate::config::MAX_PROXY_TIMEOUT_SECS).
    pub const MAX_SUCCESS_THRESHOLD: u32 = 100;
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

/// HTTP methods a route can match. Declaration order is the canonical order
/// in which stored routes return their methods. A route matches only the
/// methods it lists: `GET` does not imply `HEAD`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    /// `HEAD`.
    Head,
    /// `OPTIONS`.
    Options,
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

/// Who owns a stored upstream or route. Set on create and never changed.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedBy {
    /// Created through the Management API or the SDK.
    Api,
    /// Provisioned from a types-registry instance. Read-only through the
    /// Management API and the SDK; the registry reconcile keeps it in step.
    Registry,
}

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
    /// Who owns the route (API or types registry).
    pub managed_by: ManagedBy,
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
    /// Upstream circuit breaker.
    pub circuit_breaker: Option<CircuitBreakerConfig>,
    /// Upstream-level CORS configuration.
    pub cors: Option<CorsConfig>,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// Who owns the upstream (API or types registry).
    pub managed_by: ManagedBy,
}

/// What the proxy resolves a request to.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ProxyTarget {
    /// The effective upstream: the tenant merge, with the route's and the
    /// shadowed routes' plugins, CORS and tags. Its rate limit is the
    /// upstreams' only.
    pub upstream: Upstream,
    /// The matched route. Its rate limit is checked on its own bucket.
    pub route: Route,
    /// The enforced rate limits of the ancestor routes `route` shadows,
    /// root-first (`inst-merge-2a`).
    pub shadowed_route_limits: Vec<ShadowedRouteLimit>,
    /// The caller's tenant chain, `[self, parent, ..., root]`, for the
    /// per-request secret owner-chain guard (ADR-0022).
    pub tenant_chain: Vec<Uuid>,
    /// The tenant of the upstream whose auth config is in effect after the
    /// hierarchical merge (`inst-merge-3a0`).
    pub auth_owner_tenant_id: Uuid,
}

#[cfg(test)]
impl ProxyTarget {
    /// The upstream and route, for tests that need only those.
    pub fn into_parts(self) -> (Upstream, Route) {
        (self.upstream, self.route)
    }
}

/// The enforced rate limit of an ancestor's route that a descendant's route
/// shadows: on each upstream above the matched route's, the route its owner
/// would select for the request. The proxy checks it on that route's own
/// bucket, as if it had matched, so it does not change the upstream bucket.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowedRouteLimit {
    /// The shadowed route.
    pub route_id: Uuid,
    /// Its rate limit, `sharing: enforce`.
    pub rate_limit: RateLimitConfig,
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
    /// Upstream circuit breaker.
    pub circuit_breaker: Option<CircuitBreakerConfig>,
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
    /// Upstream circuit breaker.
    pub circuit_breaker: Option<CircuitBreakerConfig>,
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
