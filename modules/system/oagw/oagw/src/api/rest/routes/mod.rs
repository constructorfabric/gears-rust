use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::api::operation_builder::LicenseFeature;

use crate::module::AppState;

mod proxy;
mod route;
mod upstream;

pub(super) struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        "gts.x.core.lic.feat.v1~x.core.oagw.base.v1"
    }
}

impl LicenseFeature for License {}

/// Register all OAGW REST routes with OpenAPI metadata.
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    state: AppState,
) -> Router {
    router = upstream::register(router, openapi);
    router = route::register(router, openapi);
    router = proxy::register(router);
    router.layer(axum::Extension(state))
}
