//! `OperationBuilder` route registration for the
//! `/account-management/v1/tenants/{tenant_id}/users*` endpoints.

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};

use crate::api::rest::{dto, handlers};

const API_TAG: &str = "Tenant Users";

/// Collection path: `GET / POST /tenants/{tenant_id}/users`.
const COLLECTION_PATH: &str = "/account-management/v1/tenants/{tenant_id}/users";
/// Single-user path: `DELETE /tenants/{tenant_id}/users/{user_id}`.
const ENTRY_PATH: &str = "/account-management/v1/tenants/{tenant_id}/users/{user_id}";

pub(super) fn register_users_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /account-management/v1/tenants/{tenant_id}/users
    router = OperationBuilder::get(COLLECTION_PATH)
        .operation_id("account_management.list_tenant_users")
        .summary("List users provisioned in a tenant")
        .description(
            "List users provisioned in the tenant via the configured IdP plugin. \
             Filter via OData `$filter` over `id` (Uuid), `username`, `email`, \
             `display_name`, `first_name`, `last_name` (String) with operators \
             `eq`, `ne`, `in`, and `contains` / `startswith` / `endswith` on \
             String fields (case-insensitive); combine via `and` / `or` / `not`. \
             Sort via `$orderby` over the same fields (default: `username ASC, id ASC` \
             with `id ASC` tiebreaker appended even when the caller supplies their own \
             order). Point lookup is `$filter=id eq <uuid>` with `top=1`: empty page \
             is the canonical \"user absent\" signal (no 404). Cursor pagination is \
             opaque; caller MUST NOT change `$filter` or `$orderby` between \
             continuation requests with the same cursor. AM persists no local user \
             state; every read is a pass-through to the IdP.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", "Tenant UUID")
        .with_odata_filter::<account_management_sdk::IdpUserFilterField>()
        .with_odata_orderby::<account_management_sdk::IdpUserFilterField>()
        .handler(handlers::list_users)
        .json_response_with_schema::<toolkit_odata::Page<dto::UserDto>>(
            openapi,
            http::StatusCode::OK,
            "Paginated list of tenant users",
        )
        .standard_errors(openapi)
        // 501 / 503 are outside the standard set: `idp_unsupported_operation`
        // and `idp_unavailable` surface from `IdpPluginClient`.
        .problem_response(
            openapi,
            http::StatusCode::NOT_IMPLEMENTED,
            "IdP plugin does not support the requested operation",
        )
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "IdP unavailable (transport failure or timeout)",
        )
        .register(router, openapi);

    // POST /account-management/v1/tenants/{tenant_id}/users
    router = OperationBuilder::post(COLLECTION_PATH)
        .operation_id("account_management.create_tenant_user")
        .summary("Provision a user in a tenant")
        .description(
            "Provision a user in the tenant via the configured IdP plugin. AM persists no \
             local user state -- the IdP becomes the source of truth on success. Returns \
             HTTP 201 Created with the projected user body. AM does NOT expose a per-user \
             GET; clients that need to re-read the user use the filtered listing \
             `GET /tenants/{tenant_id}/users?$filter=id eq <uuid>&$top=1`.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", "Tenant UUID")
        .json_request::<dto::UserCreateRequestDto>(openapi, "User provisioning payload")
        .handler(handlers::create_user)
        .json_response_with_schema::<dto::UserDto>(
            openapi,
            http::StatusCode::CREATED,
            "Tenant user provisioned",
        )
        .standard_errors(openapi)
        .problem_response(
            openapi,
            http::StatusCode::NOT_IMPLEMENTED,
            "IdP plugin does not support user provisioning",
        )
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "IdP unavailable (transport failure or timeout)",
        )
        .register(router, openapi);

    // DELETE /account-management/v1/tenants/{tenant_id}/users/{user_id}
    router = OperationBuilder::delete(ENTRY_PATH)
        .operation_id("account_management.delete_tenant_user")
        .summary("Deprovision a user from a tenant")
        .description(
            "Deprovision a user from the tenant via the configured IdP plugin. Idempotent: \
             returns 204 No Content whether the IdP removed the row or reported the user as \
             already absent; a subsequent retry of the same DELETE also returns 204. \
             Provider implementations MUST NOT silently no-op on a genuinely supported \
             mutating operation -- unsupported deprovision surfaces as \
             `code=idp_unsupported_operation`.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", "Tenant UUID")
        .path_param("user_id", "IdP-issued UUID user identifier")
        .handler(handlers::delete_user)
        .no_content_response(http::StatusCode::NO_CONTENT, "User deprovisioned")
        .standard_errors(openapi)
        .problem_response(
            openapi,
            http::StatusCode::NOT_IMPLEMENTED,
            "IdP plugin does not support user deprovisioning",
        )
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "IdP unavailable (transport failure or timeout)",
        )
        .register(router, openapi);

    // PATCH /account-management/v1/tenants/{tenant_id}/users/{user_id}
    router = OperationBuilder::patch(ENTRY_PATH)
        .operation_id("account_management.update_tenant_user")
        .summary("Update mutable attributes of a tenant user")
        .description(
            "Apply a JSON Merge Patch (RFC 7396) to a user's mutable attributes via the \
             configured IdP plugin. AM persists no local user state; the update is a \
             pass-through and the IdP remains the source of truth. Omitted fields are left \
             unchanged; an explicit `null` clears a nullable profile field (`email`, \
             `display_name`, `first_name`, `last_name`); `username` may be renamed (send a \
             value) but NOT cleared (an explicit `null` is rejected); `password` sets a new \
             credential and is never echoed back. Returns 200 OK with the post-update user \
             body projected through `gts.cf.core.am.user.v1~`. An empty patch is rejected \
             with `code=validation`. Unlike DELETE, a patch against a user the IdP reports \
             absent returns 404 -- it is NOT folded into success. A username rename that \
             collides with an existing login returns 409.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", "Tenant UUID")
        .path_param("user_id", "IdP-issued UUID user identifier")
        .json_request::<dto::UserUpdateRequestDto>(openapi, "User attributes merge patch")
        .handler(handlers::update_user)
        .json_response_with_schema::<dto::UserDto>(
            openapi,
            http::StatusCode::OK,
            "Updated tenant user",
        )
        .standard_errors(openapi)
        // 501 / 503 are outside the standard set: `idp_unsupported_operation`
        // and `idp_unavailable` surface from `IdpPluginClient`.
        .problem_response(
            openapi,
            http::StatusCode::NOT_IMPLEMENTED,
            "IdP plugin does not support user updates",
        )
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "IdP unavailable (transport failure or timeout)",
        )
        .register(router, openapi);

    router
}
