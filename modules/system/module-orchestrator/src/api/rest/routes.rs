use axum::http;
use axum::{Extension, Router};
use modkit::api::{OpenApiRegistry, OperationBuilder};
use std::sync::Arc;

use super::dto::ModuleDto;
use super::handlers;
use crate::domain::service::ModulesService;

/// Register all REST routes for the module orchestrator
#[allow(clippy::needless_pass_by_value)]
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ModulesService>,
) -> Router {
    // GET /module-orchestrator/v1/modules - List all registered modules
    router = OperationBuilder::get("/module-orchestrator/v1/modules")
        .operation_id("module_orchestrator.list_modules")
        .summary("List all registered modules")
        .description(
            "Returns a list of all compiled-in and out-of-process modules with their \
         capabilities, dependencies, running instances, and deployment mode.",
        )
        .tag("Module Orchestrator")
        .authenticated()
        .no_license_required()
        .handler(handlers::list_modules)
        .json_response_with_schema::<Vec<ModuleDto>>(
            openapi,
            http::StatusCode::OK,
            "List of registered modules",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router = router.layer(Extension(service));

    router
}
