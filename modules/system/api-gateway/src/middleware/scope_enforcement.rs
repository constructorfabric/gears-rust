//! Gateway Scope Enforcement Middleware
//!
//! Performs coarse-grained early rejection of requests based on token scopes
//! without calling the PDP. This is an optimization for performance-critical routes.
//!
//! See `docs/arch/authorization/DESIGN.md` section "Gateway Scope Enforcement" for details.

use std::sync::Arc;

use axum::response::IntoResponse;
use glob::{MatchOptions, Pattern};

use crate::config::RoutePoliciesConfig;
use crate::middleware::common;
use modkit::api::Problem;
use modkit_security::SecurityContext;

/// Compiled scope enforcement rules for efficient runtime matching.
#[derive(Clone, Debug)]
pub struct ScopeEnforcementRules {
    /// Compiled glob patterns with their required scopes.
    rules: Arc<[CompiledRule]>,
    /// Whether scope enforcement is enabled.
    enabled: bool,
}

/// A single compiled rule: glob pattern + optional method + required scopes.
#[derive(Clone, Debug)]
struct CompiledRule {
    pattern: Pattern,
    /// HTTP method to match (uppercase). None means match any method.
    method: Option<String>,
    required_scopes: Vec<String>,
}

impl ScopeEnforcementRules {
    /// Build scope enforcement rules from configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if any glob pattern is invalid or if any rule has empty `required_scopes`.
    pub fn from_config(config: &RoutePoliciesConfig) -> Result<Self, anyhow::Error> {
        if !config.enabled {
            return Ok(Self {
                rules: Arc::from([]),
                enabled: false,
            });
        }

        let mut rules = Vec::with_capacity(config.rules.len());

        for rule in &config.rules {
            // Validate: empty required_scopes is likely a config mistake
            if rule.required_scopes.is_empty() {
                return Err(anyhow::anyhow!(
                    "Route policy rule for path '{}' has empty required_scopes. \
                     This would match all tokens and is likely a config mistake.",
                    rule.path
                ));
            }

            let pattern = Pattern::new(&rule.path).map_err(|e| {
                anyhow::anyhow!(
                    "Invalid glob pattern '{}' in route_policies: {e}",
                    rule.path
                )
            })?;

            rules.push(CompiledRule {
                pattern,
                method: rule.method.as_ref().map(|m| m.to_uppercase()),
                required_scopes: rule.required_scopes.clone(),
            });
        }

        tracing::info!(
            rules_count = rules.len(),
            "Route policy enforcement enabled with {} rules",
            rules.len()
        );

        Ok(Self {
            rules: Arc::from(rules),
            enabled: true,
        })
    }

    /// Check if the given path and method match any protected route.
    ///
    /// Returns `true` if the path/method matches at least one scope enforcement rule.
    fn matches_protected_route(&self, path: &str, method: &str) -> bool {
        if !self.enabled {
            return false;
        }

        let match_opts = MatchOptions {
            require_literal_separator: true,
            ..MatchOptions::default()
        };

        self.rules.iter().any(|rule| {
            let path_matches = rule.pattern.matches_with(path, match_opts);
            let method_matches = rule
                .method
                .as_ref()
                .is_none_or(|m| m.eq_ignore_ascii_case(method));
            path_matches && method_matches
        })
    }

    /// Check if the given path, method, and token scopes satisfy the scope requirements.
    ///
    /// Returns `Ok(())` if access is allowed, or `Err(problem)` if denied.
    #[allow(clippy::result_large_err)]
    fn check(&self, path: &str, method: &str, token_scopes: &[String]) -> Result<(), Problem> {
        if !self.enabled {
            return Ok(());
        }

        // Only wildcard scope `["*"]` is unrestricted (first-party apps).
        // Empty scopes = no permissions (fail-closed).
        if token_scopes.iter().any(|s| s == "*") {
            return Ok(());
        }

        // Match options: require `/` to be matched literally so `*` doesn't cross path segments
        let match_opts = MatchOptions {
            require_literal_separator: true,
            ..MatchOptions::default()
        };

        // First match wins: find the first matching rule and check scopes against it only.
        // This allows more specific rules to override broader ones when declared first.
        for rule in self.rules.iter() {
            let path_matches = rule.pattern.matches_with(path, match_opts);
            let method_matches = rule
                .method
                .as_ref()
                .is_none_or(|m| m.eq_ignore_ascii_case(method));

            if path_matches && method_matches {
                // Check if token has ANY of the required scopes
                let has_required_scope = rule
                    .required_scopes
                    .iter()
                    .any(|required| token_scopes.contains(required));

                if has_required_scope {
                    return Ok(());
                }

                tracing::warn!(
                    path = %path,
                    method = %method,
                    pattern = %rule.pattern,
                    rule_method = ?rule.method,
                    required_scopes = ?rule.required_scopes,
                    token_scopes = ?token_scopes,
                    "Route policy enforcement denied: insufficient scopes"
                );

                return Err(Problem::new(
                    axum::http::StatusCode::FORBIDDEN,
                    "Forbidden",
                    "Insufficient token scopes for this resource",
                ));
            }
        }

        // No rule matched — allow (unprotected route)
        Ok(())
    }
}

/// Scope enforcement middleware state.
#[derive(Clone)]
pub struct ScopeEnforcementState {
    pub rules: ScopeEnforcementRules,
}

/// Scope enforcement middleware.
///
/// Checks if the request's token scopes satisfy the configured requirements
/// for the matched route pattern. Returns 403 Forbidden if scopes are insufficient.
///
/// This middleware MUST run AFTER the auth middleware (which populates `SecurityContext`).
pub async fn scope_enforcement_middleware(
    axum::extract::State(state): axum::extract::State<ScopeEnforcementState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Skip if enforcement is disabled
    if !state.rules.enabled {
        return next.run(req).await;
    }

    // Use the concrete URI path for glob pattern matching (not MatchedPath which
    // returns the route template like "/{*path}" for catch-all routes).
    let path = req.uri().path();
    let path = common::resolve_path(&req, path);
    let method = req.method().as_str();

    // Get SecurityContext from request extensions (populated by auth middleware)
    let Some(security_context) = req.extensions().get::<SecurityContext>() else {
        // No SecurityContext means auth middleware didn't run or request is unauthenticated.
        // If the path matches a protected route, reject with 401 Unauthorized.
        // Otherwise, let it pass through for public/unprotected routes.
        if state.rules.matches_protected_route(&path, method) {
            tracing::warn!(
                path = %path,
                method = %method,
                "Route policy enforcement denied: no SecurityContext for protected route"
            );
            return Problem::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "Unauthorized",
                "Authentication required for this resource",
            )
            .into_response();
        }
        return next.run(req).await;
    };

    // Check scopes
    if let Err(problem) = state
        .rules
        .check(&path, method, security_context.token_scopes())
    {
        return problem.into_response();
    }

    next.run(req).await
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "scope_enforcement_tests.rs"]
mod scope_enforcement_tests;
