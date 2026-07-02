//! Canonical resource scopes for api-gateway request-pipeline errors.
use toolkit_canonical_errors::resource_error;

/// Errors attributable to a registered API gateway route
/// (scope / license / RBAC).
#[resource_error("gts.cf.core.api_gateway.route.v1~")]
pub struct ApiGatewayRouteError;

/// Umbrella scope for request-pipeline errors that don't target a
/// specific route resource (MIME validation, rate limit, request
/// timeout). Required because `invalid_argument`, `resource_exhausted`,
/// and `deadline_exceeded` are only available on `#[resource_error]`
/// scopes — there are no top-level `CanonicalError::*` constructors for
/// those categories.
#[resource_error("gts.cf.core.api_gateway.gateway.v1~")]
pub struct ApiGatewayGatewayError;

/// GTS resource scope rendered (as `resource_type`) for
/// [`ApiGatewayRouteError`]. Passed to the shared, scope-agnostic middleware
/// (`toolkit_http_middleware`) so route-scoped rejections keep the gateway's
/// wire identity. Kept in sync with the `#[resource_error]` attribute above by
/// `scope_consts_match_resource_error_types`.
pub const ROUTE_SCOPE: &str = "gts.cf.core.api_gateway.route.v1~";

/// GTS resource scope rendered (as `resource_type`) for
/// [`ApiGatewayGatewayError`]. See [`ROUTE_SCOPE`].
pub const GATEWAY_SCOPE: &str = "gts.cf.core.api_gateway.gateway.v1~";

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use toolkit_canonical_errors::Problem;

    #[test]
    fn scope_consts_match_resource_error_types() {
        // The consts are duplicated from the `#[resource_error]` literals (a
        // macro attribute can't reference a const); assert they stay in lockstep
        // with what actually renders on the wire.
        let route: Problem = ApiGatewayRouteError::permission_denied()
            .with_reason("TEST")
            .create()
            .into();
        assert_eq!(route.context["resource_type"], ROUTE_SCOPE);

        let gateway: Problem = ApiGatewayGatewayError::resource_exhausted("test")
            .with_quota_violation("test", "test")
            .create()
            .into();
        assert_eq!(gateway.context["resource_type"], GATEWAY_SCOPE);
    }
}
