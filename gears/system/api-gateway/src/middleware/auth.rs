use axum::http::Method;
use axum::response::IntoResponse;
use std::{collections::HashMap, sync::Arc};

use crate::middleware::common;

use authn_resolver_sdk::{AuthNResolverClient, AuthNResolverError};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

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
        if is_public {
            return AuthRequirement::None;
        }

        if is_authenticated {
            return AuthRequirement::Required;
        }

        if self.require_auth_by_default {
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
        matcher
            .insert(&path)
            .map_err(|e| anyhow::anyhow!("Failed to insert route pattern '{path}': {e}"))?;
    }

    // Build public matchers per HTTP method
    let mut public_matchers_map: HashMap<Method, PublicRouteMatcher> = HashMap::new();

    for (method, path) in public_routes {
        let matcher = public_matchers_map
            .entry(method)
            .or_insert_with(PublicRouteMatcher::new);
        matcher
            .insert(&path)
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
            log_auth_skipped(req.method(), path.as_str());
            req.extensions_mut().insert(SecurityContext::anonymous());
            next.run(req).await
        }
        AuthRequirement::Required => {
            let Some(token) = extract_bearer_token(req.headers()) else {
                log_missing_bearer(req.method(), path.as_str());
                // `instance` / `trace_id` are filled by the canonical
                // error middleware (`toolkit::api::canonical_error_middleware`)
                // on the way out — this middleware sits inside its layer.
                let mut response = CanonicalError::unauthenticated()
                    .with_reason("MISSING_BEARER")
                    .create()
                    .into_response();
                // No bearer credentials were presented (RFC 6750 §3).
                common::append_bearer_challenge(
                    &mut response,
                    common::BearerChallenge::NoCredentials,
                );
                return response;
            };

            match state.authn_client.authenticate(token).await {
                Ok(result) => {
                    log_auth_succeeded(req.method(), path.as_str(), &result.security_context);
                    req.extensions_mut().insert(result.security_context);
                    next.run(req).await
                }
                Err(err) => authn_error_to_response(&err),
            }
        }
    }
}

fn log_auth_skipped(method: &Method, path: &str) {
    tracing::debug!(method = %method, path, "authentication skipped: public route");
}

fn log_missing_bearer(method: &Method, path: &str) {
    tracing::debug!(method = %method, path, "authentication failed: missing bearer token");
}

fn log_auth_succeeded(method: &Method, path: &str, security_context: &SecurityContext) {
    tracing::debug!(
        method = %method,
        path,
        subject_id = %security_context.subject_id(),
        "authentication succeeded"
    );
}

/// Convert `AuthNResolverError` to a canonical Problem Details response.
///
/// `instance` / `trace_id` are filled by the canonical error middleware
/// (`toolkit::api::canonical_error_middleware`) on the way out — this
/// middleware sits inside its layer.
fn authn_error_to_response(err: &AuthNResolverError) -> axum::response::Response {
    log_authn_error(err);
    match err {
        AuthNResolverError::Unauthorized(_) => {
            // A token was presented but rejected (RFC 6750 §3).
            let mut response = CanonicalError::unauthenticated()
                .with_reason("AUTHN_FAILED")
                .create()
                .into_response();
            common::append_bearer_challenge(&mut response, common::BearerChallenge::InvalidToken);
            response
        }
        AuthNResolverError::NoPluginAvailable | AuthNResolverError::ServiceUnavailable(_) => {
            CanonicalError::service_unavailable()
                .with_retry_after_seconds(5)
                .create()
                .into_response()
        }
        AuthNResolverError::TokenAcquisitionFailed(_) | AuthNResolverError::Internal(_) => {
            CanonicalError::internal("authentication infrastructure failure")
                .create()
                .into_response()
        }
    }
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
mod tests {
    use super::*;
    use axum::http::Method;

    /// Helper to build `GatewayRoutePolicy` with given matchers
    fn build_test_policy(
        route_matchers: HashMap<Method, RouteMatcher>,
        public_matchers: HashMap<Method, PublicRouteMatcher>,
        require_auth_by_default: bool,
    ) -> GatewayRoutePolicy {
        GatewayRoutePolicy::new(
            Arc::new(route_matchers),
            Arc::new(public_matchers),
            require_auth_by_default,
        )
    }

    #[test]
    fn test_matchit_router_with_params() {
        let mut router = matchit::Router::new();
        router.insert("/users/{id}", "user_route").unwrap();

        let result = router.at("/users/42");
        assert!(
            result.is_ok(),
            "matchit should match /users/{{id}} against /users/42"
        );
        assert_eq!(*result.unwrap().value, "user_route");
    }

    #[test]
    fn build_route_policy_allows_colon_in_literal_paths() {
        let cfg = crate::config::ApiGatewayConfig::default();
        let authenticated_routes = std::collections::HashSet::from([
            (Method::GET, "events:poll".to_owned()),
            (Method::GET, "events:stream".to_owned()),
        ]);

        if let Err(err) =
            build_route_policy(&cfg, authenticated_routes, std::collections::HashSet::new())
        {
            panic!("literal colon route paths must not be interpreted as path parameters: {err}");
        }
    }

    #[test]
    fn explicit_public_route_with_path_params_returns_none() {
        let mut public_matchers = HashMap::new();
        let mut matcher = PublicRouteMatcher::new();
        matcher.insert("/users/{id}").unwrap();

        public_matchers.insert(Method::GET, matcher);

        let policy = build_test_policy(HashMap::new(), public_matchers, true);

        // Path parameters should match concrete values
        let result = policy.resolve(&Method::GET, "/users/42");
        assert_eq!(result, AuthRequirement::None);
    }

    #[test]
    fn explicit_public_route_exact_match_returns_none() {
        let mut public_matchers = HashMap::new();
        let mut matcher = PublicRouteMatcher::new();
        matcher.insert("/health").unwrap();
        public_matchers.insert(Method::GET, matcher);

        let policy = build_test_policy(HashMap::new(), public_matchers, true);

        let result = policy.resolve(&Method::GET, "/health");
        assert_eq!(result, AuthRequirement::None);
    }

    #[test]
    fn explicit_authenticated_route_returns_required() {
        let mut route_matchers = HashMap::new();
        let mut matcher = RouteMatcher::new();
        matcher.insert("/admin/metrics").unwrap();
        route_matchers.insert(Method::GET, matcher);

        let policy = build_test_policy(route_matchers, HashMap::new(), false);

        let result = policy.resolve(&Method::GET, "/admin/metrics");
        assert_eq!(result, AuthRequirement::Required);
    }

    #[test]
    fn route_without_requirement_with_require_auth_by_default_returns_required() {
        let policy = build_test_policy(HashMap::new(), HashMap::new(), true);

        let result = policy.resolve(&Method::GET, "/profile");
        assert_eq!(result, AuthRequirement::Required);
    }

    #[test]
    fn route_without_requirement_without_require_auth_by_default_returns_none() {
        let policy = build_test_policy(HashMap::new(), HashMap::new(), false);

        let result = policy.resolve(&Method::GET, "/profile");
        assert_eq!(result, AuthRequirement::None);
    }

    #[test]
    fn unknown_route_with_require_auth_by_default_true_returns_required() {
        let policy = build_test_policy(HashMap::new(), HashMap::new(), true);

        let result = policy.resolve(&Method::POST, "/unknown");
        assert_eq!(result, AuthRequirement::Required);
    }

    #[test]
    fn unknown_route_with_require_auth_by_default_false_returns_none() {
        let policy = build_test_policy(HashMap::new(), HashMap::new(), false);

        let result = policy.resolve(&Method::POST, "/unknown");
        assert_eq!(result, AuthRequirement::None);
    }

    #[test]
    fn public_route_overrides_require_auth_by_default() {
        let mut public_matchers = HashMap::new();
        let mut matcher = PublicRouteMatcher::new();
        matcher.insert("/public").unwrap();
        public_matchers.insert(Method::GET, matcher);

        let policy = build_test_policy(HashMap::new(), public_matchers, true);

        let result = policy.resolve(&Method::GET, "/public");
        assert_eq!(result, AuthRequirement::None);
    }

    #[test]
    fn authenticated_route_has_priority_over_default() {
        let mut route_matchers = HashMap::new();
        let mut matcher = RouteMatcher::new();
        matcher.insert("/users/{id}").unwrap();
        route_matchers.insert(Method::GET, matcher);

        let policy = build_test_policy(route_matchers, HashMap::new(), false);

        let result = policy.resolve(&Method::GET, "/users/123");
        assert_eq!(result, AuthRequirement::Required);
    }

    #[test]
    fn explicit_public_overrides_wildcard_authenticated_fallback() {
        // When a gateway registers a wildcard authenticated 404 the fallback
        // like `/{*rest}` (used to convert anonymous 404s to 401s),
        // grabs the public routes too, causing 401 on them
        let mut public_matchers = HashMap::new();
        let mut public_matcher = PublicRouteMatcher::new();
        public_matcher.insert("/v1/auth/config").unwrap();
        public_matchers.insert(Method::GET, public_matcher);

        let mut route_matchers = HashMap::new();
        let mut auth_matcher = RouteMatcher::new();
        auth_matcher.insert("/{*rest}").unwrap();
        route_matchers.insert(Method::GET, auth_matcher);

        let policy = build_test_policy(route_matchers, public_matchers, true);

        assert_eq!(
            policy.resolve(&Method::GET, "/v1/auth/config"),
            AuthRequirement::None,
            "explicit public must win over wildcard authenticated fallback"
        );
        // Sanity: a path that only matches the wildcard fallback still requires auth.
        assert_eq!(
            policy.resolve(&Method::GET, "/some/other/path"),
            AuthRequirement::Required,
            "wildcard authenticated still applies to non-public paths"
        );
    }

    #[test]
    fn different_methods_resolve_independently() {
        let mut route_matchers = HashMap::new();

        // GET /users is authenticated
        let mut get_matcher = RouteMatcher::new();
        get_matcher.insert("/user-management/v1/users").unwrap();
        route_matchers.insert(Method::GET, get_matcher);

        // POST /users is not in matchers
        let policy = build_test_policy(route_matchers, HashMap::new(), false);

        // GET should be authenticated
        let get_result = policy.resolve(&Method::GET, "/user-management/v1/users");
        assert_eq!(get_result, AuthRequirement::Required);

        // POST should be public (no requirement, require_auth_by_default=false)
        let post_result = policy.resolve(&Method::POST, "/user-management/v1/users");
        assert_eq!(post_result, AuthRequirement::None);
    }
}
