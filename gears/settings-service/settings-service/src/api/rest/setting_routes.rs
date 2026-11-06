//! Routes of the administrative read surface over effective values.

use std::sync::Arc;

use axum::Router;
use settings_service_sdk::odata::{ReviewFilterField, SettingFilterField};
use toolkit::api::canonical_prelude::*;
use toolkit::api::operation_builder::{
    OperationBuilderODataExt, ResponseHeaderSpec, ResponseHeaderType,
};
use toolkit::api::{OpenApiRegistry, OperationBuilder};
use toolkit_db::{DBProvider, DbError};

use crate::api::rest::setting_dto::{
    AuditRecordDto, EffectiveValueDto, ReviewItemDto, SettingItemDto,
};
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

/// Register the read operations.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    resolver: Arc<ConcreteResolver>,
    db: Arc<DBProvider<DbError>>,
    enforcer: Arc<authz_resolver_sdk::PolicyEnforcer>,
    names: Arc<crate::infra::actor_names::ActorNameResolver>,
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
             than an unfiltered page. `$orderby` takes `key` and `category_id`; any other \
             field answers 400. Each item carries its own outcome, so a named key \
             that does not exist is reported in its entry, never as a failure of the \
             request. `needs_review eq true` narrows the page to the settings whose own \
             override at the target tenant is flagged for review, still resolved values \
             with the flag beside them, paged and counted over the narrowed set; a named \
             key that is visible but not flagged is absent from it. The flagged rows of \
             the target's subtree are `GET /settings-service/v1/settings/review`. `q` \
             narrows the page to the settings whose declaration text or shown value \
             contains it; a value masked for the caller never matches. Under `q`, a \
             dependency that is unavailable for a setting whose value the caller could \
             have matched answers 503, since the narrowing cannot be decided; a page \
             already full still answers, with a cursor. A cursor continues only the \
             listing it was minted for: bound to the $filter, the target tenant and `q`, \
             it answers 400 under another. Values are masked by classification.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .query_param(
            "tenant",
            false,
            "Target tenant id; omitted, the caller's own tenant",
        )
        .query_param(
            "q",
            false,
            "Free text that narrows the page: at least two and at most two hundred \
             characters after trimming; keeps the settings whose key, display name, \
             description, category name, Schema Default, effective value or fallback value \
             contains it, case-insensitively; combines with $filter; the cursor is bound \
             to it beside the $filter and the target, and a page it narrows carries no \
             total_count",
        )
        .query_param_typed("limit", false, "Page size", "integer")
        .query_param("cursor", false, "Cursor for pagination")
        .handler(handlers::browse_settings)
        .json_response_with_schema::<crate::api::rest::page_dto::PageDto<SettingItemDto>>(
            openapi,
            StatusCode::OK,
            "A page of per-key outcomes with its pagination cursors and, unless q \
             narrows it, the total of settings the listing holds under its filter",
        )
        .with_odata_filter::<SettingFilterField>()
        .with_odata_orderby::<crate::domain::odata::SettingOrderField>()
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/settings-service/v1/settings/review")
        .operation_id("settings_service.list_review")
        .summary("List values needing review")
        .description(
            "The override rows flagged for review at the target tenant and its \
             descendants, one item per row at the scope that holds it - the place it must be \
             fixed - not one resolved value per setting. Ordered by setting, then tenant, \
             and paged by a row-level cursor: a setting flagged at more tenants than `limit` \
             continues on the next page, and the cursor is bound to the `$filter` and the \
             target, so it is refused 400 under another. `tenant` follows the read surface: \
             omitted, the caller's own tenant; a tenant outside the caller's subtree or a \
             standalone descendant answers 403, and standalone descendants' rows are not \
             listed. `$filter` takes `category_id eq`, `key eq` and `key in (...)`, joined \
             by `and`; `needs_review` is refused 400, as is any other field or operator, \
             and a named key with no flagged row simply has no item. `$orderby` and \
             `$select` are refused 400. `limit` defaults to 50 and is capped at 200. A \
             target whose subtree exceeds the budget answers 400 `subtree_too_large`, \
             never a partial listing. A setting hidden from the caller has no rows here. Values are masked by classification - a `secret` \
             never shows its value or its reference - and the setter's identity is masked \
             for a caller without the PII entitlement.",
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
        .handler(handlers::list_review)
        // The declared surface is browse's grammar without `needs_review`: a
        // field absent from `x-odata-filter` is refused with 400, never
        // ignored, and `needs_review` is absent on purpose.
        .with_odata_filter::<ReviewFilterField>()
        .json_response_with_schema::<toolkit_odata::Page<ReviewItemDto>>(
            openapi,
            StatusCode::OK,
            "A page of flagged override rows with its pagination cursors",
        )
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
        .json_response_with_schema::<crate::api::rest::page_dto::PageDto<AuditRecordDto>>(
            openapi,
            StatusCode::OK,
            "A page of audit records, newest first, with its pagination cursors and the \
             total",
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
        .layer(axum::Extension(names))
}
