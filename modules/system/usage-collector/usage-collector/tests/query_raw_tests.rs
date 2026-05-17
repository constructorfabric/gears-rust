#![allow(clippy::unwrap_used, clippy::expect_used)]
//
// Integration tests for `GET /usage-collector/v1/raw`.
//
// These tests boot the assembled gateway (router → handler → `Service` →
// `LocalClient` → noop plugin) through `AppHarness`, exercising the full
// raw-query wire surface end-to-end against a Postgres-free harness:
//
// - Happy path (200, empty `Page` from the noop plugin).
// - Multi-page `CursorV1` traversal: stub plugin yields a `Page` with
//   `next_cursor` on page 1 and `next_cursor=None` on page 2; the test
//   round-trips the `CursorV1` (decode → re-request) and asserts the
//   second page completes the traversal with no duplicates or gaps.
// - 400 validation paths (`inst-raw-3*` / `inst-raw-4a`):
//     - missing `from` / `to` (DTO requires them),
//     - `from >= to`,
//     - range exceeds `MAX_QUERY_TIME_RANGE`,
//     - `page_size=0`,
//     - `page_size > MAX_PAGE_SIZE`,
//     - filter string exceeds `MAX_FILTER_STRING_LEN`,
//     - cursor decode failure,
//     - cursor timestamp outside `[from, to]`.
// - Absent `page_size` uses `DEFAULT_PAGE_SIZE` (asserted via a stub plugin
//   that captures the `query.page_size` value it received).
// - 403 PDP denied / non-Denied fail-closed.
// - 503 plugin error (canonical Problem mapper, `inst-raw-8b`).

// @cpt-flow:cpt-cf-usage-collector-flow-query-api-raw:p2

mod common;

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use chrono::{Duration as ChronoDuration, Utc};
use http::{Method, Request, StatusCode};
use modkit_odata::CursorV1;
use tower::ServiceExt;
use usage_collector_sdk::UsageCollectorPluginClientV1;

use common::{
    AppHarness, MockUsageCollectorPluginClientV1, encode_dt, final_page, make_cursor_at,
    page_with_next,
};

const OVER_MAX_DAYS: i64 = 367;
const OVER_MAX_FILTER_LEN: usize = 257;
const OVER_MAX_PAGE_SIZE: u32 = 1_001;
const DEFAULT_PAGE_SIZE_EXPECTED: u32 = 100;

fn raw_uri(from_dt: chrono::DateTime<Utc>, to_dt: chrono::DateTime<Utc>) -> String {
    let from = encode_dt(from_dt);
    let to = encode_dt(to_dt);
    format!("/usage-collector/v1/raw?from={from}&to={to}")
}

#[tokio::test]
async fn query_raw_happy_path_returns_empty_page() {
    let harness = AppHarness::new().await;
    let now = Utc::now();
    let request = Request::builder()
        .method(Method::GET)
        .uri(raw_uri(now - ChronoDuration::hours(1), now))
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        json["items"].as_array().unwrap().is_empty(),
        "noop plugin must yield an empty `items` array"
    );
    assert!(
        json["page_info"]["next_cursor"].is_null()
            || json["page_info"].get("next_cursor").is_none(),
        "empty page must omit / null `next_cursor` -- final page marker per section 6"
    );
}

#[tokio::test]
async fn query_raw_multi_page_cursor_traversal_via_cursorv1() {
    // Two records straddling a `next_cursor` produced by the plugin. The
    // gateway returns the encoded `CursorV1` in `page_info.next_cursor`; the
    // test decodes it, re-requests with `?cursor=<encoded>`, and asserts the
    // second page completes the traversal.

    let now = Utc::now();
    let ts_page1 = now - ChronoDuration::minutes(40);
    let ts_page2 = now - ChronoDuration::minutes(20);
    let from_dt = now - ChronoDuration::hours(1);
    let to_dt = now;

    // Page 1: one record + `next_cursor` whose timestamp lies inside [from, to].
    let page1 = page_with_next(ts_page1, ts_page2, u64::from(DEFAULT_PAGE_SIZE_EXPECTED));
    // Page 2: terminal page (one record, no next_cursor).
    let page2 = final_page(ts_page2, u64::from(DEFAULT_PAGE_SIZE_EXPECTED));

    let plugin: Arc<dyn UsageCollectorPluginClientV1> =
        Arc::new(MockUsageCollectorPluginClientV1::with_raw_pages(vec![
            page1, page2,
        ]));
    let harness = AppHarness::with_query_plugin(plugin).await;

    // ── Request page 1 ────────────────────────────────────────────────────
    let request = Request::builder()
        .method(Method::GET)
        .uri(raw_uri(from_dt, to_dt))
        .body(Body::empty())
        .unwrap();
    let response = harness.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let page1_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let items1 = page1_body["items"].as_array().unwrap();
    assert_eq!(
        items1.len(),
        1,
        "page 1 must carry one record: {page1_body}"
    );
    let cursor_str = page1_body["page_info"]["next_cursor"]
        .as_str()
        .expect("page 1 must carry a non-null next_cursor")
        .to_owned();

    // CursorV1 round-trip: decode the opaque token to confirm it is a valid
    // keyset cursor (no HMAC).
    let _decoded = CursorV1::decode(&cursor_str).expect("next_cursor must decode as CursorV1");

    // ── Request page 2 with the cursor from page 1 ────────────────────────
    let from_q = encode_dt(from_dt);
    let to_q = encode_dt(to_dt);
    // The cursor is base64url-safe (`-_=`); no further encoding required.
    let uri = format!("/usage-collector/v1/raw?from={from_q}&to={to_q}&cursor={cursor_str}");
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let page2_body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let items2 = page2_body["items"].as_array().unwrap();
    assert_eq!(
        items2.len(),
        1,
        "page 2 must carry one record: {page2_body}"
    );
    assert!(
        page2_body["page_info"]["next_cursor"].is_null()
            || page2_body["page_info"].get("next_cursor").is_none(),
        "page 2 must be terminal: next_cursor absent / null"
    );

    // No duplicates / no gaps: assert on the records' identity
    // (`idempotency_key`), not their `timestamp`. Comparing timestamps would
    // be tautological — the test supplies distinct `ts_page1` / `ts_page2` as
    // inputs — and would not catch a regression that returns the same record
    // on both pages with a different timestamp. `make_usage_record` allocates
    // a fresh UUID v4 per record, so a duplicate key across pages can only
    // occur if the gateway re-emits the same record.
    let key1 = items1[0]["idempotency_key"].as_str().unwrap().to_owned();
    let key2 = items2[0]["idempotency_key"].as_str().unwrap().to_owned();
    assert_ne!(
        key1, key2,
        "records across pages must be distinct (no duplicate by idempotency_key)"
    );
}

#[tokio::test]
async fn query_raw_400_when_from_missing() {
    let harness = AppHarness::new().await;
    let to = encode_dt(Utc::now());
    let uri = format!("/usage-collector/v1/raw?to={to}");

    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn query_raw_400_when_to_missing() {
    let harness = AppHarness::new().await;
    let from = encode_dt(Utc::now() - ChronoDuration::hours(1));
    let uri = format!("/usage-collector/v1/raw?from={from}");

    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn query_raw_400_when_from_ge_to() {
    let harness = AppHarness::new().await;
    let now = Utc::now();
    let request = Request::builder()
        .method(Method::GET)
        .uri(raw_uri(now, now - ChronoDuration::hours(1)))
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3a
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3a
}

#[tokio::test]
async fn query_raw_400_when_time_range_too_wide() {
    let harness = AppHarness::new().await;
    let to_dt = Utc::now();
    let from_dt = to_dt - ChronoDuration::days(OVER_MAX_DAYS);
    let request = Request::builder()
        .method(Method::GET)
        .uri(raw_uri(from_dt, to_dt))
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3b
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-3b
}

#[tokio::test]
async fn query_raw_400_when_page_size_zero() {
    let harness = AppHarness::new().await;
    let now = Utc::now();
    let uri = format!(
        "{}&page_size=0",
        raw_uri(now - ChronoDuration::hours(1), now)
    );
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn query_raw_400_when_page_size_above_max() {
    let harness = AppHarness::new().await;
    let now = Utc::now();
    let uri = format!(
        "{}&page_size={OVER_MAX_PAGE_SIZE}",
        raw_uri(now - ChronoDuration::hours(1), now)
    );
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn query_raw_absent_page_size_uses_default_page_size() {
    // Build a plugin that captures the observed `query.page_size` so we can
    // assert the gateway substituted `DEFAULT_PAGE_SIZE` for the absent value.
    let plugin_struct = MockUsageCollectorPluginClientV1::new();
    let captured = plugin_struct.observed_page_size();
    let plugin: Arc<dyn UsageCollectorPluginClientV1> = Arc::new(plugin_struct);
    let harness = AppHarness::with_query_plugin(plugin).await;

    let now = Utc::now();
    let request = Request::builder()
        .method(Method::GET)
        .uri(raw_uri(now - ChronoDuration::hours(1), now))
        .body(Body::empty())
        .unwrap();
    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let observed = captured.lock().unwrap();
    assert_eq!(
        *observed,
        Some(DEFAULT_PAGE_SIZE_EXPECTED),
        "gateway must use DEFAULT_PAGE_SIZE when caller omits page_size"
    );
}

#[tokio::test]
async fn query_raw_400_when_filter_string_exceeds_max_len() {
    let harness = AppHarness::new().await;
    let now = Utc::now();
    let oversized = "a".repeat(OVER_MAX_FILTER_LEN);
    let uri = format!(
        "{}&resource_type={oversized}",
        raw_uri(now - ChronoDuration::hours(1), now)
    );
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn query_raw_400_when_cursor_malformed() {
    let harness = AppHarness::new().await;
    let now = Utc::now();
    let uri = format!(
        "{}&cursor=not-a-valid-cursor",
        raw_uri(now - ChronoDuration::hours(1), now)
    );
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn query_raw_400_when_cursor_outside_time_range() {
    let harness = AppHarness::new().await;
    let now = Utc::now();
    let from_dt = now - ChronoDuration::hours(1);
    let to_dt = now;
    let outside_ts = from_dt - ChronoDuration::days(7);
    let cursor = make_cursor_at(outside_ts)
        .encode()
        .expect("CursorV1 encode infallible");

    let uri = format!("{}&cursor={cursor}", raw_uri(from_dt, to_dt));
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn query_raw_403_when_pdp_denies() {
    let harness = AppHarness::with_deny_authz().await;
    let now = Utc::now();
    let request = Request::builder()
        .method(Method::GET)
        .uri(raw_uri(now - ChronoDuration::hours(1), now))
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-6a
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["context"], serde_json::json!({ "error": "forbidden" }));
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-6a
}

#[tokio::test]
async fn query_raw_403_when_pdp_network_error_fails_closed() {
    use async_trait::async_trait;
    use authz_resolver_sdk::models::{EvaluationRequest, EvaluationResponse};
    use authz_resolver_sdk::{AuthZResolverClient, AuthZResolverError};

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
        .uri(raw_uri(now - ChronoDuration::hours(1), now))
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[tracing_test::traced_test]
async fn query_raw_503_when_plugin_unavailable_returns_canonical_problem() {
    let harness = AppHarness::with_unavailable_plugin().await;
    let now = Utc::now();
    let request = Request::builder()
        .method(Method::GET)
        .uri(raw_uri(now - ChronoDuration::hours(1), now))
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    // @cpt-begin:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-8b
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
    // @cpt-end:cpt-cf-usage-collector-flow-query-api-raw:p2:inst-raw-8b
}

// License-gate coverage lives in the in-crate unit tests (see the matching
// comment in `tests/query_aggregated_tests.rs`).
