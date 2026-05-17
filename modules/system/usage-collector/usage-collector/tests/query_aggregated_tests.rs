#![allow(clippy::unwrap_used, clippy::expect_used)]
//
// Integration tests for `GET /usage-collector/v1/aggregated`.
//
// These tests boot the assembled gateway (router → handler → `Service` →
// `LocalClient` → noop plugin) through `AppHarness`, exercising the
// full wire surface end-to-end against a Postgres-free harness:
//
// - Happy path (200, empty array — noop plugin returns no rows).
// - Validation branches (`inst-agg-3*` / `inst-agg-4a`):
//     - missing `fn` (DTO requires it; serde rejects → 400),
//     - missing `from` / `to` (DTO requires them; serde rejects → 400),
//     - `bucket_size` absent when `group_by` includes `time_bucket`,
//     - `from >= to`,
//     - range exceeds `MAX_QUERY_TIME_RANGE`,
//     - string filter exceeding `MAX_FILTER_STRING_LEN`.
// - 403 PDP-denied path (`inst-agg-6a`, body `{"error":"forbidden"}`).
// - 403 PDP non-Denied fail-closed path (`NetworkError` → 403 + ERROR log
//   line for `inst-authz-3b`).
// - 503 plugin-error path (`inst-agg-8c`, body
//   `{"error":"service_unavailable","correlation_id":"<id>"}`).
// - `ResourceExhausted` propagated via the canonical 503 path
//   (planning decision: no 400/429 shortcut).
//
// Branch-level coverage and OpenAPI registration assertions are owned by
// Phase 5 (`handlers_tests.rs`); this file focuses on the integrated
// wiring concerns of Phase 6.

// @cpt-flow:cpt-cf-usage-collector-flow-query-api-aggregated:p1

mod common;

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use chrono::{Duration as ChronoDuration, Utc};
use http::{Method, Request, StatusCode};
use tower::ServiceExt;
use usage_collector_sdk::UsageCollectorPluginClientV1;

use common::{AppHarness, MockUsageCollectorPluginClientV1, encode_dt};

/// `MAX_QUERY_TIME_RANGE` is 366 days (~1 year); pick 367 to cross the line.
const OVER_MAX_DAYS: i64 = 367;

/// `MAX_FILTER_STRING_LEN` is 256 bytes; pick 257 to cross the line.
const OVER_MAX_FILTER_LEN: usize = 257;

fn agg_uri(from_dt: chrono::DateTime<Utc>, to_dt: chrono::DateTime<Utc>) -> String {
    let from = encode_dt(from_dt);
    let to = encode_dt(to_dt);
    format!("/usage-collector/v1/aggregated?from={from}&to={to}&fn=sum")
}

#[tokio::test]
async fn query_aggregated_happy_path_returns_200_empty_array() {
    let harness = AppHarness::new().await;

    let now = Utc::now();
    let request = Request::builder()
        .method(Method::GET)
        .uri(agg_uri(now - ChronoDuration::hours(1), now))
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        json.is_array() && json.as_array().unwrap().is_empty(),
        "noop plugin must yield an empty JSON array: {json}"
    );
}

#[tokio::test]
async fn query_aggregated_400_when_fn_missing() {
    let harness = AppHarness::new().await;

    let now = Utc::now();
    let from = encode_dt(now - ChronoDuration::hours(1));
    let to = encode_dt(now);
    // No `fn=` parameter: `AggregatedQueryParams::fn_` is required, so serde rejects.
    let uri = format!("/usage-collector/v1/aggregated?from={from}&to={to}");

    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn query_aggregated_400_when_from_missing() {
    let harness = AppHarness::new().await;
    let to = encode_dt(Utc::now());
    let uri = format!("/usage-collector/v1/aggregated?to={to}&fn=sum");

    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn query_aggregated_400_when_to_missing() {
    let harness = AppHarness::new().await;
    let from = encode_dt(Utc::now() - ChronoDuration::hours(1));
    let uri = format!("/usage-collector/v1/aggregated?from={from}&fn=sum");

    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn query_aggregated_400_when_from_ge_to() {
    let harness = AppHarness::new().await;

    let now = Utc::now();
    // from > to; the handler accumulates this into the validation envelope.
    let uri = agg_uri(now, now - ChronoDuration::hours(1));
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3a
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3a

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    // The 400 envelope lives in `context` (RFC 9457 extension), not in a
    // JSON-encoded `detail` string.
    assert_eq!(json["context"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn query_aggregated_400_when_time_range_too_wide() {
    let harness = AppHarness::new().await;

    let to_dt = Utc::now();
    let from_dt = to_dt - ChronoDuration::days(OVER_MAX_DAYS);
    let uri = agg_uri(from_dt, to_dt);

    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3b
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-3b
}

#[tokio::test]
async fn query_aggregated_400_when_time_bucket_without_bucket_size() {
    let harness = AppHarness::new().await;

    let now = Utc::now();
    let from = encode_dt(now - ChronoDuration::hours(1));
    let to = encode_dt(now);
    // `group_by=time_bucket` with no `bucket_size`: the handler accumulates a
    // validation error and returns 400 via `inst-agg-4a`.
    let uri =
        format!("/usage-collector/v1/aggregated?from={from}&to={to}&fn=sum&group_by=time_bucket");

    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn query_aggregated_400_when_filter_string_exceeds_max_len() {
    let harness = AppHarness::new().await;

    let now = Utc::now();
    let from = encode_dt(now - ChronoDuration::hours(1));
    let to = encode_dt(now);
    let oversized = "a".repeat(OVER_MAX_FILTER_LEN);
    let uri =
        format!("/usage-collector/v1/aggregated?from={from}&to={to}&fn=sum&usage_type={oversized}");

    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn query_aggregated_403_when_pdp_denies() {
    let harness = AppHarness::with_deny_authz().await;

    let now = Utc::now();
    let request = Request::builder()
        .method(Method::GET)
        .uri(agg_uri(now - ChronoDuration::hours(1), now))
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-6a
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        json["context"],
        serde_json::json!({ "error": "forbidden" }),
        "403 context must be the exact envelope -- no PDP details leaked"
    );
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-6a
}

#[tokio::test]
#[tracing_test::traced_test]
async fn query_aggregated_403_when_pdp_network_error_fails_closed() {
    use async_trait::async_trait;
    use authz_resolver_sdk::models::{EvaluationRequest, EvaluationResponse};
    use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError};

    /// PDP mock that simulates an infrastructure failure (network down,
    /// engine crash, etc.). The authz layer maps any non-Denied PDP error
    /// to `PermissionDenied` (fail-closed; `inst-authz-3b`).
    struct NetworkErrorAuthZ;

    #[async_trait]
    impl AuthZResolverClient for NetworkErrorAuthZ {
        async fn evaluate(
            &self,
            _request: EvaluationRequest,
        ) -> Result<EvaluationResponse, AuthZResolverError> {
            Err(AuthZResolverError::ServiceUnavailable(
                "PDP unreachable".to_owned(),
            ))
        }
    }

    let plugin: Arc<dyn UsageCollectorPluginClientV1> =
        Arc::new(MockUsageCollectorPluginClientV1::new());
    let harness = AppHarness::with_query_plugin_and_authz(
        plugin,
        Arc::new(NetworkErrorAuthZ) as Arc<dyn AuthZResolverClient>,
    )
    .await;

    let now = Utc::now();
    let request = Request::builder()
        .method(Method::GET)
        .uri(agg_uri(now - ChronoDuration::hours(1), now))
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "PDP infra failure must fail-closed as 403 -- never 503"
    );

    // @cpt-begin:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3b
    assert!(
        logs_contain("PDP infrastructure error"),
        "non-Denied PDP error must be captured at ERROR level"
    );
    // @cpt-end:cpt-cf-usage-collector-algo-query-api-authz-delegate:p1:inst-authz-3b
}

#[tokio::test]
#[tracing_test::traced_test]
async fn query_aggregated_503_when_plugin_unavailable_returns_canonical_problem() {
    let harness = AppHarness::with_unavailable_plugin().await;

    let now = Utc::now();
    let request = Request::builder()
        .method(Method::GET)
        .uri(agg_uri(now - ChronoDuration::hours(1), now))
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-8c
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    // The 503 envelope (`error`, `correlation_id`) lives in `context`
    // (RFC 9457 extension) per `service_unavailable_problem` in `handlers.rs`.
    let context = &json["context"];
    assert_eq!(context["error"], "service_unavailable");
    let correlation_id = context["correlation_id"]
        .as_str()
        .expect("503 body must carry a correlation_id");
    assert!(!correlation_id.is_empty());
    assert!(
        logs_contain(correlation_id),
        "ERROR log must carry the same correlation_id as the 503 body"
    );
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-aggregated:p1:inst-agg-8c
}

#[tokio::test]
async fn query_aggregated_resource_exhausted_routes_through_canonical_503_not_400() {
    // Planning decision: the gateway does NOT special-case `ResourceExhausted`
    // into a 400 / 429 shortcut. The canonical Problem mapper owns the status
    // for any non-PermissionDenied plugin error; for the gateway-side 503
    // envelope used by `service_unavailable_problem` that means
    // `ResourceExhausted` MUST surface as 503 alongside every other non-Denied
    // plugin error variant.
    let harness = AppHarness::with_resource_exhausted_plugin().await;

    let now = Utc::now();
    let request = Request::builder()
        .method(Method::GET)
        .uri(agg_uri(now - ChronoDuration::hours(1), now))
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "ResourceExhausted must propagate as the canonical 503 envelope, not 400/429"
    );

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    // The 503 envelope lives in `context` (RFC 9457 extension).
    assert_eq!(json["context"]["error"], "service_unavailable");
}

// License-gate coverage lives in the in-crate unit tests
// (`handlers_tests.rs::license_gate_literal_matches_feature_0003_dod` pins the
// literal on the `License` marker, and
// `license_gate_wired_for_both_query_routes` asserts both query routes
// register the marker). No analogous integration test exists here because
// `AppHarness` mounts a bare `axum::Router::route` without the license-gate
// middleware, so any in-test assertion would be a tautology over a local
// literal rather than a behavioural check.
