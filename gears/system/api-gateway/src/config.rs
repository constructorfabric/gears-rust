use serde::{Deserialize, Serialize};

fn default_require_auth_by_default() -> bool {
    true
}

fn default_body_limit_bytes() -> usize {
    16 * 1024 * 1024
}

/// API gateway configuration - reused from `api_gateway` gear
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
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

    /// Optional filesystem path to a built admin-panel SPA (`dist/`).
    /// When set, the gateway serves these static assets at `{prefix_path}/admin`
    /// (e.g. `/cf/admin`) directly from disk, OUTSIDE the auth middleware stack
    /// (the SPA itself is public; its API calls carry bearer tokens).
    /// Unset (the default) means the admin panel is not served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_spa_dir: Option<String>,
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
