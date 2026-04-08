// @cpt-dod:cpt-cf-resource-group-dod-type-mgmt-rest-handlers:p1
use super::{dto, handlers};
use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};
use resource_group_sdk::odata::TypeFilterField;

const API_TAG: &str = "Resource Group Types";

pub(super) fn register_type_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /types-registry/v1/types - List types with cursor-based pagination
    router = OperationBuilder::get("/types-registry/v1/types")
        .operation_id("resource_group.list_types")
        .summary("List GTS types")
        .description("Retrieve a list of GTS resource group type definitions with OData filtering")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .query_param_typed(
            "limit",
            false,
            "Maximum number of types to return",
            "integer",
        )
        .query_param("cursor", false, "Cursor for pagination")
        .handler(handlers::list_types)
        .json_response_with_schema::<modkit_odata::Page<dto::TypeDto>>(
            openapi,
            http::StatusCode::OK,
            "List of GTS types",
        )
        .with_odata_filter::<TypeFilterField>()
        .error_400(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /types-registry/v1/types - Create a new type
    router = OperationBuilder::post("/types-registry/v1/types")
        .operation_id("resource_group.create_type")
        .summary("Create a new GTS type")
        .description("Create a new GTS resource group type definition")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .json_request::<dto::CreateTypeDto>(openapi, "Type creation data")
        .handler(handlers::create_type)
        .json_response_with_schema::<dto::TypeDto>(
            openapi,
            http::StatusCode::CREATED,
            "Created type",
        )
        .error_400(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /types-registry/v1/types/{code} - Get a specific type
    router = OperationBuilder::get("/types-registry/v1/types/{code}")
        .operation_id("resource_group.get_type")
        .summary("Get GTS type by code")
        .description("Retrieve a specific GTS type definition by its GTS type path")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("code", "GTS type path")
        .handler(handlers::get_type)
        .json_response_with_schema::<dto::TypeDto>(openapi, http::StatusCode::OK, "Type found")
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /types-registry/v1/types/{code} - Update a type
    router = OperationBuilder::put("/types-registry/v1/types/{code}")
        .operation_id("resource_group.update_type")
        .summary("Update GTS type")
        .description("Update a GTS resource group type definition (full replacement)")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("code", "GTS type path")
        .json_request::<dto::UpdateTypeDto>(openapi, "Type update data")
        .handler(handlers::update_type)
        .json_response_with_schema::<dto::TypeDto>(openapi, http::StatusCode::OK, "Updated type")
        .error_400(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /types-registry/v1/types/{code} - Delete a type
    router = OperationBuilder::delete("/types-registry/v1/types/{code}")
        .operation_id("resource_group.delete_type")
        .summary("Delete GTS type")
        .description("Delete a GTS resource group type definition")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("code", "GTS type path")
        .handler(handlers::delete_type)
        .json_response(http::StatusCode::NO_CONTENT, "Type deleted successfully")
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
