use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::api::operation_builder::OperationBuilder;

use super::super::dto;
use super::super::handlers;
use super::License;

const API_TAG: &str = "OAGW Routes";

pub(super) fn register(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // POST /oagw/v1/routes — Create route
    router = OperationBuilder::post("/oagw/v1/routes")
        .operation_id("oagw.create_route")
        .summary("Create route")
        .description("Create a new route mapping for an upstream service")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateRouteRequest>(openapi, "Route configuration")
        .handler(handlers::route::create_route)
        .json_response_with_schema::<dto::RouteResponse>(
            openapi,
            http::StatusCode::CREATED,
            "Created route",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // GET /oagw/v1/routes/{id} — Get route
    router = OperationBuilder::get("/oagw/v1/routes/{id}")
        .operation_id("oagw.get_route")
        .summary("Get route by ID")
        .description("Retrieve a specific route by its GTS identifier")
        .tag(API_TAG)
        .path_param("id", "Route GTS identifier")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::route::get_route)
        .json_response_with_schema::<dto::RouteResponse>(
            openapi,
            http::StatusCode::OK,
            "Route found",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // PATCH /oagw/v1/routes/{id} — Update route
    router = OperationBuilder::patch("/oagw/v1/routes/{id}")
        .operation_id("oagw.update_route")
        .summary("Update route")
        .description("Partially update an existing route configuration")
        .tag(API_TAG)
        .path_param("id", "Route GTS identifier")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::UpdateRouteRequest>(openapi, "Route update data")
        .handler(handlers::route::update_route)
        .json_response_with_schema::<dto::RouteResponse>(
            openapi,
            http::StatusCode::OK,
            "Updated route",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // DELETE /oagw/v1/routes/{id} — Delete route
    router = OperationBuilder::delete("/oagw/v1/routes/{id}")
        .operation_id("oagw.delete_route")
        .summary("Delete route")
        .description("Delete a route by its GTS identifier")
        .tag(API_TAG)
        .path_param("id", "Route GTS identifier")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::route::delete_route)
        .json_response(http::StatusCode::NO_CONTENT, "Route deleted")
        .standard_errors(openapi)
        .register(router, openapi);

    // GET /oagw/v1/upstreams/{upstream_id}/routes — List routes by upstream
    router = OperationBuilder::get("/oagw/v1/upstreams/{upstream_id}/routes")
        .operation_id("oagw.list_routes")
        .summary("List routes by upstream")
        .description("Retrieve routes belonging to a specific upstream")
        .tag(API_TAG)
        .path_param("upstream_id", "Upstream GTS identifier")
        .query_param_typed(
            "limit",
            false,
            "Maximum number of results (default 50, max 100)",
            "integer",
        )
        .query_param_typed("offset", false, "Number of results to skip", "integer")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::route::list_routes)
        .json_response_with_schema::<Vec<dto::RouteResponse>>(
            openapi,
            http::StatusCode::OK,
            "List of routes",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router
}
