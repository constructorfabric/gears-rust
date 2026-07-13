//! REST API route definitions - `OpenAPI` and Axum routing.
//!
//! ## Architecture
//!
//! This gear defines REST routes with `OpenAPI` metadata organized by resource:
//! - `users` - User endpoints (5: list, get, create, update, delete)
//! - `cities` - City endpoints (5: list, get, create, update, delete)
//! - `addresses` - Address endpoints (3: get, upsert, delete)
//! - `events` - SSE event stream (1: user events)
//!
//! ## `OData` Integration
//!
//! List endpoints support `OData` query parameters via SDK filter schemas:
//! - `$filter` - Type-safe filtering using `users_info_sdk::odata::*FilterField`
//! - `$orderby` - Sorting on filterable fields
//! - `$select` - Field projection for response optimization
//! - Pagination via cursor-based `limit` and `cursor` params
//!
//! ## Layering
//!
//! Routes orchestrate but don't contain business logic:
//! - Delegate to `handlers::*` for request processing
//! - Handlers call `domain::service::Service` for business operations
//! - Use `dto::*` types for request/response serialization

use crate::api::rest::{dto, handlers};
use crate::gear::ConcreteAppServices;
use axum::Router;
use std::sync::Arc;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};

mod addresses;
mod cities;
mod events;
mod users;

pub(super) struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// Register all routes for the `users_info` gear
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    services: Arc<ConcreteAppServices>,
) -> Router {
    router = users::register_user_routes(router, openapi);
    router = cities::register_city_routes(router, openapi);
    router = addresses::register_address_routes(router, openapi);

    router = router.layer(axum::Extension(services));

    router
}

/// Register SSE route for user events
pub fn register_users_sse_route<S>(
    router: Router<S>,
    openapi: &dyn OpenApiRegistry,
    sse: toolkit::SseBroadcaster<dto::UserEvent>,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    events::register_sse_route(router, openapi, sse)
}
