// Created: 2026-09-07 by Constructor Tech
//! Routes of the tenant access restriction surface.

use std::sync::Arc;

use axum::Router;
use toolkit::api::canonical_prelude::*;
use toolkit::api::operation_builder::{
    ParamLocation, ParamSpec, ResponseHeaderSpec, ResponseHeaderType,
};
use toolkit::api::{OpenApiRegistry, OperationBuilder};
use toolkit_db::{DBProvider, DbError};

use crate::api::rest::access_dto::{AccessReadDto, RestrictionDto, SetRestrictionRequest};
use crate::api::rest::access_handlers::{self as handlers, ConcreteAccessService};

const TAG: &str = "settings-tenant-access";

fn tenant_param() -> ParamSpec {
    ParamSpec {
        name: "tenant".to_owned(),
        location: ParamLocation::Query,
        required: true,
        description: Some("The tenant the restriction is about, never the caller".to_owned()),
        param_type: "string".to_owned(),
        array: false,
    }
}

fn if_match_param() -> ParamSpec {
    ParamSpec {
        name: "If-Match".to_owned(),
        location: ParamLocation::Header,
        required: true,
        description: Some("The restriction state tag the caller last read for this pair: the stored row's, or `absent`".to_owned()),
        param_type: "string".to_owned(),
        array: false,
    }
}

fn etag_header() -> ResponseHeaderSpec {
    ResponseHeaderSpec::new(
        "ETag",
        "The pair's restriction state tag; a PUT or DELETE presents it in `If-Match`",
        ResponseHeaderType::String,
    )
}

/// Register the four restriction operations.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteAccessService>,
    db: Arc<DBProvider<DbError>>,
    enforcer: Arc<authz_resolver_sdk::PolicyEnforcer>,
) -> Router {
    let router = OperationBuilder::get("/settings-service/v1/settings/{key}/permissions")
        .operation_id("settings_service.read_tenant_access")
        .summary("Read one tenant's access to a setting")
        .description(
            "The pair's stored restriction if any, the effective access - the strictest row on \
             the tenant's root-to-self chain, `overridable` when none - the tenant that supplies \
             it, and in `ETag` the tag a PUT or DELETE must present; the absent-state tag when \
             no row exists. The target must be the caller's own tenant or a non-standalone \
             descendant; a setting hidden from the caller is absent.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .param(tenant_param())
        .handler(handlers::read_access)
        .json_response_with_schema::<AccessReadDto>(
            openapi,
            StatusCode::OK,
            "The tenant's access, with its state tag",
        )
        .response_header(etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::put("/settings-service/v1/settings/{key}/permissions")
        .operation_id("settings_service.set_tenant_access")
        .summary("Restrict a descendant tenant's access to a setting")
        .description(
            "Store `read_only` or `hidden` for a strict descendant that is not standalone; a \
             caller cannot restrict itself, an ancestor or a sibling. `overridable` is the \
             absence of a row and is expressed by DELETE. Requires `delegate` on the setting and \
             `If-Match` against the tag the read returned. The row is stored even when a \
             stricter ancestor already dominates it and takes effect when that is lifted. The \
             target and its descendants are evicted from the cache.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .param(tenant_param())
        .param(if_match_param())
        .json_request::<SetRestrictionRequest>(openapi, "The access to store")
        .handler(handlers::set_access)
        .json_response_with_schema::<AccessReadDto>(
            openapi,
            StatusCode::OK,
            "The stored restriction, the resulting effective access and the refreshed tag",
        )
        .response_header(etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .problem_response(
            openapi,
            http::StatusCode::PRECONDITION_FAILED,
            "Precondition Failed: the row changed since it was read",
        )
        .problem_response(
            openapi,
            http::StatusCode::PRECONDITION_REQUIRED,
            "Precondition Required: If-Match is missing",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::delete("/settings-service/v1/settings/{key}/permissions")
        .operation_id("settings_service.clear_tenant_access")
        .summary("Clear a descendant tenant's restriction on a setting")
        .description(
            "Delete the pair's row, making this level `overridable` while an ancestor row may \
             still narrow the effective result. Clearing an absent row is a no-op that still \
             requires the absent-state tag. Requires `delegate` and `If-Match`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .param(tenant_param())
        .param(if_match_param())
        .handler(handlers::clear_access)
        .json_response_with_schema::<AccessReadDto>(
            openapi,
            StatusCode::OK,
            "The resulting effective access and the absent-state tag",
        )
        .response_header(etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .problem_response(
            openapi,
            http::StatusCode::PRECONDITION_FAILED,
            "Precondition Failed",
        )
        .problem_response(
            openapi,
            http::StatusCode::PRECONDITION_REQUIRED,
            "Precondition Required",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/settings-service/v1/settings/{key}/permissions/all")
        .operation_id("settings_service.list_tenant_access")
        .summary("List the stored restrictions on a setting in the caller's subtree")
        .description(
            "Every stored restriction for the setting on the caller's own tenant and its \
             reachable descendants, each with its state tag; standalone subtrees are left out. \
             The list is small by nature and returned whole.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .handler(handlers::list_access)
        .json_response_with_schema::<toolkit_odata::Page<RestrictionDto>>(
            openapi,
            StatusCode::OK,
            "The stored restrictions",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
        .layer(axum::Extension(service))
        .layer(axum::Extension(db))
        .layer(axum::Extension(enforcer))
}
