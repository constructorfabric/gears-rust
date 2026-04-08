use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::api::operation_builder::OperationBuilder;

use super::super::dto;
use super::super::handlers;
use super::License;

const API_TAG: &str = "OAGW Upstreams";

pub(super) fn register(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // POST /oagw/v1/upstreams — Create upstream
    router = OperationBuilder::post("/oagw/v1/upstreams")
        .operation_id("oagw.create_upstream")
        .summary("Create upstream")
        .description("Create a new upstream service configuration")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateUpstreamRequest>(openapi, "Upstream configuration")
        .handler(handlers::upstream::create_upstream)
        .json_response_with_schema::<dto::UpstreamResponse>(
            openapi,
            http::StatusCode::CREATED,
            "Created upstream",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // GET /oagw/v1/upstreams — List upstreams
    router = OperationBuilder::get("/oagw/v1/upstreams")
        .operation_id("oagw.list_upstreams")
        .summary("List upstreams")
        .description("Retrieve a paginated list of upstream services")
        .tag(API_TAG)
        .query_param_typed(
            "limit",
            false,
            "Maximum number of results (default 50, max 100)",
            "integer",
        )
        .query_param_typed("offset", false, "Number of results to skip", "integer")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::upstream::list_upstreams)
        .json_response_with_schema::<Vec<dto::UpstreamResponse>>(
            openapi,
            http::StatusCode::OK,
            "List of upstreams",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // GET /oagw/v1/upstreams/{id} — Get upstream
    router = OperationBuilder::get("/oagw/v1/upstreams/{id}")
        .operation_id("oagw.get_upstream")
        .summary("Get upstream by ID")
        .description("Retrieve a specific upstream by its GTS identifier")
        .tag(API_TAG)
        .path_param("id", "Upstream GTS identifier")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::upstream::get_upstream)
        .json_response_with_schema::<dto::UpstreamResponse>(
            openapi,
            http::StatusCode::OK,
            "Upstream found",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // PATCH /oagw/v1/upstreams/{id} — Update upstream
    router = OperationBuilder::patch("/oagw/v1/upstreams/{id}")
        .operation_id("oagw.update_upstream")
        .summary("Update upstream")
        .description("Partially update an existing upstream service configuration")
        .tag(API_TAG)
        .path_param("id", "Upstream GTS identifier")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::UpdateUpstreamRequest>(openapi, "Upstream update data")
        .handler(handlers::upstream::update_upstream)
        .json_response_with_schema::<dto::UpstreamResponse>(
            openapi,
            http::StatusCode::OK,
            "Updated upstream",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // DELETE /oagw/v1/upstreams/{id} — Delete upstream
    router = OperationBuilder::delete("/oagw/v1/upstreams/{id}")
        .operation_id("oagw.delete_upstream")
        .summary("Delete upstream")
        .description("Delete an upstream and cascade-delete its routes")
        .tag(API_TAG)
        .path_param("id", "Upstream GTS identifier")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::upstream::delete_upstream)
        .json_response(http::StatusCode::NO_CONTENT, "Upstream deleted")
        .standard_errors(openapi)
        .register(router, openapi);

    router
}
