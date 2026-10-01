//! Requests `proxy_handler` rejects itself, before or after the Data Plane
//! runs, driven through the REST router.
//!
//! Skipped under `--features fips`: `AppHarness` wires its token HTTP client
//! via `HttpClientConfig::for_testing()` (plaintext), which `toolkit-http`
//! rejects with `HttpError::InsecureTransport` under FIPS — see PR #1985.

#![cfg(not(feature = "fips"))]

use http::{HeaderValue, Method, StatusCode, header};
use oagw::test_support::{AppHarness, RequestCase};
use oagw_sdk::HTTP_PROTOCOL_ID;
use oagw_sdk::api::ErrorSource;
use oagw_sdk::{
    CreateRouteRequest, CreateUpstreamRequest, Endpoint, HttpMatch, HttpMethod, MatchRules,
    PathSuffixMode, Scheme, Server,
};
use tower::ServiceExt;

/// The headers that make `is_websocket_upgrade` treat a request as one.
fn with_upgrade_headers(case: RequestCase<'_>) -> RequestCase<'_> {
    case.with_header(header::UPGRADE, HeaderValue::from_static("websocket"))
        .with_header(header::CONNECTION, HeaderValue::from_static("Upgrade"))
        .with_header(
            header::SEC_WEBSOCKET_KEY,
            HeaderValue::from_static("dGhlIHNhbXBsZSBub25jZQ=="),
        )
        .with_header(
            header::SEC_WEBSOCKET_VERSION,
            HeaderValue::from_static("13"),
        )
}

// RFC 6455 §4.1: a WebSocket upgrade must be a GET.
#[tokio::test]
async fn websocket_upgrade_with_non_get_method_returns_400() {
    let h = AppHarness::builder().build().await;

    let resp = with_upgrade_headers(h.api_v1().proxy(Method::POST, "any-alias", "ws"))
        .expect_status(400)
        .await;
    assert!(
        resp.text()
            .contains("WebSocket upgrade requires GET method"),
        "unexpected body: {}",
        resp.text()
    );
}

// RFC 6455 §4.1: a WebSocket upgrade carries no body.
#[tokio::test]
async fn websocket_upgrade_with_body_returns_400() {
    let h = AppHarness::builder().build().await;

    let resp = with_upgrade_headers(h.api_v1().proxy_get("any-alias", "ws"))
        .with_header(header::CONTENT_LENGTH, HeaderValue::from_static("4"))
        .expect_status(400)
        .await;
    assert!(
        resp.text()
            .contains("WebSocket upgrade request must not contain a body"),
        "unexpected body: {}",
        resp.text()
    );
}

// `/oagw/v1/proxy//path` names no alias.
#[tokio::test]
async fn proxy_path_without_alias_returns_400() {
    let h = AppHarness::builder().build().await;

    let resp = h.api_v1().proxy_get("", "v1/test").expect_status(400).await;
    assert!(
        resp.text().contains("missing alias in proxy path"),
        "unexpected body: {}",
        resp.text()
    );
}

// A `Content-Length` that is not visible ASCII cannot be read as a length.
#[tokio::test]
async fn non_ascii_content_length_returns_400() {
    let h = AppHarness::builder().build().await;

    let resp = h
        .api_v1()
        .proxy_post("any-alias", "v1/test")
        .with_body("body")
        .with_header(
            header::CONTENT_LENGTH,
            HeaderValue::from_bytes(b"\xff").expect("obs-text is a valid header value"),
        )
        .expect_status(400)
        .await;
    assert!(
        resp.text().contains("invalid Content-Length header"),
        "unexpected body: {}",
        resp.text()
    );
}

// Without a `Content-Length` the limit is enforced while buffering the body.
// A streamed body has no known length, so nothing sets the header for it.
#[tokio::test]
async fn body_over_limit_without_content_length_returns_413() {
    let h = AppHarness::builder().with_max_body_size(8).build().await;
    let body = axum::body::Body::from_stream(futures_util::stream::iter([
        Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"streamed in chunks, ")),
        Ok(bytes::Bytes::from_static(b"longer than eight bytes")),
    ]));
    let request = http::Request::post("/oagw/v1/proxy/any-alias/v1/test")
        .body(body)
        .expect("valid request");

    let resp = h
        .router()
        .clone()
        .oneshot(request)
        .await
        .expect("router call succeeds");
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("request body exceeds maximum of 8 bytes"),
        "unexpected body: {text}"
    );
}

// The upstream accepts the upgrade (the same route answers 101 through the
// facade in `proxy_websocket_upgrade_returns_101`), but the inbound
// connection carries no hyper upgrade handle, since the router is driven with
// `oneshot`. The handler cannot complete the 101 and reports a protocol
// error, which renders as a retryable gateway 502 without its detail.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_upgrade_without_upgradable_connection_returns_502() {
    let h = AppHarness::builder().build().await;
    let ctx = h.security_context().clone();

    let upstream = h
        .facade()
        .create_upstream(
            ctx.clone(),
            CreateUpstreamRequest::builder(
                Server {
                    endpoints: vec![Endpoint {
                        scheme: Scheme::Http,
                        host: "127.0.0.1".into(),
                        port: h.mock_port(),
                    }],
                },
                HTTP_PROTOCOL_ID,
            )
            .alias("ws-no-upgrade")
            .build(),
        )
        .await
        .expect("create upstream");
    h.facade()
        .create_route(
            ctx,
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/ws/echo".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .expect("create route");

    let resp = with_upgrade_headers(h.api_v1().proxy_get("ws-no-upgrade", "ws/echo"))
        .expect_status(502)
        .await;
    assert_eq!(
        resp.headers()
            .get("x-oagw-error-source")
            .and_then(|v| v.to_str().ok()),
        Some(ErrorSource::Gateway.as_str()),
    );
    assert!(
        resp.headers().contains_key(header::RETRY_AFTER),
        "a protocol error is retryable"
    );
}
