//! Public models for the `oagw` module.
//!
//! These are transport-agnostic data structures that define the contract
//! between the `oagw` module and its consumers. No serde derives —
//! serialization concerns belong to the REST layer.

use std::collections::HashMap;

use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared enums
// ---------------------------------------------------------------------------

/// Hierarchical configuration sharing mode.
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

/// Transport scheme for upstream endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scheme {
    Http,
    #[default]
    Https,
    Wss,
    Wt,
    Grpc,
}

/// A single upstream endpoint (scheme + host + port).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub scheme: Scheme,
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    /// Generate the alias contribution for this endpoint.
    /// Standard ports (80, 443) are omitted; non-standard ports are appended as `:port`.
    #[must_use]
    pub fn alias_contribution(&self) -> String {
        if is_standard_port(self.port) {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn is_standard_port(port: u16) -> bool {
    port == 80 || port == 443
}

/// Container for upstream server endpoints.
#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    pub endpoints: Vec<Endpoint>,
}

// ---------------------------------------------------------------------------
// AuthConfig
// ---------------------------------------------------------------------------

/// Authentication plugin configuration for an upstream.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthConfig {
    /// GTS identifier of the auth plugin type.
    pub plugin_type: String,
    pub sharing: SharingMode,
    /// Plugin-specific configuration (flat key-value pairs; schema varies by plugin type).
    pub config: Option<HashMap<String, String>>,
}

// ---------------------------------------------------------------------------
// HeadersConfig
// ---------------------------------------------------------------------------

/// Header transformation rules for request and response.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HeadersConfig {
    pub request: Option<RequestHeaderRules>,
    pub response: Option<ResponseHeaderRules>,
}

/// Header transformation rules for outbound requests.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RequestHeaderRules {
    /// Headers to set (overwrite if exists).
    pub set: HashMap<String, String>,
    /// Headers to add (append, allow duplicates).
    pub add: HashMap<String, String>,
    /// Header names to remove from inbound request.
    pub remove: Vec<String>,
    /// Which inbound headers to forward to upstream.
    pub passthrough: PassthroughMode,
    /// Headers to forward when passthrough is `allowlist`.
    pub passthrough_allowlist: Vec<String>,
}

/// Header transformation rules for upstream responses.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResponseHeaderRules {
    pub set: HashMap<String, String>,
    pub add: HashMap<String, String>,
    pub remove: Vec<String>,
}

/// Controls which inbound headers are forwarded to upstream.
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

/// Rate limiting configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitConfig {
    pub sharing: SharingMode,
    pub algorithm: RateLimitAlgorithm,
    pub sustained: SustainedRate,
    pub burst: Option<BurstConfig>,
    pub scope: RateLimitScope,
    pub strategy: RateLimitStrategy,
    pub cost: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateLimitAlgorithm {
    #[default]
    TokenBucket,
    SlidingWindow,
}

/// Sustained rate configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SustainedRate {
    /// Tokens replenished per window.
    pub rate: u32,
    pub window: Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Window {
    #[default]
    Second,
    Minute,
    Hour,
    Day,
}

/// Burst capacity configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurstConfig {
    /// Maximum burst size. Defaults to sustained.rate if not specified.
    pub capacity: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateLimitScope {
    Global,
    #[default]
    Tenant,
    User,
    Ip,
    Route,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateLimitStrategy {
    #[default]
    Reject,
    Queue,
    Degrade,
}

// ---------------------------------------------------------------------------
// PluginBinding / PluginsConfig
// ---------------------------------------------------------------------------

/// A single plugin binding: reference + optional per-plugin config.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginBinding {
    pub plugin_ref: String,
    pub config: HashMap<String, String>,
}

/// Plugin chain configuration.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PluginsConfig {
    pub sharing: SharingMode,
    /// Plugin bindings: GTS identifiers (builtin) or UUIDs (custom) with optional config.
    pub items: Vec<PluginBinding>,
}

// ---------------------------------------------------------------------------
// CorsConfig
// ---------------------------------------------------------------------------

/// HTTP methods supported by CORS configuration.
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

/// Cross-Origin Resource Sharing (CORS) configuration.
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
// Route matching
// ---------------------------------------------------------------------------

/// HTTP methods supported by route matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Patch,
}

/// How path_suffix from the proxy URL is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathSuffixMode {
    Disabled,
    #[default]
    Append,
}

/// HTTP-protocol match rules for a route.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpMatch {
    /// At least one method required.
    pub methods: Vec<HttpMethod>,
    /// Path prefix (must start with `/`).
    pub path: String,
    /// Allowed query parameters. Empty = allow none.
    pub query_allowlist: Vec<String>,
    pub path_suffix_mode: PathSuffixMode,
}

/// gRPC-protocol match rules for a route (future use).
#[derive(Debug, Clone, PartialEq)]
pub struct GrpcMatch {
    pub service: String,
    pub method: String,
}

/// Protocol-scoped matching rules. Exactly one of `http` or `grpc` must be present.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchRules {
    pub http: Option<HttpMatch>,
    pub grpc: Option<GrpcMatch>,
}

// ---------------------------------------------------------------------------
// Domain entities
// ---------------------------------------------------------------------------

/// A route mapping inbound requests to an upstream endpoint.
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

/// An external upstream service configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Upstream {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub alias: String,
    pub server: Server,
    /// Protocol GTS identifier (e.g. `gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1`).
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

/// Pagination parameters for list queries.
#[derive(Debug, Clone)]
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
// Upstream DTOs
// ---------------------------------------------------------------------------

/// Request for creating an upstream. Construct via [`CreateUpstreamRequest::builder`].
#[derive(Debug, Clone, PartialEq)]
pub struct CreateUpstreamRequest {
    server: Server,
    protocol: String,
    alias: Option<String>,
    auth: Option<AuthConfig>,
    headers: Option<HeadersConfig>,
    plugins: Option<PluginsConfig>,
    rate_limit: Option<RateLimitConfig>,
    cors: Option<CorsConfig>,
    tags: Vec<String>,
    enabled: bool,
}

impl CreateUpstreamRequest {
    /// Start building a new request. `server` and `protocol` are required.
    pub fn builder(server: Server, protocol: impl Into<String>) -> CreateUpstreamRequestBuilder {
        CreateUpstreamRequestBuilder {
            server,
            protocol: protocol.into(),
            alias: None,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            enabled: true,
        }
    }

    pub fn server(&self) -> &Server {
        &self.server
    }
    pub fn protocol(&self) -> &str {
        &self.protocol
    }
    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }
    pub fn auth(&self) -> Option<&AuthConfig> {
        self.auth.as_ref()
    }
    pub fn headers(&self) -> Option<&HeadersConfig> {
        self.headers.as_ref()
    }
    pub fn plugins(&self) -> Option<&PluginsConfig> {
        self.plugins.as_ref()
    }
    pub fn rate_limit(&self) -> Option<&RateLimitConfig> {
        self.rate_limit.as_ref()
    }
    pub fn cors(&self) -> Option<&CorsConfig> {
        self.cors.as_ref()
    }
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

pub struct CreateUpstreamRequestBuilder {
    server: Server,
    protocol: String,
    alias: Option<String>,
    auth: Option<AuthConfig>,
    headers: Option<HeadersConfig>,
    plugins: Option<PluginsConfig>,
    rate_limit: Option<RateLimitConfig>,
    cors: Option<CorsConfig>,
    tags: Vec<String>,
    enabled: bool,
}

impl CreateUpstreamRequestBuilder {
    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.alias = Some(alias.into());
        self
    }
    pub fn auth(mut self, auth: AuthConfig) -> Self {
        self.auth = Some(auth);
        self
    }
    pub fn headers(mut self, headers: HeadersConfig) -> Self {
        self.headers = Some(headers);
        self
    }
    pub fn plugins(mut self, plugins: PluginsConfig) -> Self {
        self.plugins = Some(plugins);
        self
    }
    pub fn rate_limit(mut self, rate_limit: RateLimitConfig) -> Self {
        self.rate_limit = Some(rate_limit);
        self
    }
    pub fn cors(mut self, cors: CorsConfig) -> Self {
        self.cors = Some(cors);
        self
    }
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    pub fn build(self) -> CreateUpstreamRequest {
        CreateUpstreamRequest {
            server: self.server,
            protocol: self.protocol,
            alias: self.alias,
            auth: self.auth,
            headers: self.headers,
            plugins: self.plugins,
            rate_limit: self.rate_limit,
            cors: self.cors,
            tags: self.tags,
            enabled: self.enabled,
        }
    }
}

/// Request for replacing an upstream (PUT semantics). Construct via
/// [`UpdateUpstreamRequest::builder`].
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateUpstreamRequest {
    server: Server,
    protocol: String,
    alias: Option<String>,
    auth: Option<AuthConfig>,
    headers: Option<HeadersConfig>,
    plugins: Option<PluginsConfig>,
    rate_limit: Option<RateLimitConfig>,
    cors: Option<CorsConfig>,
    tags: Vec<String>,
    enabled: bool,
}

impl UpdateUpstreamRequest {
    /// Start building an update request. `server` and `protocol` are required.
    pub fn builder(server: Server, protocol: impl Into<String>) -> UpdateUpstreamRequestBuilder {
        UpdateUpstreamRequestBuilder {
            server,
            protocol: protocol.into(),
            alias: None,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            enabled: true,
        }
    }

    pub fn server(&self) -> &Server {
        &self.server
    }
    pub fn protocol(&self) -> &str {
        &self.protocol
    }
    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }
    pub fn auth(&self) -> Option<&AuthConfig> {
        self.auth.as_ref()
    }
    pub fn headers(&self) -> Option<&HeadersConfig> {
        self.headers.as_ref()
    }
    pub fn plugins(&self) -> Option<&PluginsConfig> {
        self.plugins.as_ref()
    }
    pub fn rate_limit(&self) -> Option<&RateLimitConfig> {
        self.rate_limit.as_ref()
    }
    pub fn cors(&self) -> Option<&CorsConfig> {
        self.cors.as_ref()
    }
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

pub struct UpdateUpstreamRequestBuilder {
    server: Server,
    protocol: String,
    alias: Option<String>,
    auth: Option<AuthConfig>,
    headers: Option<HeadersConfig>,
    plugins: Option<PluginsConfig>,
    rate_limit: Option<RateLimitConfig>,
    cors: Option<CorsConfig>,
    tags: Vec<String>,
    enabled: bool,
}

impl UpdateUpstreamRequestBuilder {
    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.alias = Some(alias.into());
        self
    }
    pub fn auth(mut self, auth: AuthConfig) -> Self {
        self.auth = Some(auth);
        self
    }
    pub fn headers(mut self, headers: HeadersConfig) -> Self {
        self.headers = Some(headers);
        self
    }
    pub fn plugins(mut self, plugins: PluginsConfig) -> Self {
        self.plugins = Some(plugins);
        self
    }
    pub fn rate_limit(mut self, rate_limit: RateLimitConfig) -> Self {
        self.rate_limit = Some(rate_limit);
        self
    }
    pub fn cors(mut self, cors: CorsConfig) -> Self {
        self.cors = Some(cors);
        self
    }
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    pub fn build(self) -> UpdateUpstreamRequest {
        UpdateUpstreamRequest {
            server: self.server,
            protocol: self.protocol,
            alias: self.alias,
            auth: self.auth,
            headers: self.headers,
            plugins: self.plugins,
            rate_limit: self.rate_limit,
            cors: self.cors,
            tags: self.tags,
            enabled: self.enabled,
        }
    }
}

// ---------------------------------------------------------------------------
// Route DTOs
// ---------------------------------------------------------------------------

/// Request for creating a route. Construct via [`CreateRouteRequest::builder`].
#[derive(Debug, Clone, PartialEq)]
pub struct CreateRouteRequest {
    upstream_id: Uuid,
    match_rules: MatchRules,
    plugins: Option<PluginsConfig>,
    rate_limit: Option<RateLimitConfig>,
    cors: Option<CorsConfig>,
    tags: Vec<String>,
    priority: i32,
    enabled: bool,
}

impl CreateRouteRequest {
    /// Start building a new route request. `upstream_id` and `match_rules` are required.
    pub fn builder(upstream_id: Uuid, match_rules: MatchRules) -> CreateRouteRequestBuilder {
        CreateRouteRequestBuilder {
            upstream_id,
            match_rules,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
        }
    }

    pub fn upstream_id(&self) -> Uuid {
        self.upstream_id
    }
    pub fn match_rules(&self) -> &MatchRules {
        &self.match_rules
    }
    pub fn plugins(&self) -> Option<&PluginsConfig> {
        self.plugins.as_ref()
    }
    pub fn rate_limit(&self) -> Option<&RateLimitConfig> {
        self.rate_limit.as_ref()
    }
    pub fn cors(&self) -> Option<&CorsConfig> {
        self.cors.as_ref()
    }
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
    pub fn priority(&self) -> i32 {
        self.priority
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

pub struct CreateRouteRequestBuilder {
    upstream_id: Uuid,
    match_rules: MatchRules,
    plugins: Option<PluginsConfig>,
    rate_limit: Option<RateLimitConfig>,
    cors: Option<CorsConfig>,
    tags: Vec<String>,
    priority: i32,
    enabled: bool,
}

impl CreateRouteRequestBuilder {
    pub fn plugins(mut self, plugins: PluginsConfig) -> Self {
        self.plugins = Some(plugins);
        self
    }
    pub fn rate_limit(mut self, rate_limit: RateLimitConfig) -> Self {
        self.rate_limit = Some(rate_limit);
        self
    }
    pub fn cors(mut self, cors: CorsConfig) -> Self {
        self.cors = Some(cors);
        self
    }
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }
    pub fn priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    pub fn build(self) -> CreateRouteRequest {
        CreateRouteRequest {
            upstream_id: self.upstream_id,
            match_rules: self.match_rules,
            plugins: self.plugins,
            rate_limit: self.rate_limit,
            cors: self.cors,
            tags: self.tags,
            priority: self.priority,
            enabled: self.enabled,
        }
    }
}

/// Request for replacing a route (PUT semantics). Construct via
/// [`UpdateRouteRequest::builder`].
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateRouteRequest {
    match_rules: MatchRules,
    plugins: Option<PluginsConfig>,
    rate_limit: Option<RateLimitConfig>,
    cors: Option<CorsConfig>,
    tags: Vec<String>,
    priority: i32,
    enabled: bool,
}

impl UpdateRouteRequest {
    /// Start building an update request. `match_rules` is required.
    pub fn builder(match_rules: MatchRules) -> UpdateRouteRequestBuilder {
        UpdateRouteRequestBuilder {
            match_rules,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
        }
    }

    pub fn match_rules(&self) -> &MatchRules {
        &self.match_rules
    }
    pub fn plugins(&self) -> Option<&PluginsConfig> {
        self.plugins.as_ref()
    }
    pub fn rate_limit(&self) -> Option<&RateLimitConfig> {
        self.rate_limit.as_ref()
    }
    pub fn cors(&self) -> Option<&CorsConfig> {
        self.cors.as_ref()
    }
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
    pub fn priority(&self) -> i32 {
        self.priority
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

pub struct UpdateRouteRequestBuilder {
    match_rules: MatchRules,
    plugins: Option<PluginsConfig>,
    rate_limit: Option<RateLimitConfig>,
    cors: Option<CorsConfig>,
    tags: Vec<String>,
    priority: i32,
    enabled: bool,
}

impl UpdateRouteRequestBuilder {
    pub fn plugins(mut self, plugins: PluginsConfig) -> Self {
        self.plugins = Some(plugins);
        self
    }
    pub fn rate_limit(mut self, rate_limit: RateLimitConfig) -> Self {
        self.rate_limit = Some(rate_limit);
        self
    }
    pub fn cors(mut self, cors: CorsConfig) -> Self {
        self.cors = Some(cors);
        self
    }
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }
    pub fn priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    pub fn build(self) -> UpdateRouteRequest {
        UpdateRouteRequest {
            match_rules: self.match_rules,
            plugins: self.plugins,
            rate_limit: self.rate_limit,
            cors: self.cors,
            tags: self.tags,
            priority: self.priority,
            enabled: self.enabled,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "models_tests.rs"]
mod models_tests;
