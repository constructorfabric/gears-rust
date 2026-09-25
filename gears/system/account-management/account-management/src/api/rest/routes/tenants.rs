//! `OperationBuilder` route registration for the
//! `/account-management/v1/tenants*` endpoints.

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};

use account_management_sdk::TenantInfoFilterField;

use crate::api::rest::{dto, handlers};

const API_TAG: &str = "Tenants";

/// Collection path: `POST /tenants`.
const COLLECTION_PATH: &str = "/account-management/v1/tenants";
/// Entry path: `GET / PATCH / DELETE /tenants/{tenant_id}`.
const ENTRY_PATH: &str = "/account-management/v1/tenants/{tenant_id}";
/// Children path: `GET /tenants/{tenant_id}/children`.
const CHILDREN_PATH: &str = "/account-management/v1/tenants/{tenant_id}/children";
/// Suspend sub-resource path.
///
/// AIP-136 specifies the colon-method shape
/// `POST /tenants/{tenant_id}:suspend`, but axum 0.8.x pins
/// `matchit = "=0.8.4"` which cannot split `{param}:suffix` in one
/// segment. The sub-resource form here is the in-tree fallback (same
/// pattern as `gears/mini-chat/.../routes/turns.rs::retry`); cutover
/// to the colon-method waits on axum bumping matchit (`>= 0.9.2`,
/// see <https://github.com/tokio-rs/axum/pull/3702>) and the upstream
/// routing issue <https://github.com/tokio-rs/axum/issues/3140>.
const SUSPEND_PATH: &str = "/account-management/v1/tenants/{tenant_id}/suspend";
/// Unsuspend sub-resource path. Same fallback rationale as
/// [`SUSPEND_PATH`].
const UNSUSPEND_PATH: &str = "/account-management/v1/tenants/{tenant_id}/unsuspend";

#[allow(
    clippy::too_many_lines,
    reason = "seven OperationBuilder chains in linear sequence"
)]
pub(super) fn register_tenants_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // POST /account-management/v1/tenants
    router = OperationBuilder::post(COLLECTION_PATH)
        .operation_id("account_management.create_tenant")
        .summary("Create a child tenant")
        .description(
            "Create a non-root tenant under an existing parent tenant. The server validates \
             the chained `tenant_type` against the published GTS schema, derives the canonical \
             UUIDv5 internally, and checks hierarchy constraints (parent exists + is `active`) \
             before invoking the configured IdP tenant-provisioning contract. The new tenant \
             id is server-allocated; clients receive it in the response body and in the \
             `Location` header. Returns HTTP 201 Created with the post-create tenant \
             projection in `status=active`.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .json_request::<dto::TenantCreateRequestDto>(openapi, "Tenant provisioning payload")
        .handler(handlers::create_tenant)
        .json_response_with_schema::<dto::TenantDto>(
            openapi,
            http::StatusCode::CREATED,
            "Tenant created",
        )
        .standard_errors(openapi)
        // 501 (`idp_unsupported_operation`) and 503 (`idp_unavailable` /
        // `service_unavailable`) are outside the standard set and
        // surface from the saga's `IdpPluginClient::provision_tenant`
        // call; declare them explicitly.
        .problem_response(
            openapi,
            http::StatusCode::NOT_IMPLEMENTED,
            "IdP plugin does not support tenant provisioning",
        )
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "IdP / PDP / types-registry transport failure",
        )
        .register(router, openapi);

    // GET /account-management/v1/tenants/{tenant_id}
    router = OperationBuilder::get(ENTRY_PATH)
        .operation_id("account_management.get_tenant")
        .summary("Read tenant details")
        .description(
            "Read a tenant by id. The caller's PDP-narrowed scope clamps the lookup to its \
             authorised subtree at the database -- a caller whose scope does not contain \
             `tenant_id` sees `not_found`, never the row. Soft-deleted (`status=deleted`) \
             tenants are SDK-visible; AM-internal `provisioning` rows are never surfaced.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", "Tenant UUID")
        .handler(handlers::get_tenant)
        .json_response_with_schema::<dto::TenantDto>(
            openapi,
            http::StatusCode::OK,
            "Tenant details",
        )
        .standard_errors(openapi)
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "PDP / DB transport failure",
        )
        .register(router, openapi);

    // PATCH /account-management/v1/tenants/{tenant_id}
    router = OperationBuilder::patch(ENTRY_PATH)
        .operation_id("account_management.update_tenant")
        .summary("Update mutable tenant fields")
        .description(
            "Patch mutable tenant fields. Only `name` may be changed via this endpoint; \
             immutable fields (`parent_id`, `tenant_type`, `self_managed`) and lifecycle \
             fields (`status`) are rejected by `additionalProperties: false`. Lifecycle \
             transitions go through `POST /tenants/{tenant_id}/suspend` / `/unsuspend` \
             (sub-resource fallback for the AIP-136 colon-method) and `DELETE \
             /tenants/{tenant_id}`. An empty patch (no fields supplied) is a 400.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", "Tenant UUID")
        .json_request::<dto::TenantUpdateRequestDto>(openapi, "Mutable-fields patch")
        .handler(handlers::update_tenant)
        .json_response_with_schema::<dto::TenantDto>(
            openapi,
            http::StatusCode::OK,
            "Updated tenant",
        )
        .standard_errors(openapi)
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "PDP / DB / types-registry transport failure",
        )
        .register(router, openapi);

    // DELETE /account-management/v1/tenants/{tenant_id}
    router = OperationBuilder::delete(ENTRY_PATH)
        .operation_id("account_management.delete_tenant")
        .summary("Soft-delete a tenant")
        .description(
            "Soft-delete a tenant. Flips `status=deleted` and stamps `deleted_at` to arm \
             the retention sweep; the row is hard-deleted by the background reaper once \
             the retention window elapses. Preconditions: the tenant has no non-deleted \
             children (`tenant_has_children`), no resources owned in resource-group \
             (`tenant_has_resources`), and is not the platform root \
             (`root_tenant_cannot_delete`). Returns 204 No Content on success; the \
             caller can re-read the post-delete projection (status=`deleted`, \
             `deleted_at` set) via `GET /tenants/{tenant_id}`.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", "Tenant UUID")
        .handler(handlers::delete_tenant)
        .no_content_response(http::StatusCode::NO_CONTENT, "Tenant soft-deleted")
        .standard_errors(openapi)
        .problem_response(
            openapi,
            http::StatusCode::NOT_IMPLEMENTED,
            "IdP plugin does not support tenant deprovisioning",
        )
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "IdP / PDP / DB / RG transport failure",
        )
        .register(router, openapi);

    // POST /account-management/v1/tenants/{tenant_id}/suspend
    router = OperationBuilder::post(SUSPEND_PATH)
        .operation_id("account_management.suspend_tenant")
        .summary("Suspend an active tenant")
        .description(
            "Flip the target tenant's `status` from `active` to `suspended`. Idempotent: a \
             second call on an already-suspended row returns 200 with the same projection \
             and does NOT bump `updated_at` (the service short-circuits on same-to-same \
             inside the SERIALIZABLE write). AIP-136 specifies the colon-method shape \
             `POST /tenants/{tenant_id}:suspend`, but axum 0.8.x pins `matchit = \"=0.8.4\"` \
             which cannot split `{param}:suffix` in a single segment; this endpoint uses \
             the in-tree sub-resource fallback until axum bumps matchit (PR \
             https://github.com/tokio-rs/axum/pull/3702).",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", "Tenant UUID")
        .handler(handlers::suspend_tenant)
        .json_response_with_schema::<dto::TenantDto>(
            openapi,
            http::StatusCode::OK,
            "Suspended tenant",
        )
        .standard_errors(openapi)
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "PDP / DB transport failure",
        )
        .register(router, openapi);

    // POST /account-management/v1/tenants/{tenant_id}/unsuspend
    router = OperationBuilder::post(UNSUSPEND_PATH)
        .operation_id("account_management.unsuspend_tenant")
        .summary("Unsuspend a suspended tenant")
        .description(
            "Flip the target tenant's `status` from `suspended` back to `active`. Idempotent \
             on already-active rows (same short-circuit as `suspend_tenant`). Sub-resource \
             fallback for the AIP-136 colon-method -- same axum/matchit constraint as the \
             `suspend` endpoint.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", "Tenant UUID")
        .handler(handlers::unsuspend_tenant)
        .json_response_with_schema::<dto::TenantDto>(
            openapi,
            http::StatusCode::OK,
            "Unsuspended tenant",
        )
        .standard_errors(openapi)
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "PDP / DB transport failure",
        )
        .register(router, openapi);

    // GET /account-management/v1/tenants/{tenant_id}/children
    router = OperationBuilder::get(CHILDREN_PATH)
        .operation_id("account_management.list_tenant_children")
        .summary("List children of a tenant")
        .description(
            "List the children of the given tenant. Default: direct children only. With \
             `recursive=true`: every descendant visible to the caller under the same \
             visibility rules (a self-managed direct child of any visible tenant is listed \
             as an identity; nothing below a barrier is), and every item carries \
             `ancestors` -- the chain from the child of `tenant_id` down to the item's \
             direct parent, empty for a direct child; the key is absent without the flag. \
             Cursor-paginated with `(created_at ASC, id ASC)` as the effective sort so rows \
             sharing a `created_at` timestamp stay disambiguated across pages; a cursor is \
             bound to the mode it was minted in, and replaying it with a different \
             `recursive` value is 400 `FILTER_MISMATCH`, like a changed `$filter` (a \
             cursor issued before mode binding existed is accepted by the direct listing \
             only). \
             Soft-deleted rows are hidden by default -- opt in with \
             `?$filter=status eq 'deleted'`. AM-internal `provisioning` rows are never \
             surfaced. The parent must exist and be SDK-visible, otherwise the call \
             collapses to `not_found`. `$filter` operators: `name` -- `eq`, `ne`, `in`, \
             `contains(name,'...')`, `startswith(name,'...')`, `endswith(name,'...')` \
             (on PostgreSQL: case-sensitive, and `%` / `_` in the literal match only \
             themselves; on SQLite: case-insensitive for ASCII letters, and `%`, `_` and \
             `\\` in the literal are not matched literally); `status`, \
             `tenant_type` -- `eq`, `ne`, `in`; `id`, `tenant_type_uuid` -- `eq`, `ne`, \
             `in`; `self_managed` -- `eq`, `ne`; `created_at`, `updated_at` -- `eq`, `ne`, \
             `gt`, `ge`, `lt`, `le`, `in`; `and`, `or`, `not`, parentheses. The generated \
             `$filter` / `$orderby` parameter metadata lists operators by field kind only; \
             the per-field restrictions above are enforced by the server and win.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("tenant_id", "Tenant UUID")
        .query_param_typed(
            "limit",
            false,
            "Maximum number of children to return",
            "integer",
        )
        .query_param("cursor", false, "Cursor for pagination")
        .query_param_typed(
            "recursive",
            false,
            "When `true`, list every descendant visible to the caller instead of direct \
             children only, and carry `ancestors` on every item. Exactly `true` or \
             `false`; anything else is 400 `validation`. Default `false`.",
            "boolean",
        )
        .handler(handlers::list_tenant_children)
        .json_response_with_schema::<toolkit_odata::Page<dto::TenantDto>>(
            openapi,
            http::StatusCode::OK,
            "Paginated list of child tenants (direct, or the visible subtree with recursive=true)",
        )
        .with_odata_filter::<TenantInfoFilterField>()
        .with_odata_orderby::<TenantInfoFilterField>()
        .standard_errors(openapi)
        .problem_response(
            openapi,
            http::StatusCode::SERVICE_UNAVAILABLE,
            "PDP / DB transport failure",
        )
        .register(router, openapi);

    router
}
