//! Routes of the maintenance surface.
//!
//! Authenticated and internal: none is `.exposed()`, so the gateway does not
//! publish them and a scheduler inside the cluster calls them directly.

use std::sync::Arc;

use axum::Router;
use toolkit::api::canonical_prelude::*;
use toolkit::api::{OpenApiRegistry, OperationBuilder};
use toolkit_db::{DBProvider, DbError};

use crate::api::rest::maintenance_dto::{PruneResultDto, SweepResultDto};
use crate::api::rest::maintenance_handlers::{self as handlers, MaintenanceSettings};
use crate::gear::ConcreteResolver;
use crate::infra::value_writes::WriteCoordinator;

const TAG: &str = "settings-maintenance";

/// Register the two maintenance operations.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    writes: Arc<WriteCoordinator>,
    db: Arc<DBProvider<DbError>>,
    settings: Arc<MaintenanceSettings>,
    resolver: Arc<ConcreteResolver>,
    enforcer: Arc<authz_resolver_sdk::PolicyEnforcer>,
) -> Router {
    let router = OperationBuilder::post("/settings-service/v1/maintenance/pending-secrets/sweep")
        .operation_id("settings_service.sweep_pending_secrets")
        .summary("Run one pending-secret sweep pass")
        .description(
            "Releases the staged secrets nobody claimed whose window has passed, once, \
             with the logic the managed lifecycle runs every minute when \
             `background.pending_secret_sweep` is on. Meant for an external scheduler \
             when that loop is off. Platform scope only: the caller's tenant is the root \
             tenant and the policy permits `maintain`; no step-up is asked and a service \
             principal is accepted. Answers how many were released and whether more may \
             remain: `more` is true only when a full page was found and some of it was \
             released, so a scheduler calls again while it is true and stops when it \
             is false, as it does when the store is down. With no secret storage \
             configured it answers zero.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .handler(handlers::sweep_pending_secrets)
        .json_response_with_schema::<SweepResultDto>(
            openapi,
            StatusCode::OK,
            "How many staged secrets were released, and whether more may remain",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::post("/settings-service/v1/maintenance/audit/prune")
        .operation_id("settings_service.prune_audit")
        .summary("Run one bounded audit retention pass")
        .description(
            "Deletes audit records past their retention horizon, once, with the logic \
             the managed lifecycle runs every day when `background.audit_retention` is \
             on, under a budget a tenth of a tick's so the request stays short. Meant \
             for an external scheduler when that loop is off. Platform scope only, as \
             the sweep. Answers how many records were deleted and whether the budget \
             ran out with more possibly waiting: call again until `more` is false.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .handler(handlers::prune_audit)
        .json_response_with_schema::<PruneResultDto>(
            openapi,
            StatusCode::OK,
            "How many audit records were deleted, and whether more may remain",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
        .layer(axum::Extension(writes))
        .layer(axum::Extension(db))
        .layer(axum::Extension(settings))
        .layer(axum::Extension(resolver))
        .layer(axum::Extension(enforcer))
}

#[cfg(test)]
#[path = "maintenance_routes_tests.rs"]
mod maintenance_routes_tests;
