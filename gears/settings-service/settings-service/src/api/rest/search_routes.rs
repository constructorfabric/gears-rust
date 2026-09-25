// Created: 2026-09-17 by Virtuozzo International GmbH
//! The route of the search surface.

use std::sync::Arc;

use axum::Router;
use toolkit::api::canonical_prelude::*;
use toolkit::api::{OpenApiRegistry, OperationBuilder};
use toolkit_db::{DBProvider, DbError};

use crate::api::rest::search_dto::SearchHitDto;
use crate::api::rest::search_handlers::{self as handlers, ConcreteSearchService};
use crate::gear::ConcreteResolver;

const TAG: &str = "settings-search";

/// Register the search operation.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    search: Arc<ConcreteSearchService>,
    resolver: Arc<ConcreteResolver>,
    db: Arc<DBProvider<DbError>>,
    enforcer: Arc<authz_resolver_sdk::PolicyEnforcer>,
) -> Router {
    let router = OperationBuilder::get("/settings-service/v1/search")
        .operation_id("settings_service.search_settings")
        .summary("Search settings across categories")
        .description(
            "Cross-field search over every category the caller may see. Matches a \
             setting's key, description, category name, Schema Default and the overrides \
             explicitly set inside the target's subtree, under the same authorization, \
             target, visibility and `hidden` rules as browsing; results come back flat and \
             ordered by key, each hit with its category, the field that matched -- one of \
             `key`, `description`, `category_name`, `default_value` or `value` -- and its \
             declaration's `mode` as a tag that withholds nothing. A hit on the key, \
             description, category name or default belongs to the declaration and carries \
             no scope; a hit on an override names the scope and tenant where it is set. \
             Search runs over stored rows, not resolved values: an inherited value is a hit \
             where it was set, and a default is a hit on its declaration. `tenant` bounds the \
             override corpus to that subtree and is resolution context as everywhere else. \
             `q` is a case-insensitive substring of at least two characters and at most two \
             hundred; shorter or longer is refused 400. A `secret` value is never matched, so \
             an empty result says nothing about it; a `pii` value is matched only for a \
             caller entitled to read it unmasked. A page holds up to `limit` settings; a \
             setting with several matching overrides contributes one hit per override. \
             `$filter`, `$orderby` and `$select` are refused.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .query_param(
            "q",
            true,
            "Free text, at least two and at most two hundred characters",
        )
        .query_param(
            "tenant",
            false,
            "Target tenant id; omitted, the caller's own tenant",
        )
        .query_param_typed("limit", false, "Page size, in settings", "integer")
        .query_param("cursor", false, "Cursor for pagination")
        .handler(handlers::search_settings)
        .json_response_with_schema::<toolkit_odata::Page<SearchHitDto>>(
            openapi,
            StatusCode::OK,
            "A page of hits, ordered by key, with its pagination cursors",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
        .layer(axum::Extension(search))
        .layer(axum::Extension(resolver))
        .layer(axum::Extension(db))
        .layer(axum::Extension(enforcer))
}

#[cfg(test)]
#[path = "search_routes_tests.rs"]
mod search_routes_tests;
