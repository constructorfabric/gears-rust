use axum::http::Method;
use axum::response::IntoResponse;
use std::{collections::HashMap, sync::Arc};

use crate::middleware::common;

use authn_resolver_sdk::{AuthNResolverClient, AuthNResolverError};
use modkit::api::Problem;
use modkit_security::SecurityContext;

/// Route matcher for a specific HTTP method (authenticated routes).
#[derive(Clone)]
pub struct RouteMatcher {
    matcher: matchit::Router<()>,
}

impl RouteMatcher {
    fn new() -> Self {
        Self {
            matcher: matchit::Router::new(),
        }
    }

    fn insert(&mut self, path: &str) -> Result<(), matchit::InsertError> {
        self.matcher.insert(path, ())
    }

    fn find(&self, path: &str) -> bool {
        self.matcher.at(path).is_ok()
    }
}

/// Public route matcher for explicitly public routes
#[derive(Clone)]
pub struct PublicRouteMatcher {
    matcher: matchit::Router<()>,
}

impl PublicRouteMatcher {
    fn new() -> Self {
        Self {
            matcher: matchit::Router::new(),
        }
    }

    fn insert(&mut self, path: &str) -> Result<(), matchit::InsertError> {
        self.matcher.insert(path, ())
    }

    fn find(&self, path: &str) -> bool {
        self.matcher.at(path).is_ok()
    }
}

/// Convert Axum path syntax `:param` to matchit syntax `{param}`
///
/// Axum uses `:id` for path parameters, but matchit 0.8 uses `{id}`.
/// This function converts between the two syntaxes.
fn convert_axum_path_to_matchit(path: &str) -> String {
    // Simple regex-free approach: find :word and replace with {word}
    let mut result = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == ':' {
            // Start of a parameter - collect the parameter name
            result.push('{');
            while matches!(chars.peek(), Some(c) if c.is_alphanumeric() || *c == '_') {
                if let Some(c) = chars.next() {
                    result.push(c);
                }
            }
            result.push('}');
        } else {
            result.push(ch);
        }
    }

    result
}

/// Whether a route requires authentication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthRequirement {
    /// No authentication required (public route).
    None,
    /// Authentication required.
    Required,
}

/// Gateway-specific route policy implementation
#[derive(Clone)]
pub struct GatewayRoutePolicy {
    route_matchers: Arc<HashMap<Method, RouteMatcher>>,
    public_matchers: Arc<HashMap<Method, PublicRouteMatcher>>,
    require_auth_by_default: bool,
}

impl GatewayRoutePolicy {
    #[must_use]
    pub fn new(
        route_matchers: Arc<HashMap<Method, RouteMatcher>>,
        public_matchers: Arc<HashMap<Method, PublicRouteMatcher>>,
        require_auth_by_default: bool,
    ) -> Self {
        Self {
            route_matchers,
            public_matchers,
            require_auth_by_default,
        }
    }

    /// Resolve the authentication requirement for a given (method, path).
    #[must_use]
    pub fn resolve(&self, method: &Method, path: &str) -> AuthRequirement {
        // Check if route is explicitly authenticated
        let is_authenticated = self
            .route_matchers
            .get(method)
            .is_some_and(|matcher| matcher.find(path));

        // Check if route is explicitly public using pattern matching
        let is_public = self
            .public_matchers
            .get(method)
            .is_some_and(|matcher| matcher.find(path));

        // Public routes should not be forced to auth by default
        let needs_authn = is_authenticated || (self.require_auth_by_default && !is_public);

        if needs_authn {
            AuthRequirement::Required
        } else {
            AuthRequirement::None
        }
    }
}

/// Shared state for the authentication middleware.
#[derive(Clone)]
pub struct AuthState {
    pub authn_client: Arc<dyn AuthNResolverClient>,
    pub route_policy: GatewayRoutePolicy,
}

/// Helper to build `GatewayRoutePolicy` from operation requirements.
///
/// # Errors
///
/// Returns an error if a route pattern cannot be inserted into the matcher.
#[allow(clippy::implicit_hasher)]
pub fn build_route_policy(
    cfg: &crate::config::ApiGatewayConfig,
    authenticated_routes: std::collections::HashSet<(Method, String)>,
    public_routes: std::collections::HashSet<(Method, String)>,
) -> Result<GatewayRoutePolicy, anyhow::Error> {
    // Build route matchers per HTTP method (authenticated routes)
    let mut route_matchers_map: HashMap<Method, RouteMatcher> = HashMap::new();

    for (method, path) in authenticated_routes {
        let matcher = route_matchers_map
            .entry(method)
            .or_insert_with(RouteMatcher::new);
        // Convert Axum path syntax (:param) to matchit syntax ({param})
        let matchit_path = convert_axum_path_to_matchit(&path);
        matcher
            .insert(&matchit_path)
            .map_err(|e| anyhow::anyhow!("Failed to insert route pattern '{path}': {e}"))?;
    }

    // Build public matchers per HTTP method
    let mut public_matchers_map: HashMap<Method, PublicRouteMatcher> = HashMap::new();

    for (method, path) in public_routes {
        let matcher = public_matchers_map
            .entry(method)
            .or_insert_with(PublicRouteMatcher::new);
        // Convert Axum path syntax (:param) to matchit syntax ({param})
        let matchit_path = convert_axum_path_to_matchit(&path);
        matcher
            .insert(&matchit_path)
            .map_err(|e| anyhow::anyhow!("Failed to insert public route pattern '{path}': {e}"))?;
    }

    Ok(GatewayRoutePolicy::new(
        Arc::new(route_matchers_map),
        Arc::new(public_matchers_map),
        cfg.require_auth_by_default,
    ))
}

/// Authentication middleware that uses the `AuthN` Resolver to validate bearer tokens.
///
/// For each request:
/// 1. Skips CORS preflight requests
/// 2. Resolves the route's auth requirement via `GatewayRoutePolicy`
/// 3. For public routes: inserts anonymous `SecurityContext`
/// 4. For required routes: extracts bearer token, calls `AuthN` Resolver, inserts `SecurityContext`
pub async fn authn_middleware(
    axum::extract::State(state): axum::extract::State<AuthState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Skip CORS preflight — insert anonymous SecurityContext so downstream
    // handlers that extract Extension<SecurityContext> don't panic.
    if is_preflight_request(req.method(), req.headers()) {
        req.extensions_mut().insert(SecurityContext::anonymous());
        return next.run(req).await;
    }

    let path = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or_else(|| req.uri().path().to_owned(), |p| p.as_str().to_owned());

    let path = common::resolve_path(&req, path.as_str());

    let requirement = state.route_policy.resolve(req.method(), path.as_str());

    match requirement {
        AuthRequirement::None => {
            req.extensions_mut().insert(SecurityContext::anonymous());
            next.run(req).await
        }
        AuthRequirement::Required => {
            let Some(token) = extract_bearer_token(req.headers()) else {
                return Problem::new(
                    axum::http::StatusCode::UNAUTHORIZED,
                    "Unauthorized",
                    "Missing or invalid Authorization header",
                )
                .into_response();
            };

            match state.authn_client.authenticate(token).await {
                Ok(result) => {
                    req.extensions_mut().insert(result.security_context);
                    next.run(req).await
                }
                Err(err) => authn_error_to_response(&err),
            }
        }
    }
}

/// Convert `AuthNResolverError` to an RFC-9457 Problem Details response.
fn authn_error_to_response(err: &AuthNResolverError) -> axum::response::Response {
    log_authn_error(err);
    let (status, title, detail) = match err {
        AuthNResolverError::Unauthorized(_) => (
            axum::http::StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "Authentication failed",
        ),
        AuthNResolverError::NoPluginAvailable | AuthNResolverError::ServiceUnavailable(_) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "Authentication service unavailable",
        ),
        AuthNResolverError::TokenAcquisitionFailed(_) | AuthNResolverError::Internal(_) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "Internal authentication error",
        ),
    };
    Problem::new(status, title, detail).into_response()
}

/// Log authentication errors at appropriate levels.
///
/// Cognitive complexity is inflated by tracing macro expansion.
#[allow(clippy::cognitive_complexity)]
fn log_authn_error(err: &AuthNResolverError) {
    match err {
        AuthNResolverError::Unauthorized(msg) => tracing::debug!("AuthN rejected: {msg}"),
        AuthNResolverError::NoPluginAvailable => tracing::error!("No AuthN plugin available"),
        AuthNResolverError::ServiceUnavailable(msg) => {
            tracing::error!("AuthN service unavailable: {msg}");
        }
        AuthNResolverError::TokenAcquisitionFailed(msg) => {
            tracing::error!("AuthN token acquisition failed: {msg}");
        }
        AuthNResolverError::Internal(msg) => tracing::error!("AuthN internal error: {msg}"),
    }
}

/// Extract Bearer token from Authorization header
fn extract_bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").map(str::trim))
}

/// Check if this is a CORS preflight request
///
/// Preflight requests are OPTIONS requests with:
/// - Origin header present
/// - Access-Control-Request-Method header present
fn is_preflight_request(method: &Method, headers: &axum::http::HeaderMap) -> bool {
    method == Method::OPTIONS
        && headers.contains_key(axum::http::header::ORIGIN)
        && headers.contains_key(axum::http::header::ACCESS_CONTROL_REQUEST_METHOD)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "auth_tests.rs"]
mod auth_tests;
