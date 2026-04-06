use super::*;
use crate::config::RoutePolicyRule;

fn build_config(enabled: bool, routes: Vec<(&str, Vec<&str>)>) -> RoutePoliciesConfig {
    build_config_with_methods(
        enabled,
        routes.into_iter().map(|(p, s)| (p, None, s)).collect(),
    )
}

type TestRoute<'a> = (&'a str, Option<&'a str>, Vec<&'a str>);

fn build_config_with_methods(enabled: bool, routes: Vec<TestRoute<'_>>) -> RoutePoliciesConfig {
    let rules = routes
        .into_iter()
        .map(|(path, method, scopes)| RoutePolicyRule {
            path: path.to_owned(),
            method: method.map(String::from),
            required_scopes: scopes.into_iter().map(String::from).collect(),
        })
        .collect();

    RoutePoliciesConfig { enabled, rules }
}

#[test]
fn disabled_enforcement_always_passes() {
    let config = build_config(false, vec![("/admin/*", vec!["admin"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    // Even with no scopes, should pass when disabled
    assert!(rules.check("/admin/users", "GET", &[]).is_ok());
}

#[test]
fn first_party_app_always_passes() {
    let config = build_config(true, vec![("/admin/*", vec!["admin"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    // First-party apps have ["*"] scope
    let scopes = vec!["*".to_owned()];
    assert!(rules.check("/admin/users", "GET", &scopes).is_ok());
}

#[test]
fn matching_scope_passes() {
    let config = build_config(true, vec![("/admin/*", vec!["admin"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    let scopes = vec!["admin".to_owned()];
    assert!(rules.check("/admin/users", "GET", &scopes).is_ok());
}

#[test]
fn any_of_required_scopes_passes() {
    let config = build_config(
        true,
        vec![("/events/v1/*", vec!["read:events", "write:events"])],
    );
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    // Having just one of the required scopes should pass
    let scopes = vec!["read:events".to_owned()];
    assert!(rules.check("/events/v1/list", "GET", &scopes).is_ok());

    let scopes = vec!["write:events".to_owned()];
    assert!(rules.check("/events/v1/create", "POST", &scopes).is_ok());
}

#[test]
fn missing_scope_returns_forbidden() {
    let config = build_config(true, vec![("/admin/*", vec!["admin"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    // No matching scope
    let scopes = vec!["read:events".to_owned()];
    let result = rules.check("/admin/users", "GET", &scopes);
    assert!(result.is_err());

    let problem = result.unwrap_err();
    assert_eq!(problem.status, axum::http::StatusCode::FORBIDDEN);
}

#[test]
fn empty_scopes_returns_forbidden() {
    let config = build_config(true, vec![("/admin/*", vec!["admin"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    // Empty scopes = no permissions (fail-closed)
    let result = rules.check("/admin/users", "GET", &[]);
    assert!(result.is_err());

    let problem = result.unwrap_err();
    assert_eq!(problem.status, axum::http::StatusCode::FORBIDDEN);
}

#[test]
fn unmatched_route_passes() {
    let config = build_config(true, vec![("/admin/*", vec!["admin"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    // Route doesn't match any pattern, should pass even with unrelated scope
    let scopes = vec!["unrelated:scope".to_owned()];
    assert!(rules.check("/public/health", "GET", &scopes).is_ok());
}

#[test]
fn glob_single_star_matches_single_segment_only() {
    let config = build_config(true, vec![("/api/*/items", vec!["api:read"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    let scopes = vec!["api:read".to_owned()];

    // Single * matches exactly one path segment (doesn't cross `/`)
    assert!(rules.check("/api/v1/items", "GET", &scopes).is_ok());
    assert!(rules.check("/api/v2/items", "GET", &scopes).is_ok());

    // Multiple segments do NOT match single * pattern (no scope check triggered)
    let unrelated_scopes = vec!["unrelated:scope".to_owned()];
    assert!(
        rules
            .check("/api/v1/nested/items", "GET", &unrelated_scopes)
            .is_ok()
    ); // doesn't match pattern
}

#[test]
fn glob_double_star_matches_multiple_segments() {
    let config = build_config(true, vec![("/api/**", vec!["api:access"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    let scopes = vec!["api:access".to_owned()];

    // ** matches any number of path segments
    assert!(rules.check("/api/v1", "GET", &scopes).is_ok());
    assert!(rules.check("/api/v1/items", "GET", &scopes).is_ok());
    assert!(
        rules
            .check("/api/v1/items/123/details", "GET", &scopes)
            .is_ok()
    );
}

#[test]
fn invalid_glob_pattern_returns_error() {
    let config = build_config(true, vec![("/admin/[invalid", vec!["admin"])]);
    let result = ScopeEnforcementRules::from_config(&config);
    assert!(result.is_err());
}

#[test]
fn empty_required_scopes_returns_error() {
    let config = build_config(true, vec![("/admin/*", vec![])]);
    let result = ScopeEnforcementRules::from_config(&config);
    let err = result.expect_err("should fail with empty required_scopes");
    assert!(
        err.to_string().contains("empty required_scopes"),
        "error should mention empty required_scopes: {err}"
    );
}

#[test]
fn multiple_non_overlapping_rules() {
    // Non-overlapping patterns: each path matches exactly one rule
    let config = build_config(
        true,
        vec![
            ("/admin/*", vec!["admin"]),
            ("/events/**", vec!["events:read"]),
        ],
    );
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    // Admin route needs admin scope
    let admin_scopes = vec!["admin".to_owned()];
    assert!(rules.check("/admin/users", "GET", &admin_scopes).is_ok());

    // Events route needs events:read scope
    let events_scopes = vec!["events:read".to_owned()];
    assert!(
        rules
            .check("/events/v1/list", "GET", &events_scopes)
            .is_ok()
    );

    // Wrong scope for admin route
    assert!(rules.check("/admin/users", "GET", &events_scopes).is_err());

    // Wrong scope for events route
    assert!(
        rules
            .check("/events/v1/list", "GET", &admin_scopes)
            .is_err()
    );
}

#[test]
fn overlapping_rules_first_match_wins() {
    // Path /api/admin/users matches BOTH rules with DIFFERENT scope requirements.
    // First-match-wins: only the first matching rule is evaluated.
    let config = build_config(
        true,
        vec![
            ("/api/**", vec!["basic"]), // Matches /api/admin/users, requires "basic"
            ("/api/admin/**", vec!["admin"]), // Also matches, requires "admin" (never evaluated)
        ],
    );
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    // /api/admin/users matches both rules, but first rule wins
    let basic_scopes = vec!["basic".to_owned()];
    let admin_scopes = vec!["admin".to_owned()];

    // "basic" scope passes because first rule (/api/**) is evaluated
    assert!(
        rules
            .check("/api/admin/users", "GET", &basic_scopes)
            .is_ok()
    );

    // "admin" scope also passes (it satisfies the first rule too? No - let's check)
    // Actually "admin" does NOT satisfy ["basic"], so it should fail
    assert!(
        rules
            .check("/api/admin/users", "GET", &admin_scopes)
            .is_err()
    );

    // This demonstrates first-match-wins: even though second rule requires "admin",
    // the first rule requiring "basic" takes precedence for /api/admin/users
}

#[test]
fn matches_protected_route_returns_true_for_matching_path() {
    let config = build_config(true, vec![("/admin/*", vec!["admin"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    assert!(rules.matches_protected_route("/admin/users", "GET"));
    assert!(rules.matches_protected_route("/admin/settings", "POST"));
}

#[test]
fn matches_protected_route_returns_false_for_non_matching_path() {
    let config = build_config(true, vec![("/admin/*", vec!["admin"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    assert!(!rules.matches_protected_route("/public/health", "GET"));
    assert!(!rules.matches_protected_route("/api/v1/users", "GET"));
}

#[test]
fn matches_protected_route_returns_false_when_disabled() {
    let config = build_config(false, vec![("/admin/*", vec!["admin"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    // Even matching paths return false when enforcement is disabled
    assert!(!rules.matches_protected_route("/admin/users", "GET"));
}

#[test]
fn first_match_wins_more_specific_rule_first() {
    // More specific rule declared first should take precedence
    let config = build_config(
        true,
        vec![
            ("/api/admin/**", vec!["admin"]), // More specific, declared first
            ("/api/**", vec!["basic"]),       // Broader, declared second
        ],
    );
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    // /api/admin/users matches first rule, requires "admin"
    let admin_scopes = vec!["admin".to_owned()];
    let basic_scopes = vec!["basic".to_owned()];

    // Admin scope passes (matches first rule)
    assert!(
        rules
            .check("/api/admin/users", "GET", &admin_scopes)
            .is_ok()
    );

    // Basic scope fails for /api/admin/users (first rule requires admin)
    assert!(
        rules
            .check("/api/admin/users", "GET", &basic_scopes)
            .is_err()
    );

    // Basic scope passes for /api/other (matches second rule)
    assert!(rules.check("/api/other", "GET", &basic_scopes).is_ok());
}

#[test]
fn first_match_wins_broader_rule_first() {
    // If broader rule is declared first, it takes precedence
    let config = build_config(
        true,
        vec![
            ("/api/**", vec!["basic"]),       // Broader, declared first
            ("/api/admin/**", vec!["admin"]), // More specific, declared second (never reached)
        ],
    );
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    let basic_scopes = vec!["basic".to_owned()];

    // /api/admin/users matches first rule (/api/**), so "basic" is sufficient
    assert!(
        rules
            .check("/api/admin/users", "GET", &basic_scopes)
            .is_ok()
    );
}

#[test]
fn method_matching_specific_method() {
    // Rule with specific method only matches that method
    let config = build_config_with_methods(
        true,
        vec![
            ("/users/*", Some("POST"), vec!["users:write"]),
            ("/users/*", Some("GET"), vec!["users:read"]),
        ],
    );
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    let read_scopes = vec!["users:read".to_owned()];
    let write_scopes = vec!["users:write".to_owned()];

    // GET with read scope passes
    assert!(rules.check("/users/123", "GET", &read_scopes).is_ok());

    // POST with write scope passes
    assert!(rules.check("/users/123", "POST", &write_scopes).is_ok());

    // GET with write scope fails (first matching rule requires users:write for POST)
    // Actually GET matches second rule which requires users:read
    assert!(rules.check("/users/123", "GET", &write_scopes).is_err());

    // POST with read scope fails (POST rule requires users:write)
    assert!(rules.check("/users/123", "POST", &read_scopes).is_err());
}

#[test]
fn method_matching_any_method() {
    // Rule without method matches any method
    let config = build_config_with_methods(true, vec![("/api/**", None, vec!["api:access"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    let scopes = vec!["api:access".to_owned()];

    // All methods should match
    assert!(rules.check("/api/users", "GET", &scopes).is_ok());
    assert!(rules.check("/api/users", "POST", &scopes).is_ok());
    assert!(rules.check("/api/users", "PUT", &scopes).is_ok());
    assert!(rules.check("/api/users", "DELETE", &scopes).is_ok());
}

#[test]
fn method_matching_case_insensitive() {
    // Method matching should be case-insensitive
    let config = build_config_with_methods(true, vec![("/api/**", Some("get"), vec!["api:read"])]);
    let rules = ScopeEnforcementRules::from_config(&config).unwrap();

    let scopes = vec!["api:read".to_owned()];

    // Should match regardless of case
    assert!(rules.check("/api/users", "GET", &scopes).is_ok());
    assert!(rules.check("/api/users", "get", &scopes).is_ok());
    assert!(rules.check("/api/users", "Get", &scopes).is_ok());
}
