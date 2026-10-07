#![allow(clippy::expect_used, clippy::unwrap_used)]
use crate::test_support::*;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn browse_validates_filters_and_query_shape_without_wire_422() {
    let tenant = Uuid::new_v4();
    let (app, _) = rest_app(tenant, super::router).await;
    for query in [
        "kind=unknown",
        "kind=sku&limit=0",
        "kind=sku&limit=oops",
        "kind=sku&$filter=secret%20eq%20'x'",
        "kind=sku&$filter=oops",
        "kind=tax_category&limit=1",
    ] {
        let response = request(
            &app,
            tenant,
            Method::GET,
            &format!("/bss-products/v1/browse?{query}"),
            None,
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
    }
    for field in ["entity_id", "sku_id"] {
        let response = request(
            &app,
            tenant,
            Method::GET,
            &format!(
                "/bss-products/v1/browse?kind=sku&$filter={field}%20eq%20{}",
                Uuid::new_v4()
            ),
            None,
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "{field}");
    }
    for field in ["entity_code", "sku_code", "name"] {
        let response = request(
            &app,
            tenant,
            Method::GET,
            &format!("/bss-products/v1/browse?kind=sku&$filter={field}%20eq%20'none'"),
            None,
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "{field}");
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri("/bss-products/v1/browse?kind=sku")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// RS-41: one browse asks the PDP once, for both kinds; the door and the provider asked it once
/// each.
#[tokio::test]
async fn one_browse_asks_the_pdp_once() {
    let tenant = Uuid::new_v4();
    let (db, _, _, _dsn) = test_db().await;
    let (_app, state) =
        rest_app_on_db(tenant, super::router, resolved_usage_types(), "test", db).await;
    let (enforcer, asked) = counting_flat_in_enforcer(tenant);
    let app = super::router(state, &toolkit::api::OpenApiRegistryImpl::new())
        .layer(axum::Extension(enforcer));
    for query in ["kind=sku", "kind=tax_category"] {
        asked.store(0, std::sync::atomic::Ordering::Relaxed);
        let response = request(
            &app,
            tenant,
            Method::GET,
            &format!("/bss-products/v1/browse?{query}"),
            None,
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "{query}");
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "{query}"
        );
    }
}
