//! The anonymous read route.
//!
//! The one route of this gear declared `.anonymous().exposed()`: anonymous on
//! the authentication axis so the gateway does not ask for a token, exposed on
//! the visibility axis because routes are internal by default. Its own module
//! so the guard on every other route file can keep forbidding both.
//!
//! No rate limit is declared here: the toolkit has no in-process limiter, and
//! throttling an unauthenticated route is the gateway's responsibility.

use std::sync::Arc;

use axum::Router;
use toolkit::api::canonical_prelude::*;
use toolkit::api::operation_builder::ParamSpec;
use toolkit::api::{OpenApiRegistry, OperationBuilder};
use toolkit_db::{DBProvider, DbError};

use crate::api::rest::public_dto::PublicSettingsDto;
use crate::api::rest::public_handlers as handlers;
use crate::gear::ConcreteResolver;

const TAG: &str = "settings-public";

/// Register the anonymous read.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    resolver: Arc<ConcreteResolver>,
    db: Arc<DBProvider<DbError>>,
) -> Router {
    let router = OperationBuilder::get("/settings-service/v1/public/settings")
        .operation_id("settings_service.read_public_settings")
        .summary("Read the anonymous-exposable settings of a tenant")
        .description(
            "No authentication. The effective values, at `tenant`, of the settings an \
             administrator flagged `anonymous_exposable`, resolved by the ordinary \
             cascade, and nothing else: no other setting is listed, counted or hinted \
             at. A missing, malformed or unknown `tenant` answers 200 with an empty \
             list, the same as a tenant with nothing exposable: no 404 and no \
             distinguishable error. A failure of the database, the tenant resolver or \
             the types registry answers 503, never an empty list. Throttling of this \
             route is the gateway's responsibility.",
        )
        .tag(TAG)
        .anonymous()
        // `.anonymous()` settles the licence axis too, so `.no_license_required()`
        // is neither needed nor offered here.
        .exposed()
        .param(
            ParamSpec::query("tenant")
                .required(false)
                .description("The tenant whose exposable settings are read"),
        )
        .handler(handlers::read_public_settings)
        .json_response_with_schema::<PublicSettingsDto>(
            openapi,
            StatusCode::OK,
            "The exposable settings and their effective values, possibly none",
        )
        .error_503(openapi)
        .register(router, openapi);
    router
        .layer(axum::Extension(resolver))
        .layer(axum::Extension(db))
}

#[cfg(test)]
#[path = "public_routes_tests.rs"]
mod public_routes_tests;
