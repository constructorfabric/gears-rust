use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::LicenseFeature;
use toolkit_gts::gts_id;

use crate::gear::AppState;

mod oauth;
mod proxy;
mod route;
mod upstream;

const OAGW_BASE_LICENSE_FEATURE: &str = gts_id!("cf.core.lic.feat.v1~cf.core.oagw.base.v1");

pub(super) struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        OAGW_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// Register all OAGW REST routes with OpenAPI metadata.
///
/// When `state.config.management_api_enabled` is `false`, only read-only
/// endpoints (list / get) and the proxy catch-all are registered. Write
/// operations (create / update / delete) are omitted.
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    state: AppState,
) -> Router {
    let writable = state.config.management_api_enabled;
    router = upstream::register(router, openapi, writable);
    router = route::register(router, openapi, writable);
    router = oauth::register(router, openapi);
    router = proxy::register(router);
    router.layer(axum::Extension(state))
}

/// Create a test router with all OAGW routes registered.
///
/// Uses manual route registration without OpenAPI metadata.
/// Suitable for integration tests that don't need an `OpenApiRegistry`.
///
/// Wires the canonical error middleware
/// (`toolkit::api::canonical_error_middleware`) so integration tests
/// observe the same `instance` / `trace_id` injection production traffic
/// gets through api-gateway's middleware stack.
#[cfg(any(test, feature = "test-utils"))]
pub fn test_router(state: AppState, ctx: toolkit_security::SecurityContext) -> Router {
    use crate::api::rest::handlers::{
        oauth as oauth_h, proxy as proxy_h, route as route_h, upstream as upstream_h,
    };
    use axum::routing::{any, get, post};

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
        // Interactive OAuth authorization management
        .route(
            "/oagw/v1/upstreams/{id}/oauth",
            get(oauth_h::connection_status).delete(oauth_h::revoke_authorization),
        )
        .route(
            "/oagw/v1/upstreams/{id}/oauth/authorize",
            post(oauth_h::begin_authorization),
        )
        .route("/oagw/v1/oauth/callback", get(oauth_h::oauth_callback))
        // Proxy
        .route("/oagw/v1/proxy/{*path}", any(proxy_h::proxy_handler))
        .layer(axum::middleware::from_fn(
            toolkit::api::canonical_error_middleware,
        ))
        .layer(axum::Extension(ctx))
        .layer(axum::Extension(state))
}
