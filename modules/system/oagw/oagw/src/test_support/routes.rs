use axum::Router;
use axum::routing::{any, get, post};

use crate::api::rest::handlers::{proxy as proxy_h, route as route_h, upstream as upstream_h};
use crate::module::AppState;

/// Create a test router with all OAGW routes registered.
///
/// Uses manual route registration without OpenAPI metadata.
/// Suitable for integration tests that don't need an `OpenApiRegistry`.
pub fn test_router(state: AppState, ctx: modkit_security::SecurityContext) -> Router {
    Router::new()
        // Upstream CRUD
        .route(
            "/oagw/v1/upstreams",
            post(upstream_h::create_upstream).get(upstream_h::list_upstreams),
        )
        .route(
            "/oagw/v1/upstreams/{id}",
            get(upstream_h::get_upstream)
                .put(upstream_h::update_upstream)
                .delete(upstream_h::delete_upstream),
        )
        // Route CRUD
        .route(
            "/oagw/v1/routes",
            post(route_h::create_route).get(route_h::list_routes),
        )
        .route(
            "/oagw/v1/routes/{id}",
            get(route_h::get_route)
                .put(route_h::update_route)
                .delete(route_h::delete_route),
        )
        // Proxy
        .route("/oagw/v1/proxy/{*path}", any(proxy_h::proxy_handler))
        .layer(axum::Extension(ctx))
        .layer(axum::Extension(state))
}
