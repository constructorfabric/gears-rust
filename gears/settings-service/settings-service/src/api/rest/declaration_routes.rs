// Created: 2026-08-26 by Constructor Tech
//! Declaration route registration.
//!
//! Every route is built through `OperationBuilder` rather than mounted on the
//! router directly. That is what makes `.authenticated()` and the declared
//! responses part of the operation rather than a convention a route can skip.

use std::sync::Arc;

use axum::Router;
use toolkit::api::operation_builder::{
    OperationBuilderODataExt, ParamLocation, ParamSpec, ResponseHeaderSpec, ResponseHeaderType,
};
use toolkit::api::{OpenApiRegistry, OperationBuilder};
use toolkit_db::{DBProvider, DbError};

use toolkit::api::canonical_prelude::StatusCode;

use crate::api::rest::declaration_dto::{CreatedDeclarationDto, DeclarationDto};
use crate::api::rest::declaration_handlers as handlers;
use crate::domain::declaration::DeclarationService;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use settings_service_sdk::odata::DeclarationFilterField;

/// `OpenAPI` grouping for these operations.
const TAG: &str = "settings-declarations";

/// The step-up assertion a lifecycle change or a loosened gate presents.
fn step_up_param() -> ParamSpec {
    ParamSpec {
        name: "X-Step-Up-Token".to_owned(),
        location: ParamLocation::Header,
        required: false,
        description: Some(
            "A fresh token from the identity provider proving the caller \
             re-authenticated just now; absent, the bearer token itself is checked. \
             Required by a retire, a revive, and any edit that loosens a gate or a \
             classification."
                .to_owned(),
        ),
        param_type: "string".to_owned(),
        array: false,
    }
}

/// The `If-Match` a declaration mutation presents.
fn if_match_param() -> ParamSpec {
    ParamSpec {
        name: "If-Match".to_owned(),
        location: ParamLocation::Header,
        required: true,
        description: Some(
            "The `ETag` from the caller's last read of this declaration. Absent -> 428; \
             stale -> 412. The edit happens only against the representation the caller \
             saw."
                .to_owned(),
        ),
        param_type: "string".to_owned(),
        array: false,
    }
}

/// The `Location` of the declaration a create or revive answers with.
fn location_header() -> ResponseHeaderSpec {
    ResponseHeaderSpec::new(
        "Location",
        "URL of the declaration",
        ResponseHeaderType::String,
    )
}

/// The `ETag` every declaration representation carries.
fn etag_header() -> ResponseHeaderSpec {
    ResponseHeaderSpec::new(
        "ETag",
        "The declaration's state tag; a PATCH or DELETE presents it in `If-Match`",
        ResponseHeaderType::String,
    )
}

/// Register the declaration read and authoring routes.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<DeclarationService<DeclarationRepo>>,
    admin: Arc<handlers::ConcreteDeclarationAdmin>,
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
        .response_header(etag_header())
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    // @cpt-end:cpt-cf-settings-service-flow-setting-declarations-read:p1:inst-decl-read-1

    let router = OperationBuilder::post("/settings-service/v1/declarations")
        .operation_id("settings_service.create_declaration")
        .summary("Declare a setting")
        .description(
            "Declare a setting under an existing category. The key is composed by the \
             service from the vendor, the category's slug and the leaf name, so it can \
             never disagree with where the setting is filed, and the composed type is \
             registered in the types registry before the row is inserted. \
             `default_value` is mandatory: it is what makes resolution total, and a \
             secret-trait value type takes an empty placeholder rather than a \
             credential. The classification is derived from the value type's traits; an \
             author-supplied `secret` is refused. A key that holds a retired \
             declaration is revived instead, which requires step-up and answers `200` \
             with `reactivated`, its retained values re-entering resolution.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .param(step_up_param())
        .json_request::<crate::api::rest::declaration_dto::CreateDeclarationRequest>(
            openapi,
            "The declaration to create",
        )
        .handler(handlers::create_declaration)
        .json_response_with_schema::<CreatedDeclarationDto>(
            openapi,
            StatusCode::CREATED,
            "The created declaration, with its ETag and Location",
        )
        // `response_header` binds to the response declared last, so each success
        // status names its own headers: both a create and a revive carry them.
        .response_header(location_header())
        .response_header(etag_header())
        .json_response_with_schema::<CreatedDeclarationDto>(
            openapi,
            StatusCode::OK,
            "The revived declaration, `reactivated` true",
        )
        .response_header(location_header())
        .response_header(etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::patch("/settings-service/v1/declarations/{id}")
        .operation_id("settings_service.update_declaration")
        .summary("Edit a declaration's metadata")
        .description(
            "Edit descriptive metadata in place. Behaviour-affecting fields -- \
             `default_value`, the value type, `scope_class` -- are refused as \
             immutable, and so is any field this surface does not recognize, since an \
             unknown field must never take the immediate path. Tightening a gate or a \
             classification applies at once; loosening one -- clearing \
             `requires_step_up`, enabling `anonymous_exposable`, moving `pii` back to \
             `public` -- requires step-up. A gear's contributed declaration is refused \
             with a conflict. Requires `If-Match`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Declaration UUID")
        .param(if_match_param())
        .param(step_up_param())
        .json_request::<crate::api::rest::declaration_dto::UpdateDeclarationRequest>(
            openapi,
            "The metadata fields to change",
        )
        .handler(handlers::update_declaration)
        .json_response_with_schema::<DeclarationDto>(
            openapi,
            StatusCode::OK,
            "The updated declaration, with its refreshed ETag",
        )
        .response_header(etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .problem_response(
            openapi,
            StatusCode::PRECONDITION_FAILED,
            "The supplied If-Match is stale: re-read and retry",
        )
        .problem_response(
            openapi,
            StatusCode::PRECONDITION_REQUIRED,
            "If-Match is required on a conditional write",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::delete("/settings-service/v1/declarations/{id}")
        .operation_id("settings_service.retire_declaration")
        .summary("Retire a setting declaration")
        .description(
            "Retire a declaration: a soft delete that sets `status` to `retired` and \
             answers `200` with the retired body, not `204`. Every stored value is \
             retained and merely excluded from resolution, recoverable by re-declaring \
             the key. Retire drops a live setting out of resolution at once, so it \
             requires step-up; a gear's contributed declaration is refused with a \
             conflict. Requires `If-Match`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Declaration UUID")
        .param(if_match_param())
        .param(step_up_param())
        .handler(handlers::retire_declaration)
        .json_response_with_schema::<DeclarationDto>(
            openapi,
            StatusCode::OK,
            "The retired declaration, `status` retired",
        )
        .response_header(etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .problem_response(
            openapi,
            StatusCode::PRECONDITION_FAILED,
            "The supplied If-Match is stale: re-read and retry",
        )
        .problem_response(
            openapi,
            StatusCode::PRECONDITION_REQUIRED,
            "If-Match is required on a conditional write",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
        .layer(axum::Extension(service))
        .layer(axum::Extension(admin))
        .layer(axum::Extension(db))
        .layer(axum::Extension(enforcer))
}

#[cfg(test)]
#[path = "declaration_routes_tests.rs"]
mod declaration_routes_tests;
