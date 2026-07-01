use serde::{Deserialize, Serialize};

fn default_require_auth_by_default() -> bool {
    true
}

fn default_body_limit_bytes() -> usize {
    16 * 1024 * 1024
}

fn default_healthcheck_timeout_ms() -> u64 {
    500
}

/// API gateway configuration - reused from `api_gateway` gear
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct ApiGatewayConfig {
    pub bind_addr: String,
    #[serde(default)]
    pub enable_docs: bool,
    #[serde(default)]
    pub cors_enabled: bool,
    /// Optional detailed CORS configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cors: Option<CorsConfig>,

    /// `OpenAPI` document metadata
    #[serde(default)]
    pub openapi: OpenApiConfig,

    /// Global defaults
    #[serde(default)]
    pub defaults: Defaults,

    /// Disable authentication and authorization completely.
    /// When true, middleware automatically injects a default `SecurityContext` for all requests,
    /// providing access with no tenant filtering.
    /// This bypasses all tenant isolation and should only be used for single-user on-premise installations.
    /// Default: false (authentication required via `AuthN` Resolver).
    #[serde(default)]
    pub auth_disabled: bool,

    /// If true, routes without explicit security requirement still require authentication (AuthN-only).
    #[serde(default = "default_require_auth_by_default")]
    pub require_auth_by_default: bool,

    /// Optional URL path prefix prepended to every route (e.g. `"/cf"` → `/cf/users`).
    /// Must start with a leading slash; trailing slashes are stripped automatically.
    /// Empty string (the default) means no prefix.
    #[serde(default)]
    pub prefix_path: String,

    /// Route-level policy configuration.
    /// Allows early rejection of requests based on token scopes without calling the PDP.
    /// Rules are evaluated in declaration order (first match wins).
    #[serde(default)]
    pub route_policies: RoutePoliciesConfig,

    /// HTTP metrics configuration.
    #[serde(default)]
    pub metrics: MetricsConfig,

    /// Per-check `/health`/`/readyz` timeout (ms); raise for slow dependencies.
    #[serde(default = "default_healthcheck_timeout_ms")]
    pub healthcheck_timeout_ms: u64,

    /// Health-probe serving configuration (listener placement, bind address).
    #[serde(default)]
    pub health: HealthConfig,
}

/// Which listener(s) serve the health-probe endpoints (`/healthz`, `/readyz`, `/health`).
///
/// Health endpoints are unauthenticated in every mode; see [`HealthConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HealthServeMode {
    /// Serve health endpoints on the main gateway listener (`bind_addr`). Default.
    #[default]
    Main,
    /// Serve health endpoints only on a separate listener (`health.bind_addr`), e.g. a
    /// private/management port not exposed alongside the main API.
    Separate,
    /// Serve health endpoints on both the main and the separate listener.
    Both,
}

/// Health-probe serving configuration.
///
/// The endpoints (`/healthz`, `/readyz`, `/health`) never require authentication in any mode.
/// In `main`/`both` mode they ride the main listener as public routes on the gateway
/// middleware surface and inherit `prefix_path` (e.g. `/cf/healthz`); operators must keep the
/// main gateway on a private network if the per-component detail in `/health` is sensitive. In
/// `separate`/`both` mode they are also served at unprefixed paths on `bind_addr`, protected
/// only by network placement of that listener.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct HealthConfig {
    /// Which listener(s) serve the health endpoints.
    pub serve: HealthServeMode,
    /// Bind address for the separate health listener (e.g. `"0.0.0.0:8081"`).
    /// Required when `serve` is `separate` or `both`; unused for `main`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_addr: Option<String>,
}

impl Default for ApiGatewayConfig {
    // Manual (not derived) so defaults match the `#[serde(default = ...)]` fns above;
    // a derived Default would zero each field (e.g. timeout 0 = instant probe failure).
    fn default() -> Self {
        Self {
            bind_addr: String::default(),
            enable_docs: false,
            cors_enabled: false,
            cors: None,
            openapi: OpenApiConfig::default(),
            defaults: Defaults::default(),
            auth_disabled: false,
            require_auth_by_default: default_require_auth_by_default(),
            prefix_path: String::default(),
            route_policies: RoutePoliciesConfig::default(),
            metrics: MetricsConfig::default(),
            healthcheck_timeout_ms: default_healthcheck_timeout_ms(),
            health: HealthConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Defaults {
    /// Fallback rate limit when operation does not specify one
    pub rate_limit: RateLimitDefaults,
    /// Global request body size limit in bytes
    pub body_limit_bytes: usize,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            rate_limit: RateLimitDefaults::default(),
            body_limit_bytes: default_body_limit_bytes(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct RateLimitDefaults {
    pub rps: u32,
    pub burst: u32,
    pub in_flight: u32,
}

impl Default for RateLimitDefaults {
    fn default() -> Self {
        Self {
            rps: 50,
            burst: 100,
            in_flight: 64,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct CorsConfig {
    /// Allowed origins: `["*"]` means any
    pub allowed_origins: Vec<String>,
    /// Allowed HTTP methods, e.g. `["GET","POST","OPTIONS","PUT","DELETE","PATCH"]`
    pub allowed_methods: Vec<String>,
    /// Allowed request headers; `["*"]` means any
    pub allowed_headers: Vec<String>,
    /// Response headers exposed to browser JavaScript via
    /// `Access-Control-Expose-Headers`; `["*"]` means any (do not combine
    /// with `allow_credentials` — the wildcard is invalid with credentials).
    /// Defaults to `["ETag"]`: the optimistic-concurrency write contract
    /// requires clients to read the `ETag` response header to build
    /// `If-Match` guarded writes, and `ETag` is not on the CORS safelist —
    /// without exposing it every ETag-guarded write is impossible from a
    /// cross-origin browser client.
    pub exposed_headers: Vec<String>,
    /// Whether to allow credentials
    pub allow_credentials: bool,
    /// Max age for preflight caching in seconds
    pub max_age_seconds: u64,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: vec!["*".to_owned()],
            allowed_methods: vec![
                "GET".to_owned(),
                "POST".to_owned(),
                "PUT".to_owned(),
                "PATCH".to_owned(),
                "DELETE".to_owned(),
                "OPTIONS".to_owned(),
            ],
            allowed_headers: vec!["*".to_owned()],
            exposed_headers: vec!["ETag".to_owned()],
            allow_credentials: false,
            max_age_seconds: 600,
        }
    }
}

/// HTTP metrics configuration.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct MetricsConfig {
    /// Optional prefix for HTTP metrics instrument names.
    ///
    /// When set, metric names become `{prefix}.http.server.request.duration`
    /// and `{prefix}.http.server.active_requests` instead of the default
    /// OpenTelemetry semantic convention names.
    ///
    /// Empty string (the default) means no prefix — standard `OTel` names are used.
    pub prefix: String,
}

/// `OpenAPI` document metadata configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct OpenApiConfig {
    /// API title shown in `OpenAPI` documentation
    pub title: String,
    /// API version
    pub version: String,
    /// API description (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Default for OpenApiConfig {
    fn default() -> Self {
        Self {
            title: "API Documentation".to_owned(),
            version: "0.1.0".to_owned(),
            description: None,
        }
    }
}

/// Route-level policy configuration.
///
/// Enables coarse-grained early rejection of requests based on token scopes
/// without calling the PDP. This is an optimization for performance-critical routes.
///
/// # Example YAML
///
/// ```yaml
/// route_policies:
///   enabled: true
///   rules:
///     - path: "/admin/**"
///       required_scopes: ["admin"]
///     - path: "/events/v1/*"
///       required_scopes: ["read:events", "write:events"]  # any of these
/// ```
///
/// # Behavior
///
/// - Rules are evaluated in declaration order (first match wins)
/// - If `token_scopes: ["*"]` → always pass (first-party app)
/// - If `token_scopes` contains any of `required_scopes` → pass
/// - Otherwise → 403 Forbidden (before PDP call)
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct RoutePoliciesConfig {
    /// Whether route policy enforcement is enabled.
    pub enabled: bool,
    /// Route policy rules evaluated in declaration order.
    /// Patterns support glob syntax (e.g., `/admin/*`, `/events/v1/**`).
    pub rules: Vec<RoutePolicyRule>,
}

/// A single route policy rule.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoutePolicyRule {
    /// Path pattern to match. Supports glob syntax (`*` = one segment, `**` = any depth).
    pub path: String,
    /// HTTP method to match (GET, POST, PUT, PATCH, DELETE, etc.).
    /// If not specified, matches any method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Required scopes for this route. Request passes if token has ANY of these scopes.
    /// Must not be empty.
    pub required_scopes: Vec<String>,
    // Future fields: rate_limit, timeout, operation_id, etc.
}
