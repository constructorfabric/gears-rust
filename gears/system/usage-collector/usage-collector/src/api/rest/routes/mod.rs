//! `OperationBuilder` route registration for the foundation REST surface.
//! Per-resource registrars live under this module and are composed by
//! [`register_routes`], which then attaches the shared
//! `Extension<Arc<Service>>` layer consumed by every handler.

use std::sync::Arc;

use axum::Router;
use toolkit::api::OpenApiRegistry;

use crate::api::rest::{dto, handlers};
use crate::domain::Service;

mod usage_records;
mod usage_types;

/// Register the foundation REST routes onto `router`. Called once
/// from [`crate::module::UsageCollectorModule::register_rest`].
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<Service>,
) -> Router {
    router = usage_types::register_usage_type_routes(router, openapi);
    router = usage_records::register_usage_record_routes(router, openapi);
    router.layer(axum::Extension(service))
}
