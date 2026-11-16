//! Requests `proxy_handler` rejects itself, before or after the Data Plane
//! runs, driven through the REST router.
//!
//! Skipped under `--features fips`: `AppHarness` wires its token HTTP client
//! via `HttpClientConfig::for_testing()` (plaintext) against plaintext mocks
//! and does not install the FIPS crypto provider.

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

/// An upstream on the mock with a GET route at each of `paths`.
async fn upstream_with_get_routes(h: &AppHarness, alias: &str, paths: &[&str]) {
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
            .alias(alias)
            .build(),
        )
        .await
        .expect("create upstream");
    for path in paths {
        h.facade()
            .create_route(
                ctx.clone(),
                CreateRouteRequest::builder(
                    upstream.id,
                    MatchRules {
                        http: Some(HttpMatch {
                            methods: vec![HttpMethod::Get],
                            path: (*path).into(),
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
    }
}

/// `problem` is a 400 `invalid_argument` problem with the one violation
/// `field` / `reason`.
fn assert_field_violation(problem: &serde_json::Value, field: &str, reason: &str) {
    assert_eq!(problem["status"], 400, "{problem}");
    assert_eq!(
        problem["type"], "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        "{problem}"
    );
    let violations: Vec<(&str, &str)> = problem["context"]["field_violations"]
        .as_array()
        .unwrap_or_else(|| panic!("no field_violations: {problem}"))
        .iter()
        .map(|v| {
            (
                v["field"].as_str().unwrap_or_default(),
                v["reason"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(violations, [(field, reason)], "{problem}");
}

fn error_source(resp: &oagw::test_support::TestResponse) -> Option<&str> {
    resp.headers()
        .get("x-oagw-error-source")
        .and_then(|v| v.to_str().ok())
}

// #4979: a percent-encoded dot-segment would let an upstream that decodes the
// path escape the route prefix after OAGW matched it.
#[tokio::test]
async fn encoded_dot_segment_returns_400() {
    let h = AppHarness::builder().build().await;
    upstream_with_get_routes(&h, "dots", &["/v1/models"]).await;

    for path in ["v1/models/%2e%2e/%2e%2e/admin", "v1/models/..%2f..%2fadmin"] {
        let resp = h.api_v1().proxy_get("dots", path).expect_status(400).await;
        assert_field_violation(&resp.json(), "path", "INVALID_PROXY_PATH");
        assert_eq!(error_source(&resp), Some(ErrorSource::Gateway.as_str()));
    }
}

// #4979: an encoded `/` cannot skip a narrower route. An upstream that
// decodes `%2F` would serve `/v1/models` under the `/v1` route's policy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encoded_separator_bypassing_a_route_returns_400() {
    let h = AppHarness::builder().build().await;
    upstream_with_get_routes(&h, "separators", &["/v1", "/v1/models"]).await;

    for path in ["v1%2Fmodels", "v1%5Cmodels", "v1%EF%BC%8Fmodels"] {
        let resp = h
            .api_v1()
            .proxy_get("separators", path)
            .expect_status(400)
            .await;
        assert_field_violation(&resp.json(), "path", "INVALID_PROXY_PATH");
        assert_eq!(error_source(&resp), Some(ErrorSource::Gateway.as_str()));
    }
    // Sent decoded, it reaches the narrower route.
    h.api_v1()
        .proxy_get("separators", "v1/models")
        .expect_status(200)
        .await;
}

// #4979: route `/v1/models` does not cover `/v1/modelsx`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn route_prefix_matches_whole_segments() {
    let h = AppHarness::builder().build().await;
    upstream_with_get_routes(&h, "segments", &["/v1/models"]).await;

    h.api_v1()
        .proxy_get("segments", "v1/models")
        .expect_status(200)
        .await;
    let resp = h
        .api_v1()
        .proxy_get("segments", "v1/modelsx")
        .expect_status(404)
        .await;
    assert_eq!(error_source(&resp), Some(ErrorSource::Gateway.as_str()));
}

// #4979: the bare alias (`/oagw/v1/proxy/{alias}`) matches route `/`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_alias_matches_root_route() {
    let h = AppHarness::builder().build().await;
    upstream_with_get_routes(&h, "bare", &["/"]).await;

    let request = http::Request::get("/oagw/v1/proxy/bare")
        .body(axum::body::Body::empty())
        .expect("valid request");
    let resp = h
        .router()
        .clone()
        .oneshot(request)
        .await
        .expect("router call succeeds");
    // The mock has no `/` handler: its 404 comes from the upstream, so the
    // request was routed.
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        resp.headers()
            .get("x-oagw-error-source")
            .and_then(|v| v.to_str().ok()),
        Some(ErrorSource::Upstream.as_str()),
    );
}

// #4981: `proxy:invoke` is checked before the handler reads or validates the
// body, so a caller without it gets 403 and OAGW buffers nothing.
#[tokio::test]
async fn denied_caller_gets_403_before_body_checks() {
    let h = AppHarness::builder()
        .with_authz_client(std::sync::Arc::new(
            oagw::test_support::DenyingAuthZResolverClient,
        ))
        .with_max_body_size(8)
        .build()
        .await;

    // Over the limit by Content-Length: 413 for an allowed caller.
    let resp = h
        .api_v1()
        .proxy_post("any-alias", "v1/test")
        .with_body("longer than eight bytes")
        .with_header(header::CONTENT_LENGTH, HeaderValue::from_static("23"))
        .expect_status(403)
        .await;
    assert_eq!(error_source(&resp), Some(ErrorSource::Gateway.as_str()));

    // A Content-Length that is not a number: 400 for an allowed caller.
    h.api_v1()
        .proxy_post("any-alias", "v1/test")
        .with_body("body")
        .with_header(header::CONTENT_LENGTH, HeaderValue::from_static("four"))
        .expect_status(403)
        .await;

    // An invalid Content-Type: 400 for an allowed caller (data plane check).
    h.api_v1()
        .proxy_post("any-alias", "v1/test")
        .with_body("body")
        .with_header(header::CONTENT_TYPE, HeaderValue::from_static("not a mime"))
        .expect_status(403)
        .await;

    // Over the limit while streaming, with no Content-Length.
    let body =
        axum::body::Body::from_stream(futures_util::stream::iter([Ok::<_, std::io::Error>(
            bytes::Bytes::from_static(b"streamed body longer than eight bytes"),
        )]));
    let request = http::Request::post("/oagw/v1/proxy/any-alias/v1/test")
        .body(body)
        .expect("valid request");
    let resp = h
        .router()
        .clone()
        .oneshot(request)
        .await
        .expect("router call succeeds");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // A WebSocket upgrade with a body header: 400 for an allowed caller.
    let request = http::Request::get("/oagw/v1/proxy/any-alias/v1/ws")
        .header(header::UPGRADE, "websocket")
        .header(header::CONNECTION, "Upgrade")
        .header(header::CONTENT_LENGTH, "0")
        .body(axum::body::Body::empty())
        .expect("valid request");
    let resp = h
        .router()
        .clone()
        .oneshot(request)
        .await
        .expect("router call succeeds");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// #4981: a body stream that fails for a reason other than its size is a
// gateway 400, not a 413.
#[tokio::test]
async fn body_read_error_returns_400_not_413() {
    let h = AppHarness::builder().build().await;
    let body = axum::body::Body::from_stream(futures_util::stream::iter([
        Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"partial ")),
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "client went away",
        )),
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
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.headers()
            .get("x-oagw-error-source")
            .and_then(|v| v.to_str().ok()),
        Some(ErrorSource::Gateway.as_str()),
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem+json body");
    assert_field_violation(&problem, "body", "BODY_READ_FAILED");
}
