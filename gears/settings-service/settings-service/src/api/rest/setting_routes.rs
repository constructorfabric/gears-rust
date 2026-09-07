// Created: 2026-09-07 by Constructor Tech
//! Routes of the administrative read surface over effective values.

use std::sync::Arc;

use axum::Router;
use settings_service_sdk::odata::SettingFilterField;
use toolkit::api::canonical_prelude::*;
use toolkit::api::operation_builder::{
    OperationBuilderODataExt, ResponseHeaderSpec, ResponseHeaderType,
};
use toolkit::api::{OpenApiRegistry, OperationBuilder};
use toolkit_db::{DBProvider, DbError};

use crate::api::rest::setting_dto::{AuditRecordDto, EffectiveValueDto, SettingItemDto};
use crate::api::rest::setting_handlers as handlers;
use crate::gear::ConcreteResolver;

const TAG: &str = "settings-values";

fn etag_header() -> ResponseHeaderSpec {
    ResponseHeaderSpec::new(
        "ETag",
        "The value state tag of the requested scope's own row, or of the absent state; \
         a write at that scope presents it in `If-Match`",
        ResponseHeaderType::String,
    )
}

/// Register the two read operations.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    resolver: Arc<ConcreteResolver>,
    db: Arc<DBProvider<DbError>>,
    enforcer: Arc<authz_resolver_sdk::PolicyEnforcer>,
) -> Router {
    let router = OperationBuilder::get("/settings-service/v1/settings")
        .operation_id("settings_service.browse_settings")
        .summary("Browse effective values")
        .description(
            "Resolve a page of settings at one scope. `tenant` is resolution context, \
             never a filter: omitted, it is the caller's own tenant, which for a platform \
             administrator is the root and therefore platform scope; a tenant outside the \
             caller's subtree, or a standalone descendant, answers 403. OData `$filter` \
             supports `category_id eq`, `key eq`, `key in (...)` and `needs_review eq true`, \
             joined by `and`; an unmapped field or unsupported operator answers 400 rather \
             than an unfiltered page. Each item carries its own outcome, so a named key \
             that does not exist is reported in its entry, never as a failure of the \
             request. `needs_review eq true` lists the flagged override rows in the \
             caller's subtree instead of resolved values. Values are masked by \
             classification.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .query_param(
            "tenant",
            false,
            "Target tenant id; omitted, the caller's own tenant",
        )
        .query_param_typed("limit", false, "Page size", "integer")
        .query_param("cursor", false, "Cursor for pagination")
        .handler(handlers::browse_settings)
        .json_response_with_schema::<toolkit_odata::Page<SettingItemDto>>(
            openapi,
            StatusCode::OK,
            "A page of per-key outcomes with its pagination cursors",
        )
        .with_odata_filter::<SettingFilterField>()
        .with_odata_orderby::<SettingFilterField>()
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/settings-service/v1/settings/{key}")
        .operation_id("settings_service.get_setting")
        .summary("Read an effective value")
        .description(
            "Resolve one setting at a scope: the value masked by classification, its \
             source and source scope, the value type's traits, the inheritance trail with \
             per-entry setter identity, a leak-safe `last_change_at`, the scope's own \
             review flag when its override is flagged, and in `ETag` the value state tag a \
             write at that scope must present. A key with no declaration answers 404; a \
             retired declaration answers 410 with the `SETTING_RETIRED` precondition; a \
             target outside the caller's subtree or a standalone descendant answers 403.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .query_param(
            "tenant",
            false,
            "Target tenant id; omitted, the caller's own tenant",
        )
        .handler(handlers::get_setting)
        .json_response_with_schema::<EffectiveValueDto>(
            openapi,
            StatusCode::OK,
            "The effective value with its trace, and the value state tag in ETag",
        )
        .response_header(etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .problem_response(
            openapi,
            http::StatusCode::GONE,
            "Gone: the declaration is retired",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/settings-service/v1/settings/{key}/history")
        .operation_id("settings_service.get_setting_history")
        .summary("Read a setting's history at a scope")
        .description(
            "The mutation history of one setting at one scope, newest first and \
             cursor-paginated, served from the gear's own audit store. `tenant` follows the \
             read surface: omitted, the caller's own tenant; a tenant outside the caller's \
             subtree or a standalone descendant answers 403. A `pii` actor, and `pii` values, \
             are masked for a caller without the PII entitlement; a secret was never recorded \
             in plaintext. A retired declaration keeps its history. An empty history is an \
             empty page. `$filter`, `$orderby` and `$select` are refused.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .query_param(
            "tenant",
            false,
            "Target tenant id; omitted, the caller's own tenant",
        )
        .query_param_typed("limit", false, "Page size", "integer")
        .query_param("cursor", false, "Cursor for pagination")
        .handler(handlers::get_history)
        .json_response_with_schema::<toolkit_odata::Page<AuditRecordDto>>(
            openapi,
            StatusCode::OK,
            "A page of audit records, newest first, with its pagination cursors",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
        .layer(axum::Extension(resolver))
        .layer(axum::Extension(db))
        .layer(axum::Extension(enforcer))
}
