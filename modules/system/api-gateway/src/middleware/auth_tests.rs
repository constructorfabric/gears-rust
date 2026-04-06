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
fn test_convert_axum_path_to_matchit() {
    assert_eq!(convert_axum_path_to_matchit("/users/:id"), "/users/{id}");
    assert_eq!(
        convert_axum_path_to_matchit("/posts/:post_id/comments/:comment_id"),
        "/posts/{post_id}/comments/{comment_id}"
    );
    assert_eq!(convert_axum_path_to_matchit("/health"), "/health"); // No params
    assert_eq!(
        convert_axum_path_to_matchit("/api/v1/:resource/:id/status"),
        "/api/v1/{resource}/{id}/status"
    );
}

#[test]
fn test_matchit_router_with_params() {
    // matchit 0.8 uses {param} syntax for path parameters (NOT :param)
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
fn explicit_public_route_with_path_params_returns_none() {
    let mut public_matchers = HashMap::new();
    let mut matcher = PublicRouteMatcher::new();
    // matchit 0.8 uses {param} syntax (Axum uses :param, so conversion needed in production)
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
    // matchit 0.8 uses {param} syntax
    matcher.insert("/users/{id}").unwrap();
    route_matchers.insert(Method::GET, matcher);

    let policy = build_test_policy(route_matchers, HashMap::new(), false);

    let result = policy.resolve(&Method::GET, "/users/123");
    assert_eq!(result, AuthRequirement::Required);
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
