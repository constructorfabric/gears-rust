//! Canonical resource scopes for api-gateway middleware.
use toolkit_canonical_errors::resource_error;

/// Errors attributable to a registered API gateway route
/// (scope / license / RBAC).
#[resource_error(gts_id!("cf.core.api_gateway.route.v1~"))]
pub struct ApiGatewayRouteError;

/// Umbrella scope for request-pipeline errors that don't target a
/// specific route resource (MIME validation, rate limit, request
/// timeout). Required because `invalid_argument`, `resource_exhausted`,
/// and `deadline_exceeded` are only available on `#[resource_error]`
/// scopes — there are no top-level `CanonicalError::*` constructors for
/// those categories.
#[resource_error(gts_id!("cf.core.api_gateway.gateway.v1~"))]
pub struct ApiGatewayGatewayError;
