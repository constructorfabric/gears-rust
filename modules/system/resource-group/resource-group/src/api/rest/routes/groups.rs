// @cpt-dod:cpt-cf-resource-group-dod-entity-hier-rest-handlers:p1
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-rest-odata:p1
use super::{dto, handlers};
use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};
use resource_group_sdk::odata::{GroupFilterField, HierarchyFilterField};

const API_TAG: &str = "Resource Groups";

pub(super) fn register_group_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /resource-group/v1/groups - List groups with cursor-based pagination
    router = OperationBuilder::get("/resource-group/v1/groups")
        .operation_id("resource_group.list_groups")
        .summary("List resource groups")
        .description("Retrieve a paginated list of resource groups with OData filtering")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .query_param_typed(
            "limit",
            false,
            "Maximum number of groups to return",
            "integer",
        )
        .query_param("cursor", false, "Cursor for pagination")
        .handler(handlers::list_groups)
        .json_response_with_schema::<modkit_odata::Page<dto::GroupDto>>(
            openapi,
            http::StatusCode::OK,
            "Paginated list of resource groups",
        )
        .with_odata_filter::<GroupFilterField>()
        .error_400(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /resource-group/v1/groups - Create a new group
    router = OperationBuilder::post("/resource-group/v1/groups")
        .operation_id("resource_group.create_group")
        .summary("Create a new resource group")
        .description(
            "Create a new resource group with the provided type, name, and optional parent",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .json_request::<dto::CreateGroupDto>(openapi, "Group creation data")
        .handler(handlers::create_group)
        .json_response_with_schema::<dto::GroupDto>(
            openapi,
            http::StatusCode::CREATED,
            "Created resource group",
        )
        .error_400(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /resource-group/v1/groups/{group_id} - Get a specific group
    router = OperationBuilder::get("/resource-group/v1/groups/{group_id}")
        .operation_id("resource_group.get_group")
        .summary("Get resource group by ID")
        .description("Retrieve a specific resource group by its UUID")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("group_id", "Group UUID")
        .handler(handlers::get_group)
        .json_response_with_schema::<dto::GroupDto>(
            openapi,
            http::StatusCode::OK,
            "Resource group found",
        )
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PUT /resource-group/v1/groups/{group_id} - Update a group
    router = OperationBuilder::put("/resource-group/v1/groups/{group_id}")
        .operation_id("resource_group.update_group")
        .summary("Update resource group")
        .description("Update a resource group (full replacement via PUT, including parent move)")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("group_id", "Group UUID")
        .json_request::<dto::UpdateGroupDto>(openapi, "Group update data")
        .handler(handlers::update_group)
        .json_response_with_schema::<dto::GroupDto>(
            openapi,
            http::StatusCode::OK,
            "Updated resource group",
        )
        .error_400(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // PATCH /resource-group/v1/groups/{group_id} - Patch a group
    router = OperationBuilder::patch("/resource-group/v1/groups/{group_id}")
        .operation_id("resource_group.patch_group")
        .summary("Patch resource group")
        .description("Partially update a resource group (PATCH). Only provided fields are changed.")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("group_id", "Group UUID")
        .json_request::<dto::PatchGroupDto>(openapi, "Group patch data")
        .handler(handlers::patch_group)
        .json_response_with_schema::<dto::GroupDto>(
            openapi,
            http::StatusCode::OK,
            "Patched resource group",
        )
        .error_400(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // DELETE /resource-group/v1/groups/{group_id} - Delete a group
    router = OperationBuilder::delete("/resource-group/v1/groups/{group_id}")
        .operation_id("resource_group.delete_group")
        .summary("Delete resource group")
        .description(
            "Delete a resource group. Use ?force=true to cascade delete subtree and memberships.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("group_id", "Group UUID")
        .query_param(
            "force",
            false,
            "Force cascade delete of subtree and memberships",
        )
        .handler(handlers::delete_group)
        .json_response(http::StatusCode::NO_CONTENT, "Group deleted successfully")
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // GET /resource-group/v1/groups/{group_id}/hierarchy - List group hierarchy
    router = OperationBuilder::get("/resource-group/v1/groups/{group_id}/hierarchy")
        .operation_id("resource_group.list_group_hierarchy")
        .summary("List group hierarchy")
        .description(
            "Traverse hierarchy from a reference group with relative depth and OData filtering",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("group_id", "Reference group UUID")
        .query_param_typed(
            "limit",
            false,
            "Maximum number of hierarchy entries to return",
            "integer",
        )
        .query_param("cursor", false, "Cursor for pagination")
        .handler(handlers::list_group_hierarchy)
        .json_response_with_schema::<modkit_odata::Page<dto::GroupWithDepthDto>>(
            openapi,
            http::StatusCode::OK,
            "Paginated list of hierarchy entries with relative depth",
        )
        .with_odata_filter::<HierarchyFilterField>()
        .error_400(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router
}
