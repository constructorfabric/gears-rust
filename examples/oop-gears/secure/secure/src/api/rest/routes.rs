//! Route registration for the secure gear.

use axum::Router;
use http::StatusCode;

use toolkit::api::{OpenApiRegistry, OperationBuilder};

use super::dto::WhoAmIResponse;
use super::handlers;

/// Register all REST routes for the secure gear.
pub fn register_routes(router: Router, openapi: &dyn OpenApiRegistry) -> anyhow::Result<Router> {
    // GET /secure/v1/whoami - exposed AND authenticated.
    //
    // `.exposed()` registers the route at the edge (so the api-gateway proxies
    // it AND enforces the tenant-plane bearer there); `.authenticated()` marks
    // it as requiring a bearer, which — via the OoP pod's
    // `security_context_middleware` — is re-validated in-process on every call
    // (zero-trust; see cpt-cf-adr-two-plane-auth).
    let router = OperationBuilder::get("/secure/v1/whoami")
        .operation_id("secure.whoami")
        .summary("Who am I")
        .description("Returns the authenticated subject/tenant as re-validated inside the OoP pod.")
        .tag("Secure")
        .exposed()
        .authenticated()
        .no_license_required()
        .handler(handlers::handle_whoami)
        .json_response_with_schema::<WhoAmIResponse>(openapi, StatusCode::OK, "Identity")
        .error_401(openapi)
        .error_500(openapi)
        .register(router, openapi);

    Ok(router)
}
