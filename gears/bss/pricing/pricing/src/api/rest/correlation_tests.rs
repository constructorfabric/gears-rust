//! The edge itself: what it establishes, what it leaves alone, and what it does
//! **not** do when it was never applied.
//!
//! The behaviour that matters most about this module is negative —
//! [`require_correlation`] refusing to mint — so it is asserted directly here
//! rather than only through a route. A test that merely drove a mounted route and
//! found a non-NULL correlation would pass just as well against the per-handler
//! mint D-178 clause (2) forbids; that property is
//! `tests/rest_plans.rs::two_records_of_one_patch_carry_one_correlation_id`, and
//! it needs a database.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use tower::ServiceExt as _;

use super::{CorrelationId, establish, require_correlation};

/// A route that reports whatever correlation reached it, or 500.
fn app() -> Router {
    Router::new()
        .route(
            "/probe",
            get(
                |extension: Option<axum::extract::Extension<CorrelationId>>| async move {
                    require_correlation(extension).map_or_else(
                        |_| (StatusCode::INTERNAL_SERVER_ERROR, String::new()),
                        |value| (StatusCode::OK, value.to_string()),
                    )
                },
            ),
        )
        .layer(axum::middleware::from_fn(establish))
}

async fn probe(router: Router) -> (StatusCode, String) {
    let response = router
        .oneshot(
            Request::builder()
                .uri("/probe")
                .body(Body::empty())
                .expect("a request"),
        )
        .await
        .expect("the router answers");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("a body");
    (status, String::from_utf8(body.to_vec()).expect("utf-8"))
}

#[tokio::test]
async fn a_request_carrying_nothing_is_given_a_correlation() {
    // D-178 clause (1)'s fallback: minted at the edge when the platform supplies
    // none, so the field is always satisfiable.
    let (status, body) = probe(app()).await;
    assert_eq!(status, StatusCode::OK);
    let minted: uuid::Uuid = body.parse().expect("a uuid");
    assert!(!minted.is_nil(), "a minted correlation is a real value");
}

#[tokio::test]
async fn two_requests_are_given_two_correlations() {
    // The other half of "request-scoped": one value **per request**, not one per
    // process. Without this a constant would satisfy the test above.
    let (_, first) = probe(app()).await;
    let (_, second) = probe(app()).await;
    assert_ne!(first, second);
}

#[tokio::test]
async fn a_correlation_already_on_the_request_is_kept() {
    // The propagation half, exercised through the extension the platform would
    // populate. It is also what makes the layer safe to compose twice: a second
    // application must not give one request a second identity.
    let carried = uuid::Uuid::from_u128(0xc0_11_ab);
    let router = app().layer(axum::Extension(CorrelationId(carried)));
    let (status, body) = probe(router).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, carried.to_string());
}

#[tokio::test]
async fn a_route_mounted_without_the_edge_is_a_fault_and_not_a_fresh_mint() {
    // The guard. Minting here would answer 200 while giving every record of one
    // call a different correlation - the exact defect the layer exists to
    // prevent, and invisible to any test that only asserts "not NULL".
    let router = Router::new().route(
        "/probe",
        get(
            |extension: Option<axum::extract::Extension<CorrelationId>>| async move {
                require_correlation(extension).map_or_else(
                    |_| (StatusCode::INTERNAL_SERVER_ERROR, String::new()),
                    |value| (StatusCode::OK, value.to_string()),
                )
            },
        ),
    );
    let (status, _) = probe(router).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}
