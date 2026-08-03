//! Route registration for the hello gear.

use axum::Router;
use http::StatusCode;

use toolkit::api::{OpenApiRegistry, OperationBuilder};

use super::dto::PingResponse;
use super::handlers;

/// Register all REST routes for the hello gear.
pub fn register_routes(router: Router, openapi: &dyn OpenApiRegistry) -> anyhow::Result<Router> {
    // GET /hello/v1/ping - public, anonymous liveness/echo route.
    //
    // `.exposed()` marks it VISIBLE at the edge (so the api-gateway proxy
    // registers it); `.anonymous()` marks it as requiring NO auth. Both axes are
    // independent (see cpt-cf-adr-gateway-abstraction / ADR-0003).
    let router = OperationBuilder::get("/hello/v1/ping")
        .operation_id("hello.ping")
        .summary("Ping")
        .description("Returns a fixed greeting. Public and anonymous.")
        .tag("Hello")
        .exposed()
        .anonymous()
        .handler(handlers::handle_ping)
        .json_response_with_schema::<PingResponse>(openapi, StatusCode::OK, "Pong response")
        .error_500(openapi)
        .register(router, openapi);

    Ok(router)
}
