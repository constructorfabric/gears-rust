use axum::http::Method;
use axum::response::IntoResponse;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::middleware::common;

use authn_resolver_sdk::{AuthNResolverClient, AuthNResolverError};
use secrecy::ExposeSecret;
use toolkit_canonical_errors::CanonicalError;
use toolkit_gateway::ProxyRegistry;
use toolkit_http_middleware::{admit_platform, extract_bearer_http};
use toolkit_security::{RouteAuth, SecurityContext};

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

/// Route matcher for anonymous (unauthenticated) routes.
///
/// "Anonymous" is the auth axis, distinct from external *visibility*: a route
/// in here requires no bearer token. Do not conflate with `exposed`
/// (visibility) elsewhere in the codebase.
#[derive(Clone)]
pub struct AnonymousRouteMatcher {
    matcher: matchit::Router<()>,
}

impl AnonymousRouteMatcher {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthRequirement {
    /// No authentication required (public route).
    None,
    /// Authentication required.
    Required,
    /// Requires a validated `X-ToolKit-Internal-Token`; a presented bearer is validated but cannot
    /// admit the caller alone.
    Platform,
}

/// Request-scoped auth decision shared by the inner layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedRequirement(pub(crate) AuthRequirement);

/// Inserted only by [`platform_gate_middleware`] after platform admission; permits tenant
/// scope checks to be bypassed. The private field keeps construction inside this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlatformAdmitted(());

/// Gateway-specific route policy implementation
#[derive(Clone)]
pub struct GatewayRoutePolicy {
    route_matchers: Arc<HashMap<Method, RouteMatcher>>,
    anonymous_matchers: Arc<HashMap<Method, AnonymousRouteMatcher>>,
    /// Exact platform route templates: `/items/{id}` must not capture a declared `/items/special`.
    platform_routes: Arc<HashMap<Method, HashSet<String>>>,
    require_auth_by_default: bool,
    /// Reverse-proxy route table (embedded edge). When set, dynamically-
    /// registered proxy routes are resolved against it so each is enforced
    /// exactly as the owning gear declared. `None` when the proxy is disabled.
    proxy_registry: Option<Arc<ProxyRegistry>>,
}

impl GatewayRoutePolicy {
    #[must_use]
    pub fn new(
        route_matchers: Arc<HashMap<Method, RouteMatcher>>,
        anonymous_matchers: Arc<HashMap<Method, AnonymousRouteMatcher>>,
        require_auth_by_default: bool,
        proxy_registry: Option<Arc<ProxyRegistry>>,
    ) -> Self {
        Self {
            route_matchers,
            anonymous_matchers,
            platform_routes: Arc::new(HashMap::new()),
            require_auth_by_default,
            proxy_registry,
        }
    }

    /// The request’s cached auth requirement, or a fresh resolution.
    #[must_use]
    pub fn requirement_of(&self, req: &axum::extract::Request) -> AuthRequirement {
        req.extensions()
            .get::<ResolvedRequirement>()
            .map_or_else(|| self.resolve(req.method(), &policy_path(req)), |r| r.0)
    }

    /// Platform templates take precedence over other auth declarations.
    #[must_use]
    pub fn with_platform_routes(
        mut self,
        platform_routes: Arc<HashMap<Method, HashSet<String>>>,
    ) -> Self {
        self.platform_routes = platform_routes;
        self
    }

    /// Resolve method/path auth; undeclared HEAD inherits GET.
    #[must_use]
    pub fn resolve(&self, method: &Method, path: &str) -> AuthRequirement {
        self.resolve_declared(method, path)
            .or_else(|| {
                (*method == Method::HEAD)
                    .then(|| self.resolve_declared(&Method::GET, path))
                    .flatten()
            })
            .unwrap_or(if self.require_auth_by_default {
                AuthRequirement::Required
            } else {
                AuthRequirement::None
            })
    }

    /// The requirement declared for exactly `method` on `path`, if any.
    fn resolve_declared(&self, method: &Method, path: &str) -> Option<AuthRequirement> {
        // Platform-only first: no other declaration may relax it.
        if self
            .platform_routes
            .get(method)
            .is_some_and(|paths| paths.contains(path))
        {
            return Some(AuthRequirement::Platform);
        }

        // Check if route is explicitly authenticated
        let is_authenticated = self
            .route_matchers
            .get(method)
            .is_some_and(|matcher| matcher.find(path));

        // Check if the route is explicitly anonymous (no auth) using pattern
        // matching. This is the auth axis, not external visibility.
        let is_anonymous = self
            .anonymous_matchers
            .get(method)
            .is_some_and(|matcher| matcher.find(path));

        // Anonymous routes should not be forced to auth by default
        if is_anonymous {
            return Some(AuthRequirement::None);
        }

        // Dynamically-registered proxy routes are not in the static matchers.
        // Consult the reverse-proxy registry so each proxied route is enforced
        // exactly as the owning gear declared (authenticated vs anonymous).
        if let Some(registry) = &self.proxy_registry
            && let Some(authenticated) = registry.requires_auth(method, path)
        {
            return Some(if authenticated {
                AuthRequirement::Required
            } else {
                AuthRequirement::None
            });
        }

        // Statically-registered routes are the fallback when no proxy entry exists.
        is_authenticated.then_some(AuthRequirement::Required)
    }
}

/// Shared state for the authentication middleware.
#[derive(Clone)]
pub struct AuthState {
    pub authn_client: Arc<dyn AuthNResolverClient>,
    pub route_policy: GatewayRoutePolicy,
}

/// Helper to build `GatewayRoutePolicy` from per-route auth declarations.
///
/// Duplicate `(method, path)` entries are deduplicated per auth kind; a platform
/// declaration wins over any other declaration of the same route.
///
/// # Errors
///
/// Returns an error if a route pattern cannot be inserted into the matcher.
pub fn build_route_policy(
    cfg: &crate::config::ApiGatewayConfig,
    routes: impl IntoIterator<Item = (Method, String, RouteAuth)>,
    proxy_registry: Option<Arc<ProxyRegistry>>,
) -> Result<GatewayRoutePolicy, anyhow::Error> {
    let mut authenticated_routes: HashMap<Method, HashSet<String>> = HashMap::new();
    let mut anonymous_routes: HashMap<Method, HashSet<String>> = HashMap::new();
    let mut platform_routes_map: HashMap<Method, HashSet<String>> = HashMap::new();
    for (method, path, auth) in routes {
        let by_kind = match auth {
            RouteAuth::Authenticated => &mut authenticated_routes,
            RouteAuth::Anonymous => &mut anonymous_routes,
            RouteAuth::Platform => &mut platform_routes_map,
        };
        by_kind.entry(method).or_default().insert(path);
    }

    // Build route matchers per HTTP method (authenticated routes)
    let mut route_matchers_map: HashMap<Method, RouteMatcher> = HashMap::new();

    for (method, paths) in authenticated_routes {
        let matcher = route_matchers_map
            .entry(method)
            .or_insert_with(RouteMatcher::new);
        for path in paths {
            matcher
                .insert(&path)
                .map_err(|e| anyhow::anyhow!("Failed to insert route pattern '{path}': {e}"))?;
        }
    }

    // Build anonymous matchers per HTTP method
    let mut anonymous_matchers_map: HashMap<Method, AnonymousRouteMatcher> = HashMap::new();

    for (method, paths) in anonymous_routes {
        let matcher = anonymous_matchers_map
            .entry(method)
            .or_insert_with(AnonymousRouteMatcher::new);
        for path in paths {
            matcher.insert(&path).map_err(|e| {
                anyhow::anyhow!("Failed to insert anonymous route pattern '{path}': {e}")
            })?;
        }
    }

    Ok(GatewayRoutePolicy::new(
        Arc::new(route_matchers_map),
        Arc::new(anonymous_matchers_map),
        cfg.require_auth_by_default,
        proxy_registry,
    )
    .with_platform_routes(Arc::new(platform_routes_map)))
}

/// Matched route template with `prefix_path` removed; borrows the template before allocating.
pub(crate) fn policy_path(req: &axum::extract::Request) -> String {
    let path = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or_else(|| req.uri().path(), axum::extract::MatchedPath::as_str);
    common::resolve_path(req, path)
}

/// Enforce [`admit_platform`] even with `auth_disabled`; mark success with [`PlatformAdmitted`].
pub async fn platform_gate_middleware(
    axum::extract::State(route_policy): axum::extract::State<GatewayRoutePolicy>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Skip resolution entirely when no platform route is declared.
    let platform = !route_policy.platform_routes.is_empty()
        && route_policy.requirement_of(&req) == AuthRequirement::Platform;
    if !platform {
        return next.run(req).await;
    }
    if let Err(refusal) = admit_platform(&req) {
        return refusal.into_response();
    }
    req.extensions_mut().insert(PlatformAdmitted(()));
    next.run(req).await
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

    let path = policy_path(&req);

    let requirement = state.route_policy.resolve(req.method(), path.as_str());
    req.extensions_mut()
        .insert(ResolvedRequirement(requirement));

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
        // Validate presented bearers; only the internal token admits a platform caller.
        // The validated tenant context is forwarded as-is: platform admission bypasses
        // tenant `route_policies` scopes, so platform handlers must not authorize on it.
        // A malformed or repeated `Authorization` header inserts no context, so
        // `platform_gate_middleware` refuses it as an unverified bearer.
        AuthRequirement::Platform => {
            let Ok(token) = extract_bearer_http(req.headers()) else {
                return next.run(req).await;
            };
            match state.authn_client.authenticate(token.expose_secret()).await {
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
        anonymous_matchers: HashMap<Method, AnonymousRouteMatcher>,
        require_auth_by_default: bool,
    ) -> GatewayRoutePolicy {
        GatewayRoutePolicy::new(
            Arc::new(route_matchers),
            Arc::new(anonymous_matchers),
            require_auth_by_default,
            None,
        )
    }

    #[test]
    fn resolve_consults_proxy_registry_for_dynamic_routes() {
        use toolkit_gateway::{Endpoint, GearName, RouteTemplate};

        let registry = Arc::new(ProxyRegistry::new());
        registry.register(
            GearName::from("calc"),
            "calc-1",
            Endpoint::parse("http://calc:8080").unwrap(),
            vec![
                RouteTemplate::new(Method::GET, "/calc/v1/pub", false),
                RouteTemplate::new(Method::POST, "/calc/v1/secure", true),
            ],
        );

        // `require_auth_by_default = true`, but the proxy registry overrides per route.
        let policy = GatewayRoutePolicy::new(
            Arc::new(HashMap::new()),
            Arc::new(HashMap::new()),
            true,
            Some(registry),
        );

        assert_eq!(
            policy.resolve(&Method::GET, "/calc/v1/pub"),
            AuthRequirement::None
        );
        assert_eq!(
            policy.resolve(&Method::POST, "/calc/v1/secure"),
            AuthRequirement::Required
        );
        // A path not in the proxy registry falls back to `require_auth_by_default`.
        assert_eq!(
            policy.resolve(&Method::GET, "/unknown"),
            AuthRequirement::Required
        );
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
        let routes = [
            (
                Method::GET,
                "events:poll".to_owned(),
                RouteAuth::Authenticated,
            ),
            (
                Method::GET,
                "events:stream".to_owned(),
                RouteAuth::Authenticated,
            ),
        ];

        if let Err(err) = build_route_policy(&cfg, routes, None) {
            panic!("literal colon route paths must not be interpreted as path parameters: {err}");
        }
    }

    #[test]
    fn explicit_public_route_with_path_params_returns_none() {
        let mut anonymous_matchers = HashMap::new();
        let mut matcher = AnonymousRouteMatcher::new();
        matcher.insert("/users/{id}").unwrap();

        anonymous_matchers.insert(Method::GET, matcher);

        let policy = build_test_policy(HashMap::new(), anonymous_matchers, true);

        // Path parameters should match concrete values
        let result = policy.resolve(&Method::GET, "/users/42");
        assert_eq!(result, AuthRequirement::None);
    }

    #[test]
    fn explicit_public_route_exact_match_returns_none() {
        let mut anonymous_matchers = HashMap::new();
        let mut matcher = AnonymousRouteMatcher::new();
        matcher.insert("/health").unwrap();
        anonymous_matchers.insert(Method::GET, matcher);

        let policy = build_test_policy(HashMap::new(), anonymous_matchers, true);

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
        let mut anonymous_matchers = HashMap::new();
        let mut matcher = AnonymousRouteMatcher::new();
        matcher.insert("/public").unwrap();
        anonymous_matchers.insert(Method::GET, matcher);

        let policy = build_test_policy(HashMap::new(), anonymous_matchers, true);

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
    fn explicit_anonymous_overrides_wildcard_authenticated_fallback() {
        // When a gateway registers a wildcard authenticated 404 the fallback
        // like `/{*rest}` (used to convert anonymous 404s to 401s),
        // grabs the anonymous routes too, causing 401 on them
        let mut anonymous_matchers = HashMap::new();
        let mut anonymous_matcher = AnonymousRouteMatcher::new();
        anonymous_matcher.insert("/v1/auth/config").unwrap();
        anonymous_matchers.insert(Method::GET, anonymous_matcher);

        let mut route_matchers = HashMap::new();
        let mut auth_matcher = RouteMatcher::new();
        auth_matcher.insert("/{*rest}").unwrap();
        route_matchers.insert(Method::GET, auth_matcher);

        let policy = build_test_policy(route_matchers, anonymous_matchers, true);

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

    fn platform_policy(
        routes: &[(Method, &str)],
        route_matchers: HashMap<Method, RouteMatcher>,
        anonymous_matchers: HashMap<Method, AnonymousRouteMatcher>,
    ) -> GatewayRoutePolicy {
        let mut platform: HashMap<Method, HashSet<String>> = HashMap::new();
        for (method, path) in routes {
            platform
                .entry(method.clone())
                .or_default()
                .insert((*path).to_owned());
        }
        build_test_policy(route_matchers, anonymous_matchers, false)
            .with_platform_routes(Arc::new(platform))
    }

    #[test]
    fn platform_route_wins_over_every_other_declaration() {
        let mut anonymous_matchers = HashMap::new();
        let mut anonymous = AnonymousRouteMatcher::new();
        anonymous.insert("/p").unwrap();
        anonymous_matchers.insert(Method::POST, anonymous);

        let policy = platform_policy(&[(Method::POST, "/p")], HashMap::new(), anonymous_matchers);

        assert_eq!(
            policy.resolve(&Method::POST, "/p"),
            AuthRequirement::Platform
        );
        assert_eq!(policy.resolve(&Method::GET, "/p"), AuthRequirement::None);
    }

    #[test]
    fn platform_route_wins_over_an_authenticated_matcher_and_an_anonymous_proxy_route() {
        let mut route_matchers = HashMap::new();
        let mut authenticated = RouteMatcher::new();
        authenticated.insert("/calc/v1/health").unwrap();
        route_matchers.insert(Method::GET, authenticated);
        let registry = Arc::new(ProxyRegistry::new());
        registry.register(
            toolkit_gateway::GearName::from("calculator"),
            "calc-1",
            toolkit_gateway::Endpoint::parse("http://calculator:8080").unwrap(),
            vec![toolkit_gateway::RouteTemplate::new(
                Method::GET,
                "/calc/v1/health",
                false,
            )],
        );
        assert_eq!(
            registry.requires_auth(&Method::GET, "/calc/v1/health"),
            Some(false),
            "the proxy declares the route anonymous"
        );
        let mut platform: HashMap<Method, HashSet<String>> = HashMap::new();
        platform
            .entry(Method::GET)
            .or_default()
            .insert("/calc/v1/health".to_owned());

        let policy = GatewayRoutePolicy::new(
            Arc::new(route_matchers),
            Arc::new(HashMap::new()),
            false,
            Some(registry),
        )
        .with_platform_routes(Arc::new(platform));

        assert_eq!(
            policy.resolve(&Method::GET, "/calc/v1/health"),
            AuthRequirement::Platform,
            "neither the anonymous proxy entry nor the authenticated matcher relaxes it"
        );
    }

    fn request(method: Method, path: &str) -> axum::extract::Request {
        axum::extract::Request::builder()
            .method(method)
            .uri(path)
            .body(axum::body::Body::empty())
            .unwrap()
    }

    fn is_platform(policy: &GatewayRoutePolicy, method: Method, path: &str) -> bool {
        policy.requirement_of(&request(method, path)) == AuthRequirement::Platform
    }

    #[test]
    fn platform_resolution_keeps_an_explicit_head_ahead_of_its_get() {
        let mut anonymous_matchers = HashMap::new();
        let mut head = AnonymousRouteMatcher::new();
        head.insert("/b").unwrap();
        anonymous_matchers.insert(Method::HEAD, head);
        let policy = platform_policy(
            &[(Method::GET, "/a"), (Method::GET, "/b")],
            HashMap::new(),
            anonymous_matchers,
        );

        assert!(is_platform(&policy, Method::GET, "/a"));
        assert!(
            is_platform(&policy, Method::HEAD, "/a"),
            "HEAD inherits a platform GET"
        );
        assert!(
            !is_platform(&policy, Method::HEAD, "/b"),
            "an explicit anonymous HEAD is not overridden by its platform GET"
        );
        assert!(!is_platform(&policy, Method::GET, "/c"));
        assert!(!is_platform(&policy, Method::POST, "/a"));

        let none = platform_policy(&[], HashMap::new(), HashMap::new());
        assert!(!is_platform(&none, Method::GET, "/a"));
    }

    #[test]
    fn a_resolved_requirement_is_read_back_instead_of_resolved_again() {
        let policy = platform_policy(&[(Method::GET, "/a")], HashMap::new(), HashMap::new());
        let mut req = request(Method::GET, "/a");
        assert_eq!(policy.requirement_of(&req), AuthRequirement::Platform);

        req.extensions_mut()
            .insert(ResolvedRequirement(AuthRequirement::None));
        assert_eq!(
            policy.requirement_of(&req),
            AuthRequirement::None,
            "the outer layer's resolution is the one that holds"
        );
    }

    #[test]
    fn a_platform_template_does_not_capture_another_declared_route() {
        let mut route_matchers = HashMap::new();
        let mut authenticated = RouteMatcher::new();
        authenticated.insert("/items/special").unwrap();
        route_matchers.insert(Method::GET, authenticated);

        let policy = platform_policy(
            &[(Method::GET, "/items/{id}")],
            route_matchers,
            HashMap::new(),
        );

        assert_eq!(
            policy.resolve(&Method::GET, "/items/{id}"),
            AuthRequirement::Platform
        );
        assert_eq!(
            policy.resolve(&Method::GET, "/items/special"),
            AuthRequirement::Required,
            "the tenant route's own template must not match the platform pattern"
        );
    }

    #[test]
    fn head_inherits_a_platform_get_unless_declared_itself() {
        let mut anonymous_matchers = HashMap::new();
        let mut head = AnonymousRouteMatcher::new();
        head.insert("/b").unwrap();
        anonymous_matchers.insert(Method::HEAD, head);

        let policy = platform_policy(
            &[(Method::GET, "/a"), (Method::GET, "/b")],
            HashMap::new(),
            anonymous_matchers,
        );

        assert_eq!(
            policy.resolve(&Method::HEAD, "/a"),
            AuthRequirement::Platform
        );
        assert_eq!(policy.resolve(&Method::HEAD, "/b"), AuthRequirement::None);
    }

    #[test]
    fn head_resolves_as_get_unless_declared_itself() {
        let mut route_matchers = HashMap::new();
        let mut get_matcher = RouteMatcher::new();
        get_matcher.insert("/a").unwrap();
        get_matcher.insert("/b").unwrap();
        route_matchers.insert(Method::GET, get_matcher);
        let mut anonymous_matchers = HashMap::new();
        let mut head_matcher = AnonymousRouteMatcher::new();
        head_matcher.insert("/b").unwrap();
        anonymous_matchers.insert(Method::HEAD, head_matcher);

        let policy = build_test_policy(route_matchers, anonymous_matchers, false);

        assert_eq!(
            policy.resolve(&Method::HEAD, "/a"),
            AuthRequirement::Required,
            "HEAD served by an authenticated GET route must not bypass it"
        );
        assert_eq!(
            policy.resolve(&Method::HEAD, "/b"),
            AuthRequirement::None,
            "an explicit HEAD declaration keeps precedence"
        );
        assert_eq!(policy.resolve(&Method::HEAD, "/c"), AuthRequirement::None);
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
