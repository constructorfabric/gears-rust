//! `OperationBuilder` route registration for the
//! `GET /account-management/v1/admin/context` endpoint.
//!
//! Startup admin-context endpoint: returns the authenticated subject's id,
//! type, home tenant, admin mode, and capability hints from
//! `SecurityContext`. No service extension is layered — the handler reads
//! the framework-injected `Extension<SecurityContext>` directly.

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use crate::api::rest::{dto, handlers};

const API_TAG: &str = "Identity";
/// Admin-context endpoint: `GET /account-management/v1/admin/context`.
const ADMIN_CONTEXT_PATH: &str = "/account-management/v1/admin/context";

pub(super) fn register_admin_context_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    // GET /account-management/v1/admin/context
    OperationBuilder::get(ADMIN_CONTEXT_PATH)
        .operation_id("account_management.get_admin_context")
        .summary("Return the authenticated admin's startup context")
        .description(
            "Return the authenticated subject's startup admin context -- \
             subject id, subject type, home tenant, admin mode \
             (`platform`/`tenant`), and coarse capability hints -- read from \
             the validated bearer token's security context. Mode and \
             capabilities derive from the `subject_type` role marker; the \
             backend remains the final authority on every action and \
             capability hints are advisory for UI gating only. v0 derives the \
             role from the NON-PRODUCTION static auth stub and sets \
             `non_production_auth=true` when that stub is in effect. Enabled \
             gears are not included here -- read them from the gear \
             orchestrator.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .handler(handlers::get_admin_context)
        .json_response_with_schema::<dto::AdminContextDto>(
            openapi,
            http::StatusCode::OK,
            "Authenticated admin startup context",
        )
        .standard_errors(openapi)
        .register(router, openapi)
}
