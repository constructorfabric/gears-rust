//! `OperationBuilder` route registration for the
//! `GET /account-management/v1/me` endpoint.
//!
//! Non-tenant-scoped identity endpoint: returns the authenticated
//! subject's id, type, and home tenant from `SecurityContext`. No service
//! extension is layered — the handler reads the framework-injected
//! `Extension<SecurityContext>` directly.

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use crate::api::rest::{dto, handlers};

const API_TAG: &str = "Identity";
/// Identity endpoint: `GET /account-management/v1/me`.
const ME_PATH: &str = "/account-management/v1/me";

pub(super) fn register_me_routes(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /account-management/v1/me
    OperationBuilder::get(ME_PATH)
        .operation_id("account_management.get_me")
        .summary("Return the authenticated subject's identity and home tenant")
        .description(
            "Return the authenticated subject's identity -- subject id, subject \
             type, and home tenant (`subject_tenant_id`) -- read from the \
             validated bearer token's security context. Non-tenant-scoped: call \
             it right after login to discover your single home tenant before \
             issuing tenant-scoped requests. Pure context reflection: AM performs \
             no tenant-existence check here; a dangling or deleted home tenant \
             surfaces on the tenant-scoped resource routes, not on this endpoint.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .handler(handlers::get_me)
        .json_response_with_schema::<dto::MeDto>(
            openapi,
            http::StatusCode::OK,
            "Authenticated subject identity and home tenant",
        )
        .standard_errors(openapi)
        .register(router, openapi)
}
