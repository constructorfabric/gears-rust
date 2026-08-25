//! Wiring test for [`super::register_routes`].
//!
//! [`super::register_api_routes`] is pinned by
//! [`super::openapi_contract_tests`], which deliberately supplies no
//! `Service`. What `register_routes` adds on top is the single
//! `Extension<Arc<Service>>` layer every handler extracts — and no
//! registry-shaped assertion can observe it, because the layer changes
//! nothing about the registered `OperationSpec`s.
//!
//! So this suite dispatches one request through both compositions and
//! pins the difference. With the layer, the handler body runs and answers
//! the plugin's not-found as 404; without it, axum's `Extension`
//! extractor rejects with 500 before any handler body executes. Deleting
//! `.layer(axum::Extension(service))` collapses the first case onto the
//! second and fails loudly.
//!
//! The `Extension<SecurityContext>` the handlers extract *first* is
//! gateway-supplied in production, so both routers under test get it from
//! the test. The status difference therefore turns on the service
//! extension alone.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use toolkit_gts::gts_id;
use tower::ServiceExt as _;
use usage_collector_sdk::UsageTypeGtsId;

use crate::domain::Service;
use crate::domain::test_support::{HappyPathPlugin, authenticated_ctx, service_with_permit};

const SAMPLE_USAGE_TYPE_ID: &str =
    gts_id!("cf.core.uc.usage_record.v1~cf.mini_chat._.tokens_consumed.v1");

/// `GET /usage-collector/v1/usage-types/{gts_id}` through `router`.
async fn get_sample_usage_type(router: Router) -> StatusCode {
    let request = Request::get(format!(
        "/usage-collector/v1/usage-types/{SAMPLE_USAGE_TYPE_ID}"
    ))
    .body(Body::empty())
    .expect("request builds");

    router
        .layer(axum::Extension(authenticated_ctx()))
        .oneshot(request)
        .await
        .expect("Router's error type is Infallible")
        .status()
}

/// A `Service` whose storage plugin reports the sample usage type as
/// absent, so a handler that actually runs answers 404 — a status the
/// missing-extension path cannot produce.
fn service_reporting_not_found() -> Arc<Service> {
    let plugin = HappyPathPlugin::new();
    plugin.set_get_usage_type_not_found(
        UsageTypeGtsId::new(SAMPLE_USAGE_TYPE_ID).expect("valid usage-type gts_id"),
    );
    service_with_permit(
        plugin as Arc<dyn usage_collector_sdk::UsageCollectorPluginV1>,
        "test.routes.service_layer.happy.v1",
    )
}

#[tokio::test]
async fn register_routes_attaches_the_service_extension() {
    let status = get_sample_usage_type(super::register_routes(
        Router::new(),
        &OpenApiRegistryImpl::new(),
        service_reporting_not_found(),
    ))
    .await;

    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "with Extension<Arc<Service>> layered on, the handler must run and \
         surface the plugin's not-found as 404",
    );
}

#[tokio::test]
async fn the_same_routes_without_the_service_extension_cannot_serve_a_request() {
    // Counterpart to the test above, and the reason the contract suite can
    // assert nothing about the layer: same route set, same request, no
    // service.
    let status = get_sample_usage_type(super::register_api_routes(
        Router::new(),
        &OpenApiRegistryImpl::new(),
    ))
    .await;

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "without the service layer, axum's Extension extractor must reject \
         the request before the handler body runs",
    );
}
