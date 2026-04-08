// @cpt-dod:cpt-cf-resource-group-dod-membership-rest-handlers:p1

use super::{dto, handlers};
use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};
use resource_group_sdk::odata::MembershipFilterField;

const API_TAG: &str = "Resource Group Memberships";

pub(super) fn register_membership_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // GET /resource-group/v1/memberships - List memberships with OData filtering
    router = OperationBuilder::get("/resource-group/v1/memberships")
        .operation_id("resource_group.list_memberships")
        .summary("List memberships")
        .description(
            "Retrieve a paginated list of memberships with OData filtering on group_id, resource_type, resource_id",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .query_param_typed(
            "limit",
            false,
            "Maximum number of memberships to return",
            "integer",
        )
        .query_param("cursor", false, "Cursor for pagination")
        .handler(handlers::list_memberships)
        .json_response_with_schema::<modkit_odata::Page<dto::MembershipDto>>(
            openapi,
            http::StatusCode::OK,
            "Paginated list of memberships",
        )
        .with_odata_filter::<MembershipFilterField>()
        .error_400(openapi)
        .error_500(openapi)
        .register(router, openapi);

    // POST /resource-group/v1/memberships/{group_id}/{resource_type}/{resource_id} - Add membership
    router = OperationBuilder::post(
        "/resource-group/v1/memberships/{group_id}/{resource_type}/{resource_id}",
    )
    .operation_id("resource_group.add_membership")
    .summary("Add membership")
    .description("Add a membership link between a resource group and a resource")
    .tag(API_TAG)
    .authenticated()
    .no_license_required()
    .path_param("group_id", "Group UUID")
    .path_param("resource_type", "GTS type path of the resource type")
    .path_param("resource_id", "Resource identifier")
    .handler(handlers::add_membership)
    .json_response_with_schema::<dto::MembershipDto>(
        openapi,
        http::StatusCode::CREATED,
        "Membership created",
    )
    .error_400(openapi)
    .error_404(openapi)
    .error_409(openapi)
    .error_500(openapi)
    .register(router, openapi);

    // DELETE /resource-group/v1/memberships/{group_id}/{resource_type}/{resource_id} - Remove membership
    router = OperationBuilder::delete(
        "/resource-group/v1/memberships/{group_id}/{resource_type}/{resource_id}",
    )
    .operation_id("resource_group.remove_membership")
    .summary("Remove membership")
    .description("Remove a membership link between a resource group and a resource")
    .tag(API_TAG)
    .authenticated()
    .no_license_required()
    .path_param("group_id", "Group UUID")
    .path_param("resource_type", "GTS type path of the resource type")
    .path_param("resource_id", "Resource identifier")
    .handler(handlers::remove_membership)
    .json_response(
        http::StatusCode::NO_CONTENT,
        "Membership removed successfully",
    )
    .error_404(openapi)
    .error_500(openapi)
    .register(router, openapi);

    router
}
