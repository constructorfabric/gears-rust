//! The gateway's middleware layer.
//!
//! The middleware themselves live in the shared `toolkit_http_middleware` crate
//! and operate on neutral lookup tables/rules. This module owns the projection
//! that turns the gateway's own inputs — typed [`OperationSpec`]s and the parsed
//! `crate::config` — into those neutral inputs (the per-route validation maps,
//! rate limiters, scope-enforcement rules, and the route auth policy). It is
//! deliberately the only place that couples the gateway's specs/config/error
//! identity to the shared middleware.
use std::collections::HashSet;

use anyhow::Result;
use http::Method;
use toolkit::api::OperationSpec;
use toolkit_http_middleware::MatchitRouteAuthPolicy;
use toolkit_http_middleware::license_validation::LicenseRequirementMap;
use toolkit_http_middleware::mime_validation::MimeValidationMap;
use toolkit_http_middleware::rate_limit::{RateLimitConfig, RateLimiterMap};
use toolkit_http_middleware::scope_enforcement::ScopeEnforcementRules;

use crate::config::{ApiGatewayConfig, RoutePoliciesConfig};
use crate::errors;

// Re-exported so integration tests (separate crates) can drive the middleware
// through the gateway facade without depending on `toolkit-http-middleware`.
pub use toolkit_http_middleware::mime_validation::mime_validation_middleware;

/// Build the MIME validation map from operation specs.
#[must_use]
pub fn build_mime_validation_map(specs: &[OperationSpec]) -> MimeValidationMap {
    MimeValidationMap::from_pairs(
        errors::GATEWAY_SCOPE,
        specs.iter().filter_map(|spec| {
            spec.allowed_request_content_types
                .as_ref()
                .map(|allowed| ((spec.method.clone(), spec.path.clone()), allowed.clone()))
        }),
    )
}

/// Build the per-route required-feature map from operation specs.
#[must_use]
pub fn build_license_requirement_map(specs: &[OperationSpec]) -> LicenseRequirementMap {
    LicenseRequirementMap::from_pairs(
        errors::ROUTE_SCOPE,
        specs.iter().filter_map(|spec| {
            spec.license_requirement.as_ref().map(|req| {
                (
                    (spec.method.clone(), spec.path.clone()),
                    req.license_names.clone(),
                )
            })
        }),
    )
}

/// Build the per-route rate limiter map from operation specs and gateway
/// configuration (falling back to configured defaults where a route declares no
/// explicit rate limit).
///
/// # Errors
/// Returns an error if any effective `rps` or `burst` is 0.
// TODO: Add support for per-route rate limiting keys.
pub fn build_rate_limiter_map(
    specs: &[OperationSpec],
    cfg: &ApiGatewayConfig,
) -> Result<RateLimiterMap> {
    RateLimiterMap::from_pairs(
        errors::GATEWAY_SCOPE,
        specs.iter().map(|spec| {
            let limit = spec.rate_limit.as_ref().map_or_else(
                || cfg.defaults.rate_limit.clone(),
                |r| RateLimitConfig {
                    rps: r.rps,
                    burst: r.burst,
                    in_flight: r.in_flight,
                },
            );
            ((spec.method.clone(), spec.path.clone()), limit)
        }),
    )
}

/// Build compiled scope enforcement rules from the gateway's route-policy
/// configuration.
///
/// # Errors
/// Returns an error if any glob pattern is invalid or if any rule has empty
/// `required_scopes`.
pub fn build_scope_enforcement_rules(
    config: &RoutePoliciesConfig,
) -> Result<ScopeEnforcementRules> {
    ScopeEnforcementRules::from_config(errors::ROUTE_SCOPE, config)
}

/// Build the gateway's route auth policy from operation specs.
///
/// Collects authenticated/public route sets from the specs (plus the built-in
/// public health/docs routes) and projects them into the shared
/// [`MatchitRouteAuthPolicy`] consumed by
/// `toolkit_http_middleware::security_context_middleware`.
///
/// # Errors
/// Returns an error if a route pattern cannot be inserted into the matcher.
pub fn build_route_policy_from_specs(
    specs: &[OperationSpec],
    config: &ApiGatewayConfig,
) -> Result<MatchitRouteAuthPolicy> {
    let mut authenticated_routes = HashSet::new();
    let mut public_routes = HashSet::new();

    // Always mark built-in health check routes as public
    public_routes.insert((Method::GET, "/health".to_owned()));
    public_routes.insert((Method::GET, "/healthz".to_owned()));

    public_routes.insert((Method::GET, "/docs".to_owned()));
    public_routes.insert((Method::GET, "/openapi.json".to_owned()));

    for spec in specs {
        let route_key = (spec.method.clone(), spec.path.clone());

        if spec.authenticated {
            authenticated_routes.insert(route_key.clone());
        }

        if spec.is_public {
            public_routes.insert(route_key);
        }
    }

    let requirements_count = authenticated_routes.len();
    let public_routes_count = public_routes.len();

    let route_policy = MatchitRouteAuthPolicy::from_route_sets(
        authenticated_routes,
        public_routes,
        config.require_auth_by_default,
    )?;

    tracing::info!(
        auth_disabled = config.auth_disabled,
        require_auth_by_default = config.require_auth_by_default,
        requirements_count = requirements_count,
        public_routes_count = public_routes_count,
        "Route policy built from operation specs"
    );

    Ok(route_policy)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use http::Method;
    use toolkit::api::operation_builder::VendorExtensions;

    #[test]
    fn test_build_mime_validation_map() {
        use toolkit::api::operation_builder::{RequestBodySchema, RequestBodySpec};

        let specs = vec![OperationSpec {
            method: Method::POST,
            path: "/files/v1/upload".to_owned(),
            operation_id: None,
            summary: None,
            description: None,
            tags: vec![],
            params: vec![],
            request_body: Some(RequestBodySpec {
                content_type: "multipart/form-data",
                description: None,
                schema: RequestBodySchema::MultipartFile {
                    field_name: "file".to_owned(),
                },
                required: true,
            }),
            responses: vec![],
            handler_id: "test".to_owned(),
            authenticated: false,
            is_public: false,
            license_requirement: None,
            rate_limit: None,
            allowed_request_content_types: Some(vec!["multipart/form-data", "application/pdf"]),
            vendor_extensions: VendorExtensions::default(),
        }];

        let map = build_mime_validation_map(&specs);

        let allowed = map
            .get(&Method::POST, "/files/v1/upload")
            .expect("route should be present");
        assert_eq!(allowed.len(), 2);
        assert!(allowed.contains(&"multipart/form-data"));
        assert!(allowed.contains(&"application/pdf"));
    }
}
