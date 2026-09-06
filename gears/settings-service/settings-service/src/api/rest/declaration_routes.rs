// Created: 2026-08-26 by Constructor Tech
//! Declaration route registration.
//!
//! Every route is built through `OperationBuilder` rather than mounted on the
//! router directly. That is what makes `.authenticated()` and the declared
//! responses part of the operation rather than a convention a route can skip.

use std::sync::Arc;

use axum::Router;
use toolkit::api::operation_builder::OperationBuilderODataExt;
use toolkit::api::{OpenApiRegistry, OperationBuilder};
use toolkit_db::{DBProvider, DbError};

use toolkit::api::canonical_prelude::StatusCode;

use crate::api::rest::declaration_dto::DeclarationDto;
use crate::api::rest::declaration_handlers as handlers;
use crate::domain::declaration::DeclarationService;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use settings_service_sdk::odata::DeclarationFilterField;

/// `OpenAPI` grouping for these operations.
const TAG: &str = "settings-declarations";

/// Register the declaration read routes.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<DeclarationService<DeclarationRepo>>,
    db: Arc<DBProvider<DbError>>,
    enforcer: Arc<authz_resolver_sdk::PolicyEnforcer>,
) -> Router {
    // @cpt-begin:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-1
    let router = OperationBuilder::get("/settings-service/v1/declarations")
        .operation_id("settings_service.list_declarations")
        .summary("List setting declarations")
        .description(
            "List declarations visible to the caller. Supports OData `$filter` and \
             `$orderby` over `key`, `category_id`, `domain_affinity`, `mode`, `status` \
             and `owner_module`, with cursor pagination. `$select` is not supported and \
             is rejected rather than ignored. Each declaration carries its \
             `value_type_id` and the value type's resolved traits.",
        )
        .tag(TAG)
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-1
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-2
        .authenticated()
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-2
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-1
        .no_license_required()
        .query_param_typed("limit", false, "Page size", "integer")
        .query_param("cursor", false, "Cursor for pagination")
        .handler(handlers::list_declarations::<DeclarationRepo>)
        .json_response_with_schema::<toolkit_odata::Page<DeclarationDto>>(
            openapi,
            StatusCode::OK,
            "A page of declarations with its pagination cursors",
        )
        .with_odata_filter::<DeclarationFilterField>()
        .with_odata_orderby::<DeclarationFilterField>()
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/settings-service/v1/declarations/{id}")
        .operation_id("settings_service.get_declaration")
        .summary("Get a setting declaration")
        .description(
            "Fetch one declaration by its identifier, including its `value_type_id` \
             and the value type's resolved traits. A declaration outside the caller's \
             administrative domain is reported as absent rather than forbidden, so a \
             gated declaration's existence is not disclosed.",
        )
        .tag(TAG)
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-1
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-2
        .authenticated()
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-2
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-1
        .no_license_required()
        .path_param("id", "Declaration UUID")
        .handler(handlers::get_declaration::<DeclarationRepo>)
        .json_response_with_schema::<DeclarationDto>(
            openapi,
            StatusCode::OK,
            "The declaration and its resolved traits",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-1

    router
        .layer(axum::Extension(service))
        .layer(axum::Extension(db))
        .layer(axum::Extension(enforcer))
}

#[cfg(test)]
#[path = "declaration_routes_tests.rs"]
mod declaration_routes_tests;
