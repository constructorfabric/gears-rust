use axum::http;
use axum::{Extension, Router};
use std::sync::Arc;
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use super::dto::GearDto;
use super::handlers;
use crate::domain::service::GearsService;

/// Register all REST routes for the directory service
#[allow(clippy::needless_pass_by_value)]
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<GearsService>,
) -> Router {
    // GET /service-discovery/v1/gears - List all registered gears
    router = OperationBuilder::get("/service-discovery/v1/gears")
        .operation_id("service_discovery.list_gears")
        .summary("List all registered gears")
        .description(
            "Returns a list of all compiled-in and out-of-process gears with their \
         capabilities, dependencies, running instances, and deployment mode.",
        )
        .tag("Service Discovery")
        .authenticated()
        .no_license_required()
        .handler(handlers::list_gears)
        .json_array_response_with_schema::<GearDto>(
            openapi,
            http::StatusCode::OK,
            "List of registered gears",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router = router.layer(Extension(service));

    router
}
