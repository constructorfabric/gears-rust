//! Skipped under `--features fips`: `AppHarness` wires its token HTTP client
//! via `HttpClientConfig::for_testing()` (plaintext) against plaintext mocks
//! and does not install the FIPS crypto provider.

#![cfg(not(feature = "fips"))]

use std::collections::HashMap;
use toolkit_gts::{GTS_ID_URI_PREFIX, gts_id};

use http::{Method, StatusCode};
use oagw::test_support::{
    APIKEY_AUTH_PLUGIN_ID, AppHarness, MockBody, MockGuard, MockResponse, MockStreamEnd,
    MockUpstream, OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID,
};
use oagw_sdk::api::ErrorSource;
use oagw_sdk::{Body, HTTP_PROTOCOL_ID};
use oagw_sdk::{
    BurstConfig, CorsConfig, CorsHttpMethod, CreateRouteRequest, CreateUpstreamRequest, Endpoint,
    HeadersConfig, HttpMatch, HttpMethod, MatchRules, PassthroughMode, PathSuffixMode,
    PluginBinding, PluginsConfig, RateLimitAlgorithm, RateLimitConfig, RateLimitScope,
    RateLimitStrategy, RequestHeaderRules, ResponseHeaderRules, Scheme, Server, SharingMode,
    SustainedRate, Window,
};
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

async fn setup_openai_mock() -> AppHarness {
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-test123".into())])
        .build()
        .await;

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": HTTP_PROTOCOL_ID,
            "alias": "mock-upstream",
            "enabled": true,
            "tags": [],
            "auth": {
                "type": APIKEY_AUTH_PLUGIN_ID,
                "sharing": "private",
                "config": {
                    "header": "authorization",
                    "prefix": "Bearer ",
                    "secret_ref": "cred://openai-key"
                }
            }
        }))
        .expect_status(201)
        .await;
    let upstream_id = resp.json()["id"].as_str().unwrap().to_string();

    for (methods, path) in [
        (vec!["POST", "GET"], "/v1/chat/completions"),
        (vec!["GET"], "/error"),
    ] {
        h.api_v1()
            .post_route()
            .with_body(serde_json::json!({
                "upstream_id": &upstream_id,
                "match": {
                    "http": {
                        "methods": methods,
                        "path": path
                    }
                },
                "enabled": true,
                "tags": [],
                "priority": 0
            }))
            .expect_status(201)
            .await;
    }

    h
}

// 6.13: Full pipeline — proxy POST /v1/chat/completions with JSON body.
#[tokio::test]
async fn proxy_chat_completion_round_trip() {
    let h = setup_openai_mock().await;

    let req = http::Request::builder()
        .method(Method::POST)
        .uri("/mock-upstream/v1/chat/completions")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4","messages":[{"role":"user","content":"Hello"}]}"#,
        ))
        .unwrap();
    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response.into_body().into_bytes().await.unwrap();
    let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert!(body_json.get("id").is_some());
    assert!(body_json.get("choices").is_some());
}

// 6.13 (auth): Verify the mock received the Authorization header.
/// SREF-REG-001: API-key auth in `config` mode (no `credential_mode`) is
/// unchanged by per-request secret references.
#[tokio::test]
async fn proxy_injects_auth_header() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/v1/chat/completions",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({
                "id": "chatcmpl-auth-test",
                "object": "chat.completion",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}]
            })),
        },
    );

    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-test123".into())])
        .build()
        .await;
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
            .alias("auth-hdr-test")
            .auth(oagw_sdk::AuthConfig {
                plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
                sharing: SharingMode::Private,
                config: Some(
                    [
                        ("header".into(), "authorization".into()),
                        ("prefix".into(), "Bearer ".into()),
                        ("secret_ref".into(), "cred://openai-key".into()),
                    ]
                    .into_iter()
                    .collect(),
                ),
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/v1/chat/completions"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/auth-hdr-test{}",
            guard.path("/v1/chat/completions")
        ))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"gpt-4","messages":[{"role":"user","content":"Hello"}]}"#,
        ))
        .unwrap();
    let response = h.facade().proxy_request(ctx, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let recorded = guard.recorded_requests().await;
    assert_eq!(recorded.len(), 1);
    let auth_header = recorded[0]
        .headers
        .iter()
        .find(|(k, _)| k == "authorization")
        .map(|(_, v)| v.as_str())
        .expect("authorization header missing");
    assert_eq!(auth_header, "Bearer sk-test123");
}

// 7.2: with an auth plugin, a header the client repeats reaches the upstream
// once per line, and the client's own line of the plugin's header is
// replaced (#4982).
#[tokio::test]
async fn proxy_auth_plugin_keeps_repeated_headers() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/v1/multi",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-test123".into())])
        .build()
        .await;
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
            .alias("auth-multi-test")
            .headers(HeadersConfig {
                request: Some(RequestHeaderRules {
                    passthrough: PassthroughMode::All,
                    ..Default::default()
                }),
                response: None,
            })
            .auth(oagw_sdk::AuthConfig {
                plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
                sharing: SharingMode::Private,
                config: Some(
                    [
                        ("header".into(), "authorization".into()),
                        ("prefix".into(), "Bearer ".into()),
                        ("secret_ref".into(), "cred://openai-key".into()),
                    ]
                    .into_iter()
                    .collect(),
                ),
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: guard.path("/v1/multi"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/auth-multi-test{}", guard.path("/v1/multi")))
        .header("x-multi", "a")
        .header("x-multi", "b")
        .header(http::header::AUTHORIZATION, "Bearer client-token")
        .body(Body::Empty)
        .unwrap();
    let response = h.facade().proxy_request(ctx, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let recorded = guard.recorded_requests().await;
    assert_eq!(recorded.len(), 1);
    let values = |name: &str| -> Vec<&str> {
        recorded[0]
            .headers
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    };
    assert_eq!(values("x-multi"), ["a", "b"]);
    assert_eq!(values("authorization"), ["Bearer sk-test123"]);
}

/// An upstream on the mock with apikey auth reading `cred://openai-key`,
/// passthrough of all request headers and the request_id transform bound,
/// and a `GET` route on `path`.
async fn setup_auth_and_transform(h: &AppHarness, guard: &MockGuard, alias: &str, path: &str) {
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
            .headers(HeadersConfig {
                request: Some(RequestHeaderRules {
                    passthrough: PassthroughMode::All,
                    ..Default::default()
                }),
                response: None,
            })
            .auth(oagw_sdk::AuthConfig {
                plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
                sharing: SharingMode::Private,
                config: Some(
                    [
                        ("header".into(), "authorization".into()),
                        ("prefix".into(), "Bearer ".into()),
                        ("secret_ref".into(), "cred://openai-key".into()),
                    ]
                    .into_iter()
                    .collect(),
                ),
            })
            .plugins(PluginsConfig {
                sharing: SharingMode::Private,
                items: vec![PluginBinding {
                    plugin_ref: REQUEST_ID_TRANSFORM_PLUGIN_ID.to_string(),
                    config: Default::default(),
                }],
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx,
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: guard.path(path),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();
}

// 7.2: a request transform applies only its own change. The credential the
// auth plugin set (an obs-text value here), repeated client headers and a
// non-UTF-8 client value reach the upstream as before the transform.
#[tokio::test]
async fn proxy_transform_keeps_auth_and_client_headers() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/v1/transform-multi",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-café".into())])
        .build()
        .await;
    setup_auth_and_transform(&h, &guard, "auth-transform-test", "/v1/transform-multi").await;

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/auth-transform-test{}",
            guard.path("/v1/transform-multi")
        ))
        .header("x-multi", "a")
        .header("x-multi", "b")
        .header("x-raw", http::HeaderValue::from_bytes(b"caf\xe9").unwrap())
        .header(http::header::AUTHORIZATION, "Bearer client-token")
        .body(Body::Empty)
        .unwrap();
    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let recorded = guard.recorded_requests().await;
    assert_eq!(recorded.len(), 1);
    let values = |name: &str| -> Vec<&str> {
        recorded[0]
            .headers
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    };
    assert_eq!(values("authorization"), ["Bearer sk-café"]);
    assert_eq!(values("x-multi"), ["a", "b"]);
    // The mock records values lossily: 0xE9 alone is not UTF-8.
    assert_eq!(values("x-raw"), ["caf\u{FFFD}"]);
    let request_id = values("x-request-id");
    assert_eq!(request_id.len(), 1, "{request_id:?}");
    assert!(
        uuid::Uuid::parse_str(request_id[0]).is_ok(),
        "{request_id:?}"
    );
}

// 7.2: with a transform bound, repeated and obs-text response headers reach
// the client as the upstream sent them.
#[tokio::test]
async fn proxy_transform_keeps_response_headers() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/v1/transform-resp",
        MockResponse {
            status: 200,
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("x-multi".into(), "a".into()),
                ("x-multi".into(), "b".into()),
                ("x-raw".into(), "café".into()),
            ],
            body: MockBody::Json(json!({"ok": true})),
        },
    );
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-test123".into())])
        .build()
        .await;
    setup_auth_and_transform(&h, &guard, "transform-resp-test", "/v1/transform-resp").await;

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!(
            "/transform-resp-test{}",
            guard.path("/v1/transform-resp")
        ))
        .body(Body::Empty)
        .unwrap();
    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let multi: Vec<_> = response.headers().get_all("x-multi").iter().collect();
    assert_eq!(multi, ["a", "b"]);
    assert_eq!(
        response.headers().get("x-raw").map(|v| v.as_bytes()),
        Some("café".as_bytes())
    );
}

// 6.14: SSE streaming — proxy to dynamic SSE mock via MockGuard.
#[tokio::test]
async fn proxy_sse_streaming() {
    let mut guard = MockGuard::new();

    let chunks: Vec<String> = vec![
        json!({"id":"chatcmpl-mock-stream","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}).to_string(),
        json!({"id":"chatcmpl-mock-stream","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}).to_string(),
        "[DONE]".to_string(),
    ];
    guard.mock(
        "POST",
        "/v1/chat/completions/stream",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            body: MockBody::Sse(chunks),
        },
    );

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
            .alias("sse-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/v1/chat/completions/stream"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/sse-test{}",
            guard.path("/v1/chat/completions/stream")
        ))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"model":"gpt-4","stream":true}"#))
        .unwrap();
    let response = h.facade().proxy_request(ctx, req).await.unwrap();

    let ct = response
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("text/event-stream"), "got content-type: {ct}");

    let body_bytes = response.into_body().into_bytes().await.unwrap();
    let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert!(body_str.contains("data: [DONE]"));
}

// 13.3: the Data Plane subscribes its SSE bodies to the signal the gear stops
// streams with. Stopped first, so the mock's whole-body SSE answer cannot
// end the stream before the stop does.
#[tokio::test]
async fn proxy_sse_stream_ends_when_streams_were_stopped() {
    use futures_util::StreamExt as _;

    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/v1/chat/completions/stream",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            body: MockBody::Sse(vec!["[DONE]".to_string()]),
        },
    );

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
            .alias("sse-stopped")
            .build(),
        )
        .await
        .unwrap();
    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/v1/chat/completions/stream"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    h.end_streams();
    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/sse-stopped{}",
            guard.path("/v1/chat/completions/stream")
        ))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"stream":true}"#))
        .unwrap();
    let response = h.facade().proxy_request(ctx, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_stream();
    match body.next().await {
        Some(Err(e)) => assert!(
            e.downcast_ref::<oagw_sdk::StreamClosedByGateway>()
                .is_some(),
            "not the shutdown error: {e}"
        ),
        other => panic!("expected the shutdown error, got {other:?}"),
    }
    assert!(body.next().await.is_none(), "the stream goes on");
}

/// An upstream on the shared mock with one route, `method path` with no
/// path suffix.
async fn setup_mock_route(h: &AppHarness, alias: &str, method: HttpMethod, path: String) {
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
        .unwrap();
    h.facade()
        .create_route(
            ctx,
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![method],
                        path,
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();
}

fn sse_stream_mock(chunks: &[&str], end: MockStreamEnd) -> MockResponse {
    MockResponse {
        status: 200,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        body: MockBody::Stream {
            chunks: chunks.iter().map(|c| (*c).to_owned()).collect(),
            end,
        },
    }
}

/// Sends `POST path` over HTTP/1.1 to a server started with
/// `start_oagw_server`.
async fn http1_post(
    addr: std::net::SocketAddr,
    path: &str,
) -> http::Response<hyper::body::Incoming> {
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tcp))
            .await
            .unwrap();
    tokio::spawn(conn);
    let req = http::Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(http::header::HOST, addr.to_string())
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(http_body_util::Full::new(bytes::Bytes::from_static(
            br#"{"stream":true}"#,
        )))
        .unwrap();
    sender.send_request(req).await.unwrap()
}

/// Reads `body` until it ends: the data received, and how it ended.
async fn read_http_body(body: &mut hyper::body::Incoming) -> (Vec<u8>, Result<(), hyper::Error>) {
    use http_body_util::BodyExt as _;
    let mut data = Vec::new();
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), body.frame())
            .await
            .expect("the body neither ends nor fails");
        match frame {
            None => return (data, Ok(())),
            Some(Err(e)) => return (data, Err(e)),
            Some(Ok(frame)) => {
                if let Ok(chunk) = frame.into_data() {
                    data.extend_from_slice(&chunk);
                }
            }
        }
    }
}

// 13.3: a stop does not cut a non-SSE body: every upstream response is
// streamed, and the HTTP drain lets it finish.
#[tokio::test]
async fn proxy_non_sse_body_is_not_cut_when_streams_are_stopped() {
    use futures_util::StreamExt as _;

    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/download",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/octet-stream".into())],
            body: MockBody::Stream {
                chunks: vec!["part-1".into()],
                end: MockStreamEnd::Hold,
            },
        },
    );
    let h = AppHarness::builder().build().await;
    setup_mock_route(
        &h,
        "download-stop",
        HttpMethod::Get,
        guard.path("/download"),
    )
    .await;

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/download-stop{}", guard.path("/download")))
        .body(Body::Empty)
        .unwrap();
    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_stream();
    assert_eq!(body.next().await.unwrap().unwrap(), "part-1");

    h.end_streams();
    let next = tokio::time::timeout(std::time::Duration::from_millis(500), body.next()).await;
    assert!(next.is_err(), "the stop ended the body: {next:?}");
}

// 13.4: an SSE stream the upstream leaves silent past the streaming idle
// timeout ends with `StreamingError::IdleTimeout` for SDK readers, not as a
// complete stream.
#[tokio::test]
async fn proxy_sse_idle_timeout_ends_the_stream_with_an_error() {
    use futures_util::StreamExt as _;
    use oagw_sdk::{ServerEvent, ServerEventsResponse, ServerEventsStream, StreamingError};

    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/sse/hold",
        sse_stream_mock(&["data: held\n\n"], MockStreamEnd::Hold),
    );
    let h = AppHarness::builder()
        .with_streaming_idle_timeout(std::time::Duration::from_millis(300))
        .build()
        .await;
    setup_mock_route(
        &h,
        "sse-idle-sdk",
        HttpMethod::Post,
        guard.path("/sse/hold"),
    )
    .await;

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/sse-idle-sdk{}", guard.path("/sse/hold")))
        .body(Body::from(r#"{"stream":true}"#))
        .unwrap();
    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    let ServerEventsResponse::Events(mut events) =
        ServerEventsStream::from_response::<ServerEvent>(response)
    else {
        panic!("not an SSE response");
    };
    assert_eq!(events.next().await.unwrap().unwrap().data, "held");
    let next = tokio::time::timeout(std::time::Duration::from_secs(5), events.next())
        .await
        .expect("the idle timeout ends the stream");
    assert!(
        matches!(next, Some(Err(StreamingError::IdleTimeout))),
        "expected IdleTimeout, got {next:?}"
    );
    assert!(events.next().await.is_none());
}

// 13.4 over HTTP: the idle cut aborts the chunked body (no terminating
// chunk) after the events already sent; nothing is written after them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_sse_idle_timeout_aborts_the_http_body() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/sse/hold",
        sse_stream_mock(&["data: held\n\n"], MockStreamEnd::Hold),
    );
    let h = AppHarness::builder()
        .with_streaming_idle_timeout(std::time::Duration::from_secs(1))
        .build()
        .await;
    setup_mock_route(
        &h,
        "sse-idle-http",
        HttpMethod::Post,
        guard.path("/sse/hold"),
    )
    .await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    // Before the request: the idle timer starts before the head is sent.
    let started = std::time::Instant::now();
    let response = http1_post(
        addr,
        &format!("/oagw/v1/proxy/sse-idle-http{}", guard.path("/sse/hold")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut body = response.into_body();
    let (data, end) = read_http_body(&mut body).await;
    assert_eq!(data, b"data: held\n\n");
    assert!(end.is_err(), "the idle cut passed for a complete stream");
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(1),
        "cut before the idle timeout: {:?}",
        started.elapsed()
    );

    server_handle.abort();
}

// 13.5: the upstream fails mid-stream. The caller gets the events sent before
// the failure and then a broken body: no JSON error is injected after the
// head, and the stream does not end cleanly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_sse_upstream_failure_mid_stream_aborts_the_http_body() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/sse/fail",
        sse_stream_mock(&["data: one\n\n", "data: two\n\n"], MockStreamEnd::Abort),
    );
    let h = AppHarness::builder().build().await;
    setup_mock_route(
        &h,
        "sse-fail-http",
        HttpMethod::Post,
        guard.path("/sse/fail"),
    )
    .await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    let response = http1_post(
        addr,
        &format!("/oagw/v1/proxy/sse-fail-http{}", guard.path("/sse/fail")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.headers()["x-oagw-error-source"], "upstream");
    let mut body = response.into_body();
    let (data, end) = read_http_body(&mut body).await;
    assert_eq!(data, b"data: one\n\ndata: two\n\n");
    assert!(end.is_err(), "the broken stream passed for a complete one");

    server_handle.abort();
}

/// A raw HTTP/1.1 upstream that answers each request with a chunked
/// `content_type` head and one chunk in a single write, then closes (FIN)
/// without the body's terminating chunk.
async fn start_upstream_that_dies(content_type: &'static str) -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match tcp.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\n\
                     transfer-encoding: chunked\r\n\r\nb\r\ndata: one\n\n\r\n"
                );
                let _ = tcp.write_all(response.as_bytes()).await;
                let _ = tcp.shutdown().await;
            });
        }
    });
    addr
}

/// The chunk and the upstream's EOF arrive together. The caller still gets
/// the head and the chunk before the body breaks, every time.
async fn assert_chunk_sent_just_before_the_upstream_dies_arrives(
    alias: &str,
    content_type: &'static str,
) {
    let upstream_addr = start_upstream_that_dies(content_type).await;
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
                        port: upstream_addr.port(),
                    }],
                },
                HTTP_PROTOCOL_ID,
            )
            .alias(alias)
            .build(),
        )
        .await
        .unwrap();
    h.facade()
        .create_route(
            ctx,
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: "/dies".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();
    let (addr, server_handle) = start_oagw_server(&h).await;

    for _ in 0..20 {
        let response = http1_post(addr, &format!("/oagw/v1/proxy/{alias}/dies")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], content_type);
        let mut body = response.into_body();
        let (data, end) = read_http_body(&mut body).await;
        assert_eq!(data, b"data: one\n\n");
        assert!(end.is_err(), "the broken body passed for a complete one");
    }

    server_handle.abort();
}

// 13.5: the event and the upstream's EOF arrive together. The caller still
// gets the event before the body breaks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_sse_event_sent_just_before_the_upstream_dies_reaches_the_caller() {
    assert_chunk_sent_just_before_the_upstream_dies_arrives("sse-dies", "text/event-stream").await;
}

// The same for a non-SSE body: the chunk is not lost with the body error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_chunk_sent_just_before_the_upstream_dies_reaches_the_caller() {
    assert_chunk_sent_just_before_the_upstream_dies_arrives(
        "download-dies",
        "application/octet-stream",
    )
    .await;
}

// 13.3 over HTTP: an SSE request answered after the stop still gets its head,
// then a broken body, not a dropped connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_sse_stream_cut_before_its_first_event_keeps_its_head() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/sse/hold",
        sse_stream_mock(&["data: held\n\n"], MockStreamEnd::Hold),
    );
    let h = AppHarness::builder().build().await;
    setup_mock_route(
        &h,
        "sse-stopped-http",
        HttpMethod::Post,
        guard.path("/sse/hold"),
    )
    .await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    h.end_streams();
    let response = http1_post(
        addr,
        &format!("/oagw/v1/proxy/sse-stopped-http{}", guard.path("/sse/hold")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut body = response.into_body();
    let (data, end) = read_http_body(&mut body).await;
    assert!(data.is_empty(), "data after the stop: {data:?}");
    assert!(end.is_err(), "the cut stream passed for a complete one");

    server_handle.abort();
}

// 13.5 in process: an SDK reader gets the events, then a stream error.
#[tokio::test]
async fn proxy_sse_upstream_failure_mid_stream_is_a_stream_error() {
    use futures_util::StreamExt as _;
    use oagw_sdk::{ServerEvent, ServerEventsResponse, ServerEventsStream, StreamingError};

    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/sse/fail",
        sse_stream_mock(&["data: one\n\n"], MockStreamEnd::Abort),
    );
    let h = AppHarness::builder().build().await;
    setup_mock_route(
        &h,
        "sse-fail-sdk",
        HttpMethod::Post,
        guard.path("/sse/fail"),
    )
    .await;

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/sse-fail-sdk{}", guard.path("/sse/fail")))
        .body(Body::from(r#"{"stream":true}"#))
        .unwrap();
    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    let ServerEventsResponse::Events(mut events) =
        ServerEventsStream::from_response::<ServerEvent>(response)
    else {
        panic!("not an SSE response");
    };
    assert_eq!(events.next().await.unwrap().unwrap().data, "one");
    let next = tokio::time::timeout(std::time::Duration::from_secs(5), events.next())
        .await
        .expect("the upstream failure ends the stream");
    assert!(
        matches!(next, Some(Err(StreamingError::Stream(_)))),
        "expected a stream error, got {next:?}"
    );
    assert!(events.next().await.is_none());
}

// 6.15: Upstream 500 error passthrough.
#[tokio::test]
async fn proxy_upstream_500_passthrough() {
    let h = setup_openai_mock().await;

    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/mock-upstream/error/500")
        .body(Body::Empty)
        .unwrap();
    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        response.extensions().get::<ErrorSource>().copied(),
        Some(ErrorSource::Upstream)
    );
}

// 6.17: Pipeline abort — nonexistent alias returns 404 without calling mock.
#[tokio::test]
async fn proxy_nonexistent_alias_returns_404() {
    let h = setup_openai_mock().await;

    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/nonexistent/v1/test")
        .body(Body::Empty)
        .unwrap();
    match h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
    {
        Err(err) => assert!(matches!(err, CanonicalError::NotFound { .. })),
        Ok(_) => panic!("expected error"),
    }
}

// 6.5: Known alias, no matching route: 404 without calling the upstream.
#[tokio::test]
async fn proxy_known_alias_without_matching_route_returns_404() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/v1/chat",
        MockResponse {
            status: 200,
            headers: vec![],
            body: MockBody::Json(json!({})),
        },
    );
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
            .alias("no-route-test")
            .build(),
        )
        .await
        .unwrap();
    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/v1/chat"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Wrong method on the route's path, then the right method on another path.
    for (method, path) in [
        (Method::GET, guard.path("/v1/chat")),
        (Method::POST, guard.path("/v2/other")),
    ] {
        let req = http::Request::builder()
            .method(method.clone())
            .uri(format!("/no-route-test{path}"))
            .body(Body::Empty)
            .unwrap();
        match h.facade().proxy_request(ctx.clone(), req).await {
            Err(err) => assert!(
                matches!(err, CanonicalError::NotFound { .. })
                    && err.resource_type() == Some(oagw_sdk::gts::ROUTE_SCHEMA),
                "{method} {path}: expected a route 404, got {err:?}"
            ),
            Ok(resp) => panic!("{method} {path}: expected 404, got {}", resp.status()),
        }
    }
    assert!(guard.recorded_requests().await.is_empty());
}

// 6.17: Pipeline abort — disabled upstream returns 503.
#[tokio::test]
async fn proxy_disabled_upstream_returns_503() {
    let h = setup_openai_mock().await;
    let ctx = h.security_context().clone();

    let _upstream = h
        .facade()
        .create_upstream(
            ctx.clone(),
            CreateUpstreamRequest::builder(
                Server {
                    endpoints: vec![Endpoint {
                        scheme: Scheme::Http,
                        host: "127.0.0.1".into(),
                        port: 9999,
                    }],
                },
                HTTP_PROTOCOL_ID,
            )
            .alias("disabled-upstream")
            .enabled(false)
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/disabled-upstream/test")
        .body(Body::Empty)
        .unwrap();
    match h.facade().proxy_request(ctx.clone(), req).await {
        Err(err) => assert!(matches!(err, CanonicalError::ServiceUnavailable { .. })),
        Ok(_) => panic!("expected error"),
    }
}

// 6.17: Pipeline abort — rate limit exceeded returns 429.
#[tokio::test]
async fn proxy_rate_limit_exceeded_returns_429() {
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
            .alias("rate-limited")
            .rate_limit(RateLimitConfig {
                sharing: SharingMode::Private,
                algorithm: RateLimitAlgorithm::TokenBucket,
                sustained: SustainedRate {
                    rate: 1,
                    window: Window::Minute,
                },
                burst: Some(BurstConfig { capacity: 1 }),
                scope: RateLimitScope::Tenant,
                strategy: RateLimitStrategy::Reject,
                cost: 1,
                response_headers: true,
                budget: None,
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/models".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // First request should succeed.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/rate-limited/v1/models")
        .body(Body::Empty)
        .unwrap();
    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Second request should be rate limited.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/rate-limited/v1/models")
        .body(Body::Empty)
        .unwrap();
    match h.facade().proxy_request(ctx.clone(), req).await {
        Err(err) => assert!(matches!(err, CanonicalError::ResourceExhausted { .. })),
        Ok(_) => panic!("expected rate limit error"),
    }
}

// 18.3: Rate limit scope=user — different subjects within the same tenant get
// separate buckets. Proves scope-aware keying works end-to-end.
// Cross-tenant isolation is deferred to e2e tests requiring multi-tenant harness.
#[tokio::test]
async fn proxy_rate_limit_scope_user_isolates_by_subject() {
    let h = AppHarness::builder().build().await;
    let ctx = h.security_context().clone();
    let tenant_id = ctx.subject_tenant_id();

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
            .alias("scope-user")
            .rate_limit(RateLimitConfig {
                sharing: SharingMode::Private,
                algorithm: RateLimitAlgorithm::TokenBucket,
                sustained: SustainedRate {
                    rate: 1,
                    window: Window::Minute,
                },
                burst: Some(BurstConfig { capacity: 1 }),
                scope: RateLimitScope::User,
                strategy: RateLimitStrategy::Reject,
                cost: 1,
                response_headers: true,
                budget: None,
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/models".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Two users in the same tenant.
    let user_a = SecurityContext::builder()
        .subject_tenant_id(tenant_id)
        .subject_id(uuid::Uuid::new_v4())
        .build()
        .unwrap();
    let user_b = SecurityContext::builder()
        .subject_tenant_id(tenant_id)
        .subject_id(uuid::Uuid::new_v4())
        .build()
        .unwrap();

    // User A — allowed (own bucket).
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/scope-user/v1/models")
        .body(Body::Empty)
        .unwrap();
    let resp = h.facade().proxy_request(user_a.clone(), req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // User B — allowed (separate bucket).
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/scope-user/v1/models")
        .body(Body::Empty)
        .unwrap();
    let resp = h.facade().proxy_request(user_b.clone(), req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // User A again — rejected (own bucket exhausted).
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/scope-user/v1/models")
        .body(Body::Empty)
        .unwrap();
    match h.facade().proxy_request(user_a, req).await {
        Err(err) => assert!(
            matches!(err, CanonicalError::ResourceExhausted { .. }),
            "expected RateLimitExceeded, got: {err:?}"
        ),
        Ok(resp) => panic!("expected rate limit error, got status {}", resp.status()),
    }

    // User B again — rejected (own bucket exhausted).
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/scope-user/v1/models")
        .body(Body::Empty)
        .unwrap();
    match h.facade().proxy_request(user_b, req).await {
        Err(err) => assert!(
            matches!(err, CanonicalError::ResourceExhausted { .. }),
            "expected RateLimitExceeded, got: {err:?}"
        ),
        Ok(resp) => panic!("expected rate limit error, got status {}", resp.status()),
    }
}

// 18.4: Rate limit scope=route — different routes on the same upstream get
// separate buckets. Proves route-scoped keying isolates per route.
#[tokio::test]
async fn proxy_rate_limit_scope_route_isolates_by_route() {
    // Register a dynamic mock for the second route path.
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/v1/embeddings",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"object": "list", "data": []})),
        },
    );

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
            .alias("scope-route")
            .build(),
        )
        .await
        .unwrap();

    // Route A — rate limited with scope=Route (uses static /v1/models mock).
    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/models".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .rate_limit(RateLimitConfig {
                sharing: SharingMode::Private,
                algorithm: RateLimitAlgorithm::TokenBucket,
                sustained: SustainedRate {
                    rate: 1,
                    window: Window::Minute,
                },
                burst: Some(BurstConfig { capacity: 1 }),
                scope: RateLimitScope::Route,
                strategy: RateLimitStrategy::Reject,
                cost: 1,
                response_headers: true,
                budget: None,
            })
            .build(),
        )
        .await
        .unwrap();

    // Route B — same upstream, different path, same rate limit config
    // (uses dynamic mock registered via MockGuard).
    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: guard.path("/v1/embeddings"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .rate_limit(RateLimitConfig {
                sharing: SharingMode::Private,
                algorithm: RateLimitAlgorithm::TokenBucket,
                sustained: SustainedRate {
                    rate: 1,
                    window: Window::Minute,
                },
                burst: Some(BurstConfig { capacity: 1 }),
                scope: RateLimitScope::Route,
                strategy: RateLimitStrategy::Reject,
                cost: 1,
                response_headers: true,
                budget: None,
            })
            .build(),
        )
        .await
        .unwrap();

    let route_b_path = format!("/scope-route{}", guard.path("/v1/embeddings"));

    // Route A — allowed (own bucket).
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/scope-route/v1/models")
        .body(Body::Empty)
        .unwrap();
    let resp = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Route B — allowed (separate bucket).
    let req = http::Request::builder()
        .method(Method::GET)
        .uri(&route_b_path)
        .body(Body::Empty)
        .unwrap();
    let resp = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Route A again — rejected (own bucket exhausted).
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/scope-route/v1/models")
        .body(Body::Empty)
        .unwrap();
    match h.facade().proxy_request(ctx.clone(), req).await {
        Err(err) => assert!(
            matches!(err, CanonicalError::ResourceExhausted { .. }),
            "expected RateLimitExceeded, got: {err:?}"
        ),
        Ok(resp) => panic!("expected rate limit error, got status {}", resp.status()),
    }

    // Route B again — rejected (own bucket exhausted).
    let req = http::Request::builder()
        .method(Method::GET)
        .uri(&route_b_path)
        .body(Body::Empty)
        .unwrap();
    match h.facade().proxy_request(ctx, req).await {
        Err(err) => assert!(
            matches!(err, CanonicalError::ResourceExhausted { .. }),
            "expected RateLimitExceeded, got: {err:?}"
        ),
        Ok(resp) => panic!("expected rate limit error, got status {}", resp.status()),
    }
}

// 6.16: Upstream timeout — proxy to gated mock that never responds, assert 504.
// Uses multi_thread runtime so the timer driver runs on a dedicated thread,
// preventing stalls when other test binaries compete for CPU.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_upstream_timeout_returns_504() {
    let mut guard = MockGuard::new();
    // Register a gated route that will never respond (sender kept alive but not signaled).
    let _gate = guard.mock_gated(
        "GET",
        "/timeout",
        MockResponse {
            status: 200,
            headers: vec![],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

    let h = AppHarness::builder()
        .with_request_timeout(std::time::Duration::from_millis(500))
        .build()
        .await;
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
            .alias("timeout-upstream")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: guard.path("/timeout"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/timeout-upstream{}", guard.path("/timeout")))
        .body(Body::Empty)
        .unwrap();
    match h.facade().proxy_request(ctx.clone(), req).await {
        Err(err) => assert!(matches!(err, CanonicalError::DeadlineExceeded { .. })),
        Ok(_) => panic!("expected timeout error"),
    }
}

// 8.9: Query allowlist enforcement.
#[tokio::test]
async fn proxy_query_allowlist_allowed_param_succeeds() {
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
            .alias("ql-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/models".into(),
                        query_allowlist: vec!["version".into()],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/ql-test/v1/models?version=2")
        .body(Body::Empty)
        .unwrap();
    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn proxy_query_allowlist_unknown_param_rejected() {
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
            .alias("ql-reject")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/models".into(),
                        query_allowlist: vec!["version".into()],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/ql-reject/v1/models?version=2&debug=true")
        .body(Body::Empty)
        .unwrap();
    match h.facade().proxy_request(ctx.clone(), req).await {
        Err(err) => assert!(matches!(err, CanonicalError::InvalidArgument { .. })),
        Ok(_) => panic!("expected validation error"),
    }
}

/// Assert `err` is a 400 field violation with `reason`.
fn assert_invalid_argument(err: CanonicalError, field: &str, reason: &str) {
    match err {
        CanonicalError::InvalidArgument { ctx, .. } => match ctx {
            toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations } => {
                assert!(
                    field_violations
                        .iter()
                        .any(|v| v.field == field && v.reason == reason),
                    "{field_violations:?}"
                );
            }
            other => panic!("expected FieldViolations, got {other:?}"),
        },
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

// 13.5: an auth plugin OAGW does not implement is refused on write (#4974),
// not left to fail every proxied request.
#[tokio::test]
async fn create_with_unknown_auth_plugin_is_rejected() {
    let h = AppHarness::builder().build().await;
    let err = h
        .facade()
        .create_upstream(
            h.security_context().clone(),
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
            .alias("bad-auth")
            .auth(oagw_sdk::AuthConfig {
                plugin_type: gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.nonexistent.v1")
                    .into(),
                sharing: SharingMode::Private,
                config: None,
            })
            .build(),
        )
        .await
        .expect_err("an unknown auth plugin must be refused");
    assert_invalid_argument(err, "auth.type", "NOT_IMPLEMENTED");
}

// 13.6: Assert on recorded_requests() URI and body content.
#[tokio::test]
async fn proxy_recorded_request_has_correct_uri_and_body() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/v1/chat/completions",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({
                "id": "chatcmpl-rec-test",
                "object": "chat.completion",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}]
            })),
        },
    );

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
            .alias("rec-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/v1/chat/completions"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let body_payload = r#"{"model":"gpt-4","messages":[{"role":"user","content":"Hello"}]}"#;
    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/rec-test{}", guard.path("/v1/chat/completions")))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(body_payload))
        .unwrap();
    let response = h.facade().proxy_request(ctx, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let recorded = guard.recorded_requests().await;
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].uri.ends_with("/v1/chat/completions"));
    assert_eq!(recorded[0].method, "POST");

    let body_str = String::from_utf8(recorded[0].body.clone()).unwrap();
    assert!(body_str.contains("gpt-4"));
    assert!(body_str.contains("Hello"));
}

// A route path ending in `/` matches a request that continues its last
// segment; the upstream gets the path the client sent, without a `/` before
// the suffix.
#[tokio::test]
async fn proxy_trailing_slash_route_forwards_segment_suffix_unchanged() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/v1/admin.json",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

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
            .alias("trailing-slash")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: guard.path("/v1/admin/"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/trailing-slash{}", guard.path("/v1/admin.json")))
        .body(Body::Empty)
        .unwrap();
    let response = h.facade().proxy_request(ctx, req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let recorded = guard.recorded_requests().await;
    assert_eq!(recorded.len(), 1);
    assert!(
        recorded[0].uri.ends_with("/v1/admin.json"),
        "forwarded {}",
        recorded[0].uri
    );
}

// Response header sanitization: hop-by-hop and x-oagw-* headers stripped from upstream response.
#[tokio::test]
async fn proxy_response_headers_sanitized() {
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
            .alias("resp-hdr-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/response-headers".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/resp-hdr-test/response-headers")
        .body(Body::Empty)
        .unwrap();
    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let resp_headers = response.headers();

    // Safe headers should be preserved.
    assert_eq!(
        resp_headers.get("x-custom-safe").unwrap(),
        "keep-me",
        "safe custom header should be forwarded"
    );
    assert!(
        resp_headers.get("content-type").is_some(),
        "content-type should be preserved"
    );

    // Hop-by-hop headers should be stripped.
    assert!(
        resp_headers.get("proxy-authenticate").is_none(),
        "proxy-authenticate should be stripped from response"
    );
    assert!(
        resp_headers.get("trailer").is_none(),
        "trailer should be stripped from response"
    );
    assert!(
        resp_headers.get("upgrade").is_none(),
        "upgrade should be stripped from response"
    );

    // Internal x-oagw-* headers should be stripped.
    assert!(
        resp_headers.get("x-oagw-debug").is_none(),
        "x-oagw-debug should be stripped from response"
    );
    assert!(
        resp_headers.get("x-oagw-trace-id").is_none(),
        "x-oagw-trace-id should be stripped from response"
    );
}

// 8.10: path_suffix_mode=disabled rejects suffix; append succeeds.
#[tokio::test]
async fn proxy_path_suffix_disabled_rejects_extra_path() {
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
            .alias("psm-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/models".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Exact path succeeds.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/psm-test/v1/models")
        .body(Body::Empty)
        .unwrap();
    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Extra suffix rejected with 400.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/psm-test/v1/models/gpt-4")
        .body(Body::Empty)
        .unwrap();
    match h.facade().proxy_request(ctx.clone(), req).await {
        Err(err) => assert!(matches!(err, CanonicalError::InvalidArgument { .. })),
        Ok(_) => panic!("expected validation error for disabled path_suffix_mode"),
    }
}

// ---------------------------------------------------------------------------
// Multi-endpoint load balancing integration tests
// ---------------------------------------------------------------------------

// positive-2.1 (custom-header-routing), positive-2.10 (upstreams): Round-robin distribution across 2 endpoints.
//
// Uses a single mock on 127.0.0.1 with two identical endpoint entries so the
// test works on all platforms (macOS only has 127.0.0.1 on loopback by default).
// Actual round-robin distribution across distinct backends is covered by unit
// tests in endpoint_selector_tests.rs (`select_round_robin_distribution`) and service.rs
// (`select_endpoint_round_robin_fallback`).  This integration test verifies the
// full proxy pipeline succeeds with a multi-endpoint upstream configuration.
#[tokio::test]
async fn proxy_multi_endpoint_round_robin() {
    let mock = MockUpstream::start().await;
    let port = mock.addr().port();

    let h = AppHarness::builder().build().await;
    let ctx = h.security_context().clone();

    let upstream = h
        .facade()
        .create_upstream(
            ctx.clone(),
            CreateUpstreamRequest::builder(
                Server {
                    endpoints: vec![
                        Endpoint {
                            scheme: Scheme::Http,
                            host: "127.0.0.1".into(),
                            port,
                        },
                        Endpoint {
                            scheme: Scheme::Http,
                            host: "127.0.0.1".into(),
                            port,
                        },
                    ],
                },
                HTTP_PROTOCOL_ID,
            )
            .alias("rr-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/models".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Send 4 requests — all reach the single mock via the multi-endpoint pool.
    for _ in 0..4 {
        let req = http::Request::builder()
            .method(Method::GET)
            .uri("/rr-test/v1/models")
            .body(Body::Empty)
            .unwrap();
        let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    assert_eq!(
        mock.recorded_requests().await.len(),
        4,
        "mock should have received all 4 requests"
    );
}

// positive-2.2 (custom-header-routing): X-OAGW-Target-Host explicit selection.
//
// Uses a single mock on 127.0.0.1 with two identical endpoint entries so the
// test works on all platforms (macOS only has 127.0.0.1 on loopback by default).
// This test verifies the header is accepted without error; actual host-based
// routing is covered by the unit test `select_endpoint_target_host_matches` in
// service.rs.
#[tokio::test]
async fn proxy_target_host_header_selects_endpoint() {
    let mock = MockUpstream::start().await;
    let port = mock.addr().port();

    let h = AppHarness::builder().build().await;
    let ctx = h.security_context().clone();

    let upstream = h
        .facade()
        .create_upstream(
            ctx.clone(),
            CreateUpstreamRequest::builder(
                Server {
                    endpoints: vec![
                        Endpoint {
                            scheme: Scheme::Http,
                            host: "127.0.0.1".into(),
                            port,
                        },
                        Endpoint {
                            scheme: Scheme::Http,
                            host: "127.0.0.1".into(),
                            port,
                        },
                    ],
                },
                HTTP_PROTOCOL_ID,
            )
            .alias("target-host-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/models".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Send request with X-OAGW-Target-Host header selecting endpoint host.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/target-host-test/v1/models")
        .header("x-oagw-target-host", "127.0.0.1")
        .body(Body::Empty)
        .unwrap();
    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// negative-2.1 (custom-header-routing): X-OAGW-Target-Host validation — unknown host returns error.
#[tokio::test]
async fn proxy_target_host_unknown_returns_error() {
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
            .alias("target-host-unknown")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/models".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/target-host-unknown/v1/models")
        .header("x-oagw-target-host", "unknown.com")
        .body(Body::Empty)
        .unwrap();

    match h.facade().proxy_request(ctx.clone(), req).await {
        Err(err) => assert!(
            matches!(err, CanonicalError::InvalidArgument { .. }),
            "expected InvalidTargetHost (Unknown), got: {err:?}"
        ),
        Ok(_) => panic!("expected error for unknown target host"),
    }
}

// negative-2.10 (upstreams): All backends unreachable returns connection error (503).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_all_backends_unreachable() {
    let h = AppHarness::builder()
        .with_request_timeout(std::time::Duration::from_secs(5))
        .build()
        .await;
    let ctx = h.security_context().clone();

    // Ports 19991/19992 are unlikely to be listening.
    let upstream = h
        .facade()
        .create_upstream(
            ctx.clone(),
            CreateUpstreamRequest::builder(
                Server {
                    endpoints: vec![Endpoint {
                        scheme: Scheme::Http,
                        host: "127.0.0.1".into(),
                        port: 19991,
                    }],
                },
                HTTP_PROTOCOL_ID,
            )
            .alias("unreachable-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/models".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/unreachable-test/v1/models")
        .body(Body::Empty)
        .unwrap();

    match h.facade().proxy_request(ctx.clone(), req).await {
        Err(err) => assert!(
            matches!(err, CanonicalError::ServiceUnavailable { .. }),
            "expected DownstreamError for unreachable backend, got: {err:?}"
        ),
        Ok(resp) => {
            // A gateway-generated error response. An unreachable backend maps to 502 (DownstreamError
            // — connection refused/closed), 503 (LinkUnavailable — no route),
            // or 504 (connect timeout), depending on how the connection fails.
            assert!(
                resp.status() == StatusCode::BAD_GATEWAY
                    || resp.status() == StatusCode::SERVICE_UNAVAILABLE
                    || resp.status() == StatusCode::GATEWAY_TIMEOUT,
                "expected 502, 503, or 504, got: {}",
                resp.status()
            );
        }
    }
}

// positive-2.13 (upstreams): CRUD invalidation — update upstream endpoints, verify new endpoint used.
#[tokio::test]
async fn proxy_crud_invalidation_after_update() {
    let mock_a = MockUpstream::start().await;
    let mock_b = MockUpstream::start().await;
    let port_a = mock_a.addr().port();
    let port_b = mock_b.addr().port();

    let h = AppHarness::builder().build().await;
    let ctx = h.security_context().clone();

    // Create upstream pointing to mock_a only.
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": port_a, "scheme": "http"}]
            },
            "protocol": HTTP_PROTOCOL_ID,
            "alias": "crud-invalidation",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let upstream_id = resp.json()["id"].as_str().unwrap().to_string();

    h.api_v1()
        .post_route()
        .with_body(json!({
            "upstream_id": &upstream_id,
            "match": {
                "http": {
                    "methods": ["GET"],
                    "path": "/v1/models"
                }
            },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    // Proxy to mock_a.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/crud-invalidation/v1/models")
        .body(Body::Empty)
        .unwrap();
    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        mock_a.recorded_requests().await.len(),
        1,
        "mock_a should have received 1 request"
    );
    assert_eq!(
        mock_b.recorded_requests().await.len(),
        0,
        "mock_b should have received 0 requests"
    );

    // Update upstream to point to mock_b via REST API (triggers invalidation).
    h.api_v1()
        .put_upstream(&upstream_id)
        .with_body(json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": port_b, "scheme": "http"}]
            },
            "protocol": HTTP_PROTOCOL_ID,
            "alias": "crud-invalidation",
            "enabled": true,
            "tags": []
        }))
        .expect_status(200)
        .await;

    // Proxy again — should now go to mock_b (cache was invalidated).
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/crud-invalidation/v1/models")
        .body(Body::Empty)
        .unwrap();
    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        !mock_b.recorded_requests().await.is_empty(),
        "mock_b should have received at least 1 request after update"
    );
}

// Demonstrate MockGuard pattern for custom per-test responses
#[tokio::test]
async fn proxy_with_mock_guard_custom_response() {
    // Create a MockGuard for test-isolated mock responses
    let mut guard = MockGuard::new();

    // Register a custom response at a unique path
    guard.mock(
        "POST",
        "/custom/endpoint",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({
                "custom": "response",
                "test": "data"
            })),
        },
    );

    let h = AppHarness::builder().build().await;
    let ctx = h.security_context().clone();

    // Create upstream pointing to mock server
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
            .alias("guard-test")
            .build(),
        )
        .await
        .unwrap();

    // Create route using the guard's prefixed path
    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/custom/endpoint"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Make request to the prefixed path
    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/guard-test{}", guard.path("/custom/endpoint")))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"test":"input"}"#))
        .unwrap();

    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response.into_body().into_bytes().await.unwrap();
    let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body_json["custom"], "response");
    assert_eq!(body_json["test"], "data");

    // Verify request was recorded (filtered by guard prefix)
    let recorded = guard.recorded_requests().await;
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].uri.contains("/custom/endpoint"));
}

// WebSocket upgrade request returns 101 Switching Protocols when upstream accepts.
/// SREF-REG-001: an upstream with no auth is unchanged by per-request
/// secret references.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_websocket_upgrade_returns_101() {
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
            .alias("ws-echo-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
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
        .unwrap();

    // Send a WebSocket upgrade request through the data plane.
    // The mock server has a ws_echo handler at /ws/echo.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/ws-echo-test/ws/echo")
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .body(Body::Empty)
        .unwrap();

    let resp = h
        .facade()
        .proxy_request(ctx.clone(), req)
        .await
        .expect("WebSocket upgrade should succeed");

    assert_eq!(
        resp.status(),
        http::StatusCode::SWITCHING_PROTOCOLS,
        "expected 101, got {}",
        resp.status()
    );
    assert_eq!(
        resp.headers().get("upgrade").and_then(|v| v.to_str().ok()),
        Some("websocket"),
        "response should contain Upgrade: websocket header"
    );
}

// WebSocket upgrade rejected by upstream returns 502 ProtocolError with gateway error source.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_websocket_upgrade_rejected_returns_502_protocol_error() {
    let h = AppHarness::builder().build().await;
    let ctx = h.security_context().clone();

    // Point at /v1/models — a plain GET endpoint, not a WebSocket handler.
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
            .alias("ws-reject-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/models".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Send a WebSocket upgrade request to the non-WebSocket upstream.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/ws-reject-test/v1/models")
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .body(Body::Empty)
        .unwrap();

    let err = h
        .facade()
        .proxy_request(ctx, req)
        .await
        .expect_err("expected ProtocolError for rejected WebSocket upgrade");

    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "expected ProtocolError, got {err:?}"
    );
}

// WebSocket E2E: upgrade through real TCP, send text frame, receive echo.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_websocket_e2e_echo() {
    use tokio::io::AsyncWriteExt;

    let h = AppHarness::builder().build().await;
    setup_ws_upstream(&h, "ws-e2e").await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    ws_handshake(&mut stream, "/oagw/v1/proxy/ws-e2e/ws/echo").await;

    // Send a masked text frame containing "hello" and read the echo.
    let frame = build_masked_frame(0x1, b"hello");
    stream.write_all(&frame).await.unwrap();
    let (opcode, payload) = read_ws_frame(&mut stream).await.expect("echo frame");
    assert_eq!(opcode, 0x1, "expected text frame opcode");
    assert_eq!(payload, b"hello", "echoed payload mismatch");

    // Send a close frame with code 1000 (Normal Closure).
    let close_frame = build_masked_frame(0x8, &1000u16.to_be_bytes());
    stream.write_all(&close_frame).await.unwrap();
    if let Some((op, _)) = read_ws_frame(&mut stream).await {
        assert_eq!(op, 0x8, "expected close frame opcode");
    }

    server_handle.abort();
}

/// A TLS WebSocket echo upstream on `/ws/echo`. It offers `h2` in ALPN but
/// serves HTTP/1.1 only, so a gateway that offered `h2` for the upgrade
/// fails the connection. Records the HTTP version of each upgrade request.
async fn start_wss_echo() -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::Mutex<Vec<http::Version>>>,
) {
    use axum::extract::WebSocketUpgrade;
    use axum::extract::ws::Message;

    let mut tls = oagw::test_support::test_server_config();
    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(tls));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let versions = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let seen = std::sync::Arc::clone(&versions);
    let app = axum::Router::new().route(
        "/ws/echo",
        axum::routing::get(move |version: http::Version, ws: WebSocketUpgrade| {
            seen.lock().unwrap().push(version);
            async move {
                ws.on_upgrade(|mut socket| async move {
                    while let Some(Ok(msg)) = socket.recv().await {
                        if matches!(msg, Message::Close(_)) || socket.send(msg).await.is_err() {
                            break;
                        }
                    }
                })
            }
        }),
    );
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                continue;
            };
            let acceptor = acceptor.clone();
            let app = app.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let _ = hyper_util::server::conn::auto::Builder::new(
                    hyper_util::rt::TokioExecutor::new(),
                )
                .http1_only()
                .serve_connection_with_upgrades(
                    hyper_util::rt::TokioIo::new(tls),
                    hyper_util::service::TowerToHyperService::new(app),
                )
                .await;
            });
        }
    });
    (addr, versions)
}

// 14.1 over TLS: a `wss` upstream gets the upgrade as HTTP/1.1 inside TLS,
// and frames go both ways through the relay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_websocket_round_trip_over_tls() {
    use tokio::io::AsyncWriteExt;

    let (upstream_addr, versions) = start_wss_echo().await;
    let h = AppHarness::builder()
        .with_skip_upstream_tls_verify(true)
        .build()
        .await;
    let ctx = h.security_context().clone();
    let upstream = h
        .facade()
        .create_upstream(
            ctx.clone(),
            CreateUpstreamRequest::builder(
                Server {
                    endpoints: vec![Endpoint {
                        scheme: Scheme::Wss,
                        host: "127.0.0.1".into(),
                        port: upstream_addr.port(),
                    }],
                },
                HTTP_PROTOCOL_ID,
            )
            .alias("wss-echo")
            .build(),
        )
        .await
        .unwrap();
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
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();
    let (addr, server_handle) = start_oagw_server(&h).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    ws_handshake(&mut stream, "/oagw/v1/proxy/wss-echo/ws/echo").await;
    for payload in [b"over tls".as_slice(), b"second".as_slice()] {
        stream
            .write_all(&build_masked_frame(0x1, payload))
            .await
            .unwrap();
        let (opcode, echoed) = read_ws_frame(&mut stream).await.expect("echo frame");
        assert_eq!(opcode, 0x1, "expected text frame opcode");
        assert_eq!(echoed, payload);
    }
    let seen = versions.lock().unwrap().clone();
    assert_eq!(seen, [http::Version::HTTP_11]);

    server_handle.abort();
}

// 14.2: Auth credentials are injected into the outbound WebSocket upgrade request.
#[tokio::test]
async fn proxy_websocket_auth_injected_during_handshake() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/ws/echo",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

    let h = AppHarness::builder()
        .with_credentials(vec![("cred://ws-key".into(), "sk-ws-secret".into())])
        .build()
        .await;
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
            .alias("ws-auth-test")
            .auth(oagw_sdk::AuthConfig {
                plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
                sharing: SharingMode::Private,
                config: Some(
                    [
                        ("header".into(), "authorization".into()),
                        ("prefix".into(), "Bearer ".into()),
                        ("secret_ref".into(), "cred://ws-key".into()),
                    ]
                    .into_iter()
                    .collect(),
                ),
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: guard.path("/ws/echo"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/ws-auth-test{}", guard.path("/ws/echo")))
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .body(Body::Empty)
        .unwrap();

    // The MockGuard returns 200 (it can't do a real WebSocket handshake),
    // so the proxy correctly returns ProtocolError. The upgrade request was
    // already sent to upstream before the response is checked, so
    // guard.recorded_requests() still captures the outbound headers we need.
    let err = h
        .facade()
        .proxy_request(ctx, req)
        .await
        .expect_err("mock returns 200, so upgrade should fail with ProtocolError");
    assert!(
        matches!(err, CanonicalError::ServiceUnavailable { .. }),
        "expected ProtocolError, got {err:?}"
    );

    let recorded = guard.recorded_requests().await;
    assert_eq!(recorded.len(), 1, "expected exactly 1 recorded request");

    // Verify the auth plugin injected credentials into the upgrade request.
    let auth_header = recorded[0]
        .headers
        .iter()
        .find(|(k, _)| k == "authorization")
        .map(|(_, v)| v.as_str())
        .expect("authorization header missing from upgrade request");
    assert_eq!(auth_header, "Bearer sk-ws-secret");

    // Verify WebSocket-specific headers were forwarded (not stripped by auth pipeline).
    let has_upgrade = recorded[0]
        .headers
        .iter()
        .any(|(k, v)| k == "upgrade" && v == "websocket");
    assert!(has_upgrade, "Upgrade header missing from outbound request");

    let has_ws_key = recorded[0]
        .headers
        .iter()
        .any(|(k, _)| k == "sec-websocket-key");
    assert!(
        has_ws_key,
        "Sec-WebSocket-Key header missing from outbound request"
    );
}

// 14.3: Rate limiting applies to WebSocket connection establishment.
#[tokio::test]
async fn proxy_websocket_rate_limit_on_handshake() {
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
            .alias("ws-rate-limit")
            .rate_limit(RateLimitConfig {
                sharing: SharingMode::Private,
                algorithm: RateLimitAlgorithm::TokenBucket,
                sustained: SustainedRate {
                    rate: 1,
                    window: Window::Minute,
                },
                burst: Some(BurstConfig { capacity: 1 }),
                scope: RateLimitScope::Tenant,
                strategy: RateLimitStrategy::Reject,
                cost: 1,
                response_headers: true,
                budget: None,
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
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
        .unwrap();

    // First WebSocket upgrade should succeed.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/ws-rate-limit/ws/echo")
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .body(Body::Empty)
        .unwrap();
    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::SWITCHING_PROTOCOLS,
        "first WS upgrade should succeed with 101"
    );

    // Second WebSocket upgrade within the rate window should be rejected.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/ws-rate-limit/ws/echo")
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .body(Body::Empty)
        .unwrap();
    match h.facade().proxy_request(ctx, req).await {
        Err(err) => assert!(
            matches!(err, CanonicalError::ResourceExhausted { .. }),
            "expected RateLimitExceeded, got: {err:?}"
        ),
        Ok(resp) => panic!(
            "expected rate limit error on second WS upgrade, got status {}",
            resp.status()
        ),
    }
}

// 14.4: WebSocket idle timeout — gateway closes the connection after inactivity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_websocket_idle_timeout_closes_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // 1000ms instead of 200ms: the readiness probe (Ping→Pong roundtrip
    // through proxy→upstream→proxy) can exceed 200ms under CI resource
    // pressure, causing the idle timer to fire before the probe completes.
    let h = AppHarness::builder()
        .with_websocket_idle_timeout(std::time::Duration::from_secs(1))
        .build()
        .await;
    setup_ws_upstream(&h, "ws-idle-timeout").await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    ws_handshake(&mut stream, "/oagw/v1/proxy/ws-idle-timeout/ws/echo").await;

    // Send a text frame to prove the connection is alive.
    let frame = build_masked_frame(0x1, b"ping");
    stream.write_all(&frame).await.unwrap();
    let (opcode, _) = read_ws_frame(&mut stream).await.expect("echo frame");
    assert_eq!(opcode, 0x1, "expected text frame opcode");

    // Wait longer than the idle timeout (1s).
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Close 1001 "Going Away", then the gateway half-closes: a bare EOF
    // would be a dropped connection, not an idle close.
    let mut close = [0u8; 14];
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_exact(&mut close),
    )
    .await
    .expect("the gateway sends a Close frame on idle")
    .expect("a Close frame, not EOF");
    let mut expected = vec![0x88, 0x0c, 0x03, 0xe9];
    expected.extend_from_slice(b"Going Away");
    assert_eq!(close.as_slice(), expected.as_slice());
    let mut rest = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut rest),
    )
    .await
    .expect("the gateway closes the connection after the Close frame")
    .unwrap();
    assert!(rest.is_empty(), "data after the Close frame: {rest:?}");

    server_handle.abort();
}

// 13.3: the Data Plane subscribes its WebSocket relays to the signal the gear
// stops streams with. The default idle timeout is kept: it would send the same
// Close 1001 and hide a broken wiring.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_websocket_closes_when_streams_were_stopped() {
    use tokio::io::AsyncReadExt;

    let h = AppHarness::builder().build().await;
    setup_ws_upstream(&h, "ws-stopped").await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    h.end_streams();
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    ws_send_upgrade_request(
        &mut stream,
        &default_ws_upgrade_request("/oagw/v1/proxy/ws-stopped/ws/echo"),
    )
    .await;

    let mut close = [0u8; 14];
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_exact(&mut close),
    )
    .await
    .expect("the gateway closes the relay at once")
    .unwrap();
    let mut expected = vec![0x88, 0x0c, 0x03, 0xe9];
    expected.extend_from_slice(b"Going Away");
    assert_eq!(close.as_slice(), expected.as_slice());

    server_handle.abort();
}

// P4 #18: an unreachable upstream produces a valid RFC 9457 Problem body with correct GTS type.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_unreachable_backend_returns_rfc9457_problem_body() {
    let h = AppHarness::builder()
        .with_request_timeout(std::time::Duration::from_secs(5))
        .build()
        .await;
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
                        port: 19993,
                    }],
                },
                HTTP_PROTOCOL_ID,
            )
            .alias("rfc9457-body-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/test".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/rfc9457-body-test/v1/test")
        .body(Body::Empty)
        .unwrap();

    match h.facade().proxy_request(ctx.clone(), req).await {
        Err(err) => {
            // Went through DomainError path — already covered by other tests.
            // Still verify the variant is one of the two expected for an
            // unreachable backend (not e.g. ValidationError which would be a bug).
            assert!(
                matches!(
                    err,
                    CanonicalError::ServiceUnavailable { .. }
                        | CanonicalError::DeadlineExceeded { .. }
                ),
                "expected ServiceUnavailable or DeadlineExceeded for unreachable backend, got: {err:?}"
            );
        }
        Ok(resp) => {
            // A gateway-generated error response. An
            // unreachable backend maps to 502 (DownstreamError), 503
            // (LinkUnavailable), or 504 (connect timeout).
            let status = resp.status();
            assert!(
                status == StatusCode::BAD_GATEWAY
                    || status == StatusCode::SERVICE_UNAVAILABLE
                    || status == StatusCode::GATEWAY_TIMEOUT,
                "expected 502, 503, or 504, got: {status}"
            );

            let body_bytes = resp.into_body().into_bytes().await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body_bytes)
                .expect("fail_to_proxy response body should be valid JSON");

            // Must have all RFC 9457 Problem fields.
            assert!(body.get("type").is_some(), "missing 'type' field");
            assert!(body.get("title").is_some(), "missing 'title' field");
            assert!(body.get("status").is_some(), "missing 'status' field");
            assert!(body.get("detail").is_some(), "missing 'detail' field");
            assert!(body.get("instance").is_some(), "missing 'instance' field");

            // GTS type must not be about:blank.
            let gts_type = body["type"].as_str().unwrap();
            let gts_id = gts_type
                .strip_prefix(GTS_ID_URI_PREFIX)
                .expect("canonical problem type should use GTS URI prefix");
            assert!(
                gts::GtsId::try_new(gts_id).is_ok(),
                "expected canonical GTS error type, got: {gts_type}"
            );

            // Status field in body must match HTTP status.
            assert_eq!(
                body["status"].as_u64().unwrap(),
                status.as_u16() as u64,
                "body status must match HTTP status"
            );
        }
    }
}

// negative-8.1 (body-validation): Streaming body exceeding max_body_size returns 413 PayloadTooLarge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_streaming_body_exceeding_limit_returns_413() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/v1/upload",
        MockResponse {
            status: 200,
            headers: vec![],
            body: MockBody::Json(serde_json::json!({"ok": true})),
        },
    );

    let h = AppHarness::builder()
        .with_max_body_size(64) // tiny limit for testing
        .build()
        .await;
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
            .alias("body-limit-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/v1/upload"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Build a streaming body that exceeds the 64-byte limit.
    let chunks: Vec<Result<bytes::Bytes, oagw_sdk::body::BoxError>> = vec![
        Ok(bytes::Bytes::from(vec![b'A'; 40])),
        Ok(bytes::Bytes::from(vec![b'B'; 40])), // total = 80 > 64
    ];
    let stream: oagw_sdk::body::BodyStream = Box::pin(futures_util::stream::iter(chunks));
    let body = Body::Stream(stream);

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/body-limit-test{}/v1/upload", guard.prefix()))
        .header(http::header::CONTENT_TYPE, "application/octet-stream")
        .body(body)
        .unwrap();

    match h.facade().proxy_request(ctx.clone(), req).await {
        Err(err) => assert!(
            matches!(err, CanonicalError::OutOfRange { .. }),
            "expected PayloadTooLarge, got: {err:?}"
        ),
        Ok(resp) => panic!(
            "expected PayloadTooLarge error, got response with status {}",
            resp.status()
        ),
    }
}

// Body::Stream POST must reach the upstream intact via chunked transfer
// encoding through the upstream engine.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_streaming_body_post_arrives_intact() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/v1/upload",
        MockResponse {
            status: 200,
            headers: vec![],
            body: MockBody::Json(serde_json::json!({"received": true})),
        },
    );

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
            .alias("stream-body-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/v1/upload"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Build a multi-chunk streaming body (simulates multipart/streaming upload).
    let chunk_a = bytes::Bytes::from_static(b"hello ");
    let chunk_b = bytes::Bytes::from_static(b"streamed ");
    let chunk_c = bytes::Bytes::from_static(b"world");
    let chunks: Vec<Result<bytes::Bytes, oagw_sdk::body::BoxError>> =
        vec![Ok(chunk_a), Ok(chunk_b), Ok(chunk_c)];
    let stream: oagw_sdk::body::BodyStream = Box::pin(futures_util::stream::iter(chunks));
    let body = Body::Stream(stream);

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/stream-body-test{}/v1/upload", guard.prefix()))
        .header(http::header::CONTENT_TYPE, "application/octet-stream")
        .body(body)
        .unwrap();

    let resp = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "streaming POST should succeed"
    );

    // Verify the upstream mock received the complete, reassembled body.
    let recorded = guard.recorded_requests().await;
    assert_eq!(recorded.len(), 1, "expected exactly one recorded request");
    assert_eq!(
        recorded[0].body, b"hello streamed world",
        "upstream must receive the full concatenated streaming body"
    );
}

// Empty chunks in a Body::Stream must be silently skipped — writing a
// zero-length chunk would emit the chunked terminator (0\r\n\r\n) and
// prematurely end the body.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_streaming_body_with_empty_chunks_succeeds() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/v1/upload-empty",
        MockResponse {
            status: 200,
            headers: vec![],
            body: MockBody::Json(serde_json::json!({"received": true})),
        },
    );

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
            .alias("stream-empty-chunks")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/v1/upload-empty"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Interleave real chunks with empty ones.
    let chunks: Vec<Result<bytes::Bytes, oagw_sdk::body::BoxError>> = vec![
        Ok(bytes::Bytes::new()), // empty — must be skipped
        Ok(bytes::Bytes::from_static(b"AB")),
        Ok(bytes::Bytes::new()), // empty — must be skipped
        Ok(bytes::Bytes::new()), // empty — must be skipped
        Ok(bytes::Bytes::from_static(b"CD")),
        Ok(bytes::Bytes::new()), // trailing empty
    ];
    let stream: oagw_sdk::body::BodyStream = Box::pin(futures_util::stream::iter(chunks));
    let body = Body::Stream(stream);

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/stream-empty-chunks{}/v1/upload-empty",
            guard.prefix()
        ))
        .header(http::header::CONTENT_TYPE, "application/octet-stream")
        .body(body)
        .unwrap();

    let resp = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "streaming POST with empty chunks should succeed"
    );

    let recorded = guard.recorded_requests().await;
    assert_eq!(recorded.len(), 1, "expected exactly one recorded request");
    assert_eq!(
        recorded[0].body, b"ABCD",
        "upstream must receive only the non-empty chunks, concatenated"
    );
}

// Single-chunk streaming body (boundary condition).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_streaming_body_single_chunk() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/v1/upload",
        MockResponse {
            status: 200,
            headers: vec![],
            body: MockBody::Json(serde_json::json!({"received": true})),
        },
    );

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
            .alias("stream-single-chunk")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/v1/upload"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let chunks: Vec<Result<bytes::Bytes, oagw_sdk::body::BoxError>> =
        vec![Ok(bytes::Bytes::from_static(b"single-payload"))];
    let stream: oagw_sdk::body::BodyStream = Box::pin(futures_util::stream::iter(chunks));
    let body = Body::Stream(stream);

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/stream-single-chunk{}/v1/upload", guard.prefix()))
        .header(http::header::CONTENT_TYPE, "application/octet-stream")
        .body(body)
        .unwrap();

    let resp = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "single-chunk streaming POST should succeed"
    );

    let recorded = guard.recorded_requests().await;
    assert_eq!(recorded.len(), 1, "expected exactly one recorded request");
    assert_eq!(
        recorded[0].body, b"single-payload",
        "upstream must receive the single chunk intact"
    );
}

// A stream error mid-body sends the cause on the abort channel so the chunked
// terminator is NOT written.  The main select! receives the reason immediately,
// returning a DownstreamError without waiting for the request timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proxy_streaming_body_error_mid_stream_does_not_send_terminator() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/v1/upload-err",
        MockResponse {
            status: 200,
            headers: vec![],
            body: MockBody::Json(serde_json::json!({"received": true})),
        },
    );

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
            .alias("stream-err-test")
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/v1/upload-err"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // First chunk succeeds, second chunk is an error — triggers the abort channel.
    let chunks: Vec<Result<bytes::Bytes, oagw_sdk::body::BoxError>> = vec![
        Ok(bytes::Bytes::from_static(b"partial")),
        Err(Box::new(std::io::Error::other("simulated stream failure"))),
    ];
    let stream: oagw_sdk::body::BodyStream = Box::pin(futures_util::stream::iter(chunks));
    let body = Body::Stream(stream);

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/stream-err-test{}/v1/upload-err", guard.prefix()))
        .header(http::header::CONTENT_TYPE, "application/octet-stream")
        .body(body)
        .unwrap();

    match h.facade().proxy_request(ctx.clone(), req).await {
        Err(err) => assert!(
            matches!(err, CanonicalError::ServiceUnavailable { .. }),
            "expected DownstreamError, got: {err:?}"
        ),
        Ok(resp) => panic!(
            "expected DownstreamError, got response with status {}",
            resp.status()
        ),
    }
}

// ---------------------------------------------------------------------------
// OAuth2 Client Credentials integration tests
// ---------------------------------------------------------------------------

/// 9.5: OAuth2 CC happy path — token fetched from mock IdP, injected as Bearer.
///
/// SREF-REG-001: OAuth2 auth is unchanged by per-request secret references.
#[tokio::test]
async fn proxy_oauth2_client_cred_injects_bearer_token() {
    let mut guard = MockGuard::new();

    // Mock token endpoint on the same shared mock server.
    guard.mock(
        "POST",
        "/oauth/token",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(
                json!({"access_token":"tok-oauth2-test","expires_in":3600,"token_type":"Bearer"}),
            ),
        },
    );

    // Mock upstream API endpoint.
    guard.mock(
        "GET",
        "/api/resource",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"status":"ok"})),
        },
    );

    let h = AppHarness::builder()
        .with_credentials(vec![
            ("cred://oauth2-client-id".into(), "test-id".into()),
            ("cred://oauth2-client-secret".into(), "test-secret".into()),
        ])
        .build()
        .await;
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
            .alias("oauth2-test")
            .auth(oagw_sdk::AuthConfig {
                plugin_type: OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
                sharing: SharingMode::Private,
                config: Some(
                    [
                        (
                            "token_endpoint".into(),
                            format!(
                                "http://127.0.0.1:{}{}",
                                h.mock_port(),
                                guard.path("/oauth/token")
                            ),
                        ),
                        ("client_id_ref".into(), "cred://oauth2-client-id".into()),
                        (
                            "client_secret_ref".into(),
                            "cred://oauth2-client-secret".into(),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: guard.path("/api/resource"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/oauth2-test{}", guard.path("/api/resource")))
        .body(Body::Empty)
        .unwrap();
    let response = h.facade().proxy_request(ctx, req).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    // Verify the upstream received the Bearer token from the OAuth2 flow.
    let recorded = guard.recorded_requests().await;
    let api_request = recorded
        .iter()
        .find(|r| r.uri.contains("/api/resource"))
        .expect("upstream API request not found");
    let auth_header = api_request
        .headers
        .iter()
        .find(|(k, _)| k == "authorization")
        .map(|(_, v)| v.as_str())
        .expect("authorization header missing");
    assert_eq!(auth_header, "Bearer tok-oauth2-test");
}

/// 9.5: OAuth2 CC refs that the credential store does not hold are refused
/// on write (#4974), naming the key, instead of failing every request.
#[tokio::test]
async fn oauth2_refs_are_checked_on_write() {
    // No credentials loaded for the OAuth2 refs.
    let h = AppHarness::builder().build().await;
    let err = h
        .facade()
        .create_upstream(
            h.security_context().clone(),
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
            .alias("oauth2-missing-creds")
            .auth(oagw_sdk::AuthConfig {
                plugin_type: OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID.into(),
                sharing: SharingMode::Private,
                config: Some(
                    [
                        (
                            "token_endpoint".into(),
                            format!("http://127.0.0.1:{}/oauth/token", h.mock_port()),
                        ),
                        ("client_id_ref".into(), "cred://missing-id".into()),
                        ("client_secret_ref".into(), "cred://missing-secret".into()),
                    ]
                    .into_iter()
                    .collect(),
                ),
            })
            .build(),
        )
        .await
        .expect_err("refs the credential store does not hold must be refused");
    match err {
        CanonicalError::FailedPrecondition { ctx, detail, .. } => {
            let subjects: Vec<&str> = ctx.violations.iter().map(|v| v.subject.as_str()).collect();
            assert_eq!(subjects, ["auth.config.client_id_ref"]);
            // The reference itself is not echoed.
            assert!(!detail.contains("missing-id"), "{detail}");
            assert!(
                ctx.violations
                    .iter()
                    .all(|v| !v.description.contains("missing-id")),
                "{:?}",
                ctx.violations
            );
        }
        other => panic!("expected FailedPrecondition, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Guard plugin integration tests
// ---------------------------------------------------------------------------

const REQUIRED_HEADERS_GUARD_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.guard_plugin.v1~cf.core.oagw.required_headers.v1");

/// Verify that the RequiredHeadersGuardPlugin allows requests that include
/// all required headers.
#[tokio::test]
async fn proxy_guard_allows_when_required_header_present() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/guard-hdr-ok",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

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
            .alias("guard-hdr-ok")
            .headers(HeadersConfig {
                request: Some(RequestHeaderRules {
                    passthrough: PassthroughMode::All,
                    ..Default::default()
                }),
                response: None,
            })
            .plugins(PluginsConfig {
                sharing: SharingMode::Private,
                items: vec![PluginBinding {
                    plugin_ref: REQUIRED_HEADERS_GUARD_PLUGIN_ID.to_string(),
                    config: [("required_request_headers".into(), "x-correlation-id".into())].into(),
                }],
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/guard-hdr-ok"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/guard-hdr-ok{}", guard.path("/guard-hdr-ok")))
        .header(http::header::CONTENT_TYPE, "application/json")
        .header("x-correlation-id", "test-123")
        .body(Body::from(r#"{"test": true}"#))
        .unwrap();

    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// Verify that the RequiredHeadersGuardPlugin rejects requests missing a
/// required header, returning a 400 GuardRejected error.
#[tokio::test]
async fn proxy_guard_rejects_missing_required_header() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/guard-hdr-miss",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

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
            .alias("guard-hdr-miss")
            .plugins(PluginsConfig {
                sharing: SharingMode::Private,
                items: vec![PluginBinding {
                    plugin_ref: REQUIRED_HEADERS_GUARD_PLUGIN_ID.to_string(),
                    config: [("required_request_headers".into(), "x-correlation-id".into())].into(),
                }],
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/guard-hdr-miss"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Send request WITHOUT the required x-correlation-id header.
    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/guard-hdr-miss{}", guard.path("/guard-hdr-miss")))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"test": true}"#))
        .unwrap();

    let err = h
        .facade()
        .proxy_request(ctx.clone(), req)
        .await
        .expect_err("guard should reject missing required header");

    // GuardRejected no longer exists as a typed variant — the impl crate
    // maps a 400/422 guard rejection into the canonical `InvalidArgument`
    // category with the plugin-supplied `error_code` in
    // `field_violations[].reason`. Assert directly on the canonical.
    match err {
        CanonicalError::InvalidArgument { ctx, .. } => {
            let reason = match ctx {
                toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations } => {
                    field_violations
                        .first()
                        .map(|v| v.reason.clone())
                        .unwrap_or_default()
                }
                _ => panic!("expected FieldViolations variant"),
            };
            assert_eq!(
                reason, "REQUIRED_HEADER_MISSING",
                "expected REQUIRED_HEADER_MISSING in field_violations[].reason, got {reason}",
            );
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }

    // Verify the request never reached the upstream.
    let recorded = guard.recorded_requests().await;
    assert!(
        recorded.is_empty(),
        "rejected request should not reach upstream"
    );
}

/// Verify that an unconfigured RequiredHeadersGuardPlugin allows all requests.
#[tokio::test]
async fn proxy_guard_allows_unconfigured() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/guard-hdr-noconf",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

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
            .alias("guard-hdr-noconf")
            .plugins(PluginsConfig {
                sharing: SharingMode::Private,
                items: vec![PluginBinding {
                    plugin_ref: REQUIRED_HEADERS_GUARD_PLUGIN_ID.to_string(),
                    config: HashMap::new(),
                }],
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/guard-hdr-noconf"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/guard-hdr-noconf{}",
            guard.path("/guard-hdr-noconf")
        ))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"test": true}"#))
        .unwrap();

    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Transform plugin integration tests
// ---------------------------------------------------------------------------

const REQUEST_ID_TRANSFORM_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.transform_plugin.v1~cf.core.oagw.request_id.v1");

/// Verify that the RequestIdTransformPlugin injects an X-Request-ID header when
/// the inbound request does not include one.
#[tokio::test]
async fn proxy_transform_injects_request_id() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/transform-test",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

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
            .alias("transform-inject")
            .plugins(PluginsConfig {
                sharing: SharingMode::Private,
                items: vec![PluginBinding {
                    plugin_ref: REQUEST_ID_TRANSFORM_PLUGIN_ID.to_string(),
                    config: Default::default(),
                }],
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/transform-test"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Send request WITHOUT X-Request-ID.
    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/transform-inject{}",
            guard.path("/transform-test")
        ))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"test": true}"#))
        .unwrap();

    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // The mock upstream should have received the request WITH an X-Request-ID.
    let recorded = guard.recorded_requests().await;
    assert_eq!(recorded.len(), 1);
    let has_request_id = recorded[0]
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("x-request-id"));
    assert!(
        has_request_id,
        "upstream should have received x-request-id header injected by transform plugin"
    );
}

/// Verify that the RequestIdTransformPlugin preserves an existing X-Request-ID
/// from the inbound request (does not overwrite it).
#[tokio::test]
async fn proxy_transform_preserves_request_id() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/transform-preserve",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

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
            .alias("transform-preserve")
            .headers(HeadersConfig {
                request: Some(RequestHeaderRules {
                    passthrough: PassthroughMode::All,
                    ..Default::default()
                }),
                ..Default::default()
            })
            .plugins(PluginsConfig {
                sharing: SharingMode::Private,
                items: vec![PluginBinding {
                    plugin_ref: REQUEST_ID_TRANSFORM_PLUGIN_ID.to_string(),
                    config: Default::default(),
                }],
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path("/transform-preserve"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // Send request WITH an existing X-Request-ID.
    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/transform-preserve{}",
            guard.path("/transform-preserve")
        ))
        .header(http::header::CONTENT_TYPE, "application/json")
        .header("x-request-id", "custom-trace-id-999")
        .body(Body::from(r#"{"test": true}"#))
        .unwrap();

    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // The mock upstream should have received the ORIGINAL X-Request-ID.
    let recorded = guard.recorded_requests().await;
    assert_eq!(recorded.len(), 1);
    let request_id = recorded[0]
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-request-id"))
        .map(|(_, v)| v.as_str());
    assert_eq!(
        request_id,
        Some("custom-trace-id-999"),
        "upstream should have received the original x-request-id, not a generated one"
    );
}

/// A transform plugin OAGW does not implement is refused on write (#4974).
#[tokio::test]
async fn create_with_unknown_transform_plugin_is_rejected() {
    let h = AppHarness::builder().build().await;
    let err = h
        .facade()
        .create_upstream(
            h.security_context().clone(),
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
            .alias("transform-error")
            .plugins(PluginsConfig {
                sharing: SharingMode::Private,
                items: vec![PluginBinding {
                    plugin_ref: gts_id!(
                        "cf.core.oagw.transform_plugin.v1~cf.core.oagw.nonexistent.v1"
                    )
                    .to_string(),
                    config: Default::default(),
                }],
            })
            .build(),
        )
        .await
        .expect_err("an unknown transform plugin must be refused");
    assert_invalid_argument(err, "plugins.items.plugin_ref", "NOT_IMPLEMENTED");
}

// ---------------------------------------------------------------------------
// CORS integration tests
// ---------------------------------------------------------------------------

fn test_cors_config() -> CorsConfig {
    CorsConfig {
        sharing: SharingMode::Private,
        enabled: true,
        allowed_origins: vec!["https://example.com".to_string()],
        allowed_methods: vec![CorsHttpMethod::Get, CorsHttpMethod::Post],
        expose_headers: vec!["x-request-id".to_string()],
        allow_credentials: false,
    }
}

/// Helper: create an upstream with CORS enabled and a single route pointing at the mock.
async fn setup_cors_upstream(
    h: &AppHarness,
    guard: &MockGuard,
    alias: &str,
    cors: Option<CorsConfig>,
) -> uuid::Uuid {
    let ctx = h.security_context().clone();

    let mut builder = CreateUpstreamRequest::builder(
        Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Http,
                host: "127.0.0.1".into(),
                port: h.mock_port(),
            }],
        },
        HTTP_PROTOCOL_ID,
    )
    .alias(alias);

    if let Some(c) = cors {
        builder = builder.cors(c);
    }

    let upstream = h
        .facade()
        .create_upstream(ctx.clone(), builder.build())
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get, HttpMethod::Post],
                        path: guard.path("/api/data"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    upstream.id
}

// CORS: Upstream Vary header is preserved alongside CORS Vary header.
#[tokio::test]
async fn cors_actual_request_preserves_upstream_vary() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/api/data",
        MockResponse {
            status: 200,
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("vary".into(), "Accept-Encoding".into()),
            ],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

    let h = AppHarness::builder().build().await;
    setup_cors_upstream(&h, &guard, "cors-vary", Some(test_cors_config())).await;

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/cors-vary{}", guard.path("/api/data")))
        .header("origin", "https://example.com")
        .body(Body::Empty)
        .unwrap();

    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let vary_values: Vec<&str> = response
        .headers()
        .get_all(http::header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();

    assert!(
        vary_values.iter().any(|v| v.contains("Accept-Encoding")),
        "upstream Vary: Accept-Encoding must be preserved, got: {vary_values:?}"
    );
    assert!(
        vary_values.iter().any(|v| v.contains("Origin")),
        "CORS Vary: Origin must be present, got: {vary_values:?}"
    );
}

// CORS: Actual request with allowed origin includes CORS headers.
#[tokio::test]
async fn cors_actual_request_includes_headers() {
    let mut guard = MockGuard::new();
    guard.mock(
        "POST",
        "/api/data",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

    let h = AppHarness::builder().build().await;
    setup_cors_upstream(&h, &guard, "cors-actual", Some(test_cors_config())).await;

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/cors-actual{}", guard.path("/api/data")))
        .header("origin", "https://example.com")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"test": true}"#))
        .unwrap();

    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let headers = response.headers();
    assert_eq!(
        headers.get("access-control-allow-origin").unwrap(),
        "https://example.com"
    );
    assert_eq!(
        headers.get("access-control-expose-headers").unwrap(),
        "x-request-id"
    );
    assert!(
        headers.get(http::header::VARY).is_some(),
        "Vary header must be present"
    );
}

// CORS: No CORS config means no CORS headers in response.
#[tokio::test]
async fn cors_disabled_no_cors_headers() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/api/data",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

    let h = AppHarness::builder().build().await;
    setup_cors_upstream(&h, &guard, "cors-disabled", None).await;

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/cors-disabled{}", guard.path("/api/data")))
        .header("origin", "https://example.com")
        .body(Body::Empty)
        .unwrap();

    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none(),
        "no CORS config should produce no CORS headers"
    );
}

// CORS: Upstream error (500) still includes CORS headers so browsers can read error details.
#[tokio::test]
async fn cors_headers_present_on_upstream_error_response() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/api/data",
        MockResponse {
            status: 500,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"error": "internal server error"})),
        },
    );

    let h = AppHarness::builder().build().await;
    setup_cors_upstream(&h, &guard, "cors-err", Some(test_cors_config())).await;

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/cors-err{}", guard.path("/api/data")))
        .header("origin", "https://example.com")
        .body(Body::Empty)
        .unwrap();

    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "https://example.com",
        "CORS headers must be present even on error responses"
    );
}

// CORS: allow_credentials reflects origin and sets credentials header.
#[tokio::test]
async fn cors_credentials_reflects_origin() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/api/data",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

    let h = AppHarness::builder().build().await;
    let cors = CorsConfig {
        allow_credentials: true,
        ..test_cors_config()
    };
    setup_cors_upstream(&h, &guard, "cors-creds", Some(cors)).await;

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/cors-creds{}", guard.path("/api/data")))
        .header("origin", "https://example.com")
        .body(Body::Empty)
        .unwrap();

    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let headers = response.headers();
    assert_eq!(
        headers.get("access-control-allow-origin").unwrap(),
        "https://example.com",
        "credentials mode must reflect origin, not wildcard"
    );
    assert_eq!(
        headers.get("access-control-allow-credentials").unwrap(),
        "true"
    );
}

// CORS: Wildcard origin returns literal "*".
#[tokio::test]
async fn cors_wildcard_origin_returns_star() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/api/data",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

    let h = AppHarness::builder().build().await;
    let cors = CorsConfig {
        allowed_origins: vec!["*".to_string()],
        ..test_cors_config()
    };
    setup_cors_upstream(&h, &guard, "cors-wildcard", Some(cors)).await;

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/cors-wildcard{}", guard.path("/api/data")))
        .header("origin", "https://anything.example.com")
        .body(Body::Empty)
        .unwrap();

    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "*",
        "wildcard config must return literal '*'"
    );
}

// CORS: Route-level CORS with Inherit merges origins from upstream.
#[tokio::test]
async fn cors_route_inherit_merges_origins() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/api/data",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

    let h = AppHarness::builder().build().await;
    let ctx = h.security_context().clone();

    // Upstream allows https://example.com
    let upstream_cors = test_cors_config();
    let mut builder = CreateUpstreamRequest::builder(
        Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Http,
                host: "127.0.0.1".into(),
                port: h.mock_port(),
            }],
        },
        HTTP_PROTOCOL_ID,
    )
    .alias("cors-route-inherit");
    builder = builder.cors(upstream_cors);

    let upstream = h
        .facade()
        .create_upstream(ctx.clone(), builder.build())
        .await
        .unwrap();

    // Route adds https://other.com via Inherit sharing.
    let route_cors = CorsConfig {
        sharing: SharingMode::Inherit,
        enabled: true,
        allowed_origins: vec!["https://other.com".to_string()],
        allowed_methods: vec![CorsHttpMethod::Get, CorsHttpMethod::Post],
        expose_headers: vec!["x-request-id".to_string()],
        allow_credentials: false,
    };

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get, HttpMethod::Post],
                        path: guard.path("/api/data"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .cors(route_cors)
            .build(),
        )
        .await
        .unwrap();

    // Request from the route-added origin should be allowed.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/cors-route-inherit{}", guard.path("/api/data")))
        .header("origin", "https://other.com")
        .body(Body::Empty)
        .unwrap();

    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "https://other.com",
        "route-level Inherit origin must be allowed"
    );

    // Request from the upstream origin should also still be allowed.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/cors-route-inherit{}", guard.path("/api/data")))
        .header("origin", "https://example.com")
        .body(Body::Empty)
        .unwrap();

    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "https://example.com",
        "upstream origin must still be allowed after route Inherit merge"
    );
}

// CORS: Disallowed origin is rejected before reaching upstream.
#[tokio::test]
async fn cors_actual_request_disallowed_origin_rejected_before_upstream() {
    let guard = MockGuard::new();
    // No mock registered — if the upstream is called, the request will fail differently.

    let h = AppHarness::builder().build().await;
    setup_cors_upstream(&h, &guard, "cors-reject-origin", Some(test_cors_config())).await;

    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/cors-reject-origin{}", guard.path("/api/data")))
        .header("origin", "https://evil.com")
        .body(Body::Empty)
        .unwrap();

    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await;
    assert!(
        response.is_err(),
        "disallowed origin on actual request should be rejected before reaching upstream"
    );
}

// CORS: Disallowed method is rejected before reaching upstream.
#[tokio::test]
async fn cors_actual_request_disallowed_method_rejected_before_upstream() {
    let guard = MockGuard::new();
    // No mock registered — if the upstream is called, the request will fail differently.

    let h = AppHarness::builder().build().await;
    let ctx = h.security_context().clone();

    // Route allows DELETE but CORS config only allows GET and POST.
    let cors = test_cors_config(); // allowed_methods: [Get, Post]

    let mut builder = CreateUpstreamRequest::builder(
        Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Http,
                host: "127.0.0.1".into(),
                port: h.mock_port(),
            }],
        },
        HTTP_PROTOCOL_ID,
    )
    .alias("cors-reject-method");
    builder = builder.cors(cors);

    let upstream = h
        .facade()
        .create_upstream(ctx.clone(), builder.build())
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get, HttpMethod::Post, HttpMethod::Delete],
                        path: guard.path("/api/data"),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // DELETE is allowed by the route but not by CORS — should be rejected.
    let req = http::Request::builder()
        .method(Method::DELETE)
        .uri(format!("/cors-reject-method{}", guard.path("/api/data")))
        .header("origin", "https://example.com")
        .body(Body::Empty)
        .unwrap();

    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await;
    assert!(
        response.is_err(),
        "disallowed method on actual request should be rejected before reaching upstream"
    );
}

// CORS: Multiple specific origins — matching origin gets headers, non-matching doesn't.
#[tokio::test]
async fn cors_multiple_specific_origins() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/api/data",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );

    let h = AppHarness::builder().build().await;
    let cors = CorsConfig {
        allowed_origins: vec![
            "https://alpha.com".to_string(),
            "https://beta.com".to_string(),
        ],
        ..test_cors_config()
    };
    setup_cors_upstream(&h, &guard, "cors-multi", Some(cors)).await;

    // Matching origin gets CORS headers.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/cors-multi{}", guard.path("/api/data")))
        .header("origin", "https://beta.com")
        .body(Body::Empty)
        .unwrap();

    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "https://beta.com"
    );

    // Non-matching origin is rejected before reaching upstream.
    let req = http::Request::builder()
        .method(Method::GET)
        .uri(format!("/cors-multi{}", guard.path("/api/data")))
        .header("origin", "https://gamma.com")
        .body(Body::Empty)
        .unwrap();

    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await;
    assert!(
        response.is_err(),
        "non-matching origin should be rejected with 403"
    );
}

// ---------------------------------------------------------------------------
// WebSocket frame-aware relay integration tests
// ---------------------------------------------------------------------------

/// Read HTTP response headers from a raw TCP stream until the `\r\n\r\n` terminator.
/// Returns the complete header block as a String. Panics on timeout or if the
/// buffer fills without finding the terminator.
async fn read_http_response_headers(stream: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt;
    // Read one byte at a time so we never consume bytes beyond the
    // \r\n\r\n header terminator. Chunk-based reads can over-read into
    // the WebSocket frame stream, silently swallowing data and causing
    // flaky EOF errors on the first frame read.
    let mut buf = Vec::with_capacity(4096);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let mut byte = [0u8; 1];
        tokio::time::timeout_at(deadline, stream.read_exact(&mut byte))
            .await
            .expect("timed out reading HTTP response headers")
            .expect("read error / unexpected EOF");
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            return String::from_utf8_lossy(&buf).into_owned();
        }
        assert!(buf.len() < 4096, "header buffer full without \\r\\n\\r\\n");
    }
}

/// Helper: perform a WebSocket handshake over a raw TCP stream.
/// Returns the stream positioned after the 101 response headers and after
/// confirming the proxy bridge is fully operational via a Ping/Pong probe.
///
/// If the bridge task hasn't started in time (CI/coverage builds can be slow),
/// the proxy may send a Close frame before the readiness probe Pong arrives.
/// In that case we reconnect and retry up to `MAX_HANDSHAKE_RETRIES` times.
async fn ws_handshake(stream: &mut tokio::net::TcpStream, uri: &str) {
    ws_handshake_to(stream, uri, stream.peer_addr().unwrap()).await;
}

/// Like [`ws_handshake`] but accepts a separate `addr` for reconnection.
/// Used by call sites that haven't connected the stream yet, or need the
/// addr for retry logic.
async fn ws_handshake_to(
    stream: &mut tokio::net::TcpStream,
    uri: &str,
    addr: std::net::SocketAddr,
) {
    ws_handshake_to_with_request(stream, &default_ws_upgrade_request(uri), addr).await
}

/// Like [`ws_handshake_to`] but accepts a caller-provided HTTP/1.1 upgrade
/// request — useful for tests that exercise non-standard headers (e.g. a
/// multi-token `Connection` value). Reuses the same readiness-probe + retry
/// shell as the default handshake so the test still survives the bridge-not-
/// ready race that's especially common under coverage instrumentation.
async fn ws_handshake_to_with_request(
    stream: &mut tokio::net::TcpStream,
    req: &str,
    addr: std::net::SocketAddr,
) {
    const MAX_HANDSHAKE_RETRIES: usize = 3;
    for attempt in 0..=MAX_HANDSHAKE_RETRIES {
        ws_send_upgrade_request(stream, req).await;
        match ws_readiness_probe(stream).await {
            Ok(()) => return,
            Err(msg) if attempt < MAX_HANDSHAKE_RETRIES => {
                eprintln!(
                    "ws_handshake: attempt {}/{} failed ({msg}), reconnecting",
                    attempt + 1,
                    MAX_HANDSHAKE_RETRIES + 1,
                );
                tokio::time::sleep(std::time::Duration::from_millis(50 * (attempt as u64 + 1)))
                    .await;
                *stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            }
            Err(msg) => {
                panic!(
                    "ws_handshake: readiness probe failed after {} attempts: {msg}",
                    MAX_HANDSHAKE_RETRIES + 1,
                );
            }
        }
    }
}

/// Build the standard WebSocket upgrade request used by [`ws_handshake_to`].
fn default_ws_upgrade_request(uri: &str) -> String {
    format!(
        "GET {uri} HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n"
    )
}

/// Send a pre-built HTTP/1.1 upgrade request and assert a 101 response.
async fn ws_send_upgrade_request(stream: &mut tokio::net::TcpStream, req: &str) {
    use tokio::io::AsyncWriteExt;
    stream.write_all(req.as_bytes()).await.unwrap();
    let resp = read_http_response_headers(stream).await;
    assert!(
        resp.starts_with("HTTP/1.1 101"),
        "expected 101 Switching Protocols, got: {resp}"
    );
}

/// Helper: build a masked WebSocket frame for sending from a client.
fn build_masked_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mask = [0x37, 0xfa, 0x21, 0x3d];
    let mut frame = Vec::new();
    frame.push(0x80 | opcode); // FIN + opcode
    if payload.len() < 126 {
        frame.push(0x80 | payload.len() as u8); // MASK + len
    } else {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(&mask);
    for (i, &byte) in payload.iter().enumerate() {
        frame.push(byte ^ mask[i % 4]);
    }
    frame
}

/// Helper: read a WebSocket frame from a raw stream.
/// Returns (opcode, payload) or None on EOF or timeout.
async fn read_ws_frame(stream: &mut tokio::net::TcpStream) -> Option<(u8, Vec<u8>)> {
    use tokio::io::AsyncReadExt;
    let timeout = std::time::Duration::from_secs(5);

    let mut hdr = [0u8; 2];
    match tokio::time::timeout(timeout, stream.read_exact(&mut hdr)).await {
        Ok(Ok(_)) => {}
        _ => return None,
    }
    let opcode = hdr[0] & 0x0F;
    let masked = hdr[1] & 0x80 != 0;
    let len_byte = (hdr[1] & 0x7F) as u64;

    let payload_len: usize = if len_byte < 126 {
        len_byte as usize
    } else if len_byte == 126 {
        let mut buf = [0u8; 2];
        tokio::time::timeout(timeout, stream.read_exact(&mut buf))
            .await
            .ok()?
            .ok()?;
        u16::from_be_bytes(buf) as usize
    } else {
        let mut buf = [0u8; 8];
        tokio::time::timeout(timeout, stream.read_exact(&mut buf))
            .await
            .ok()?
            .ok()?;
        u64::from_be_bytes(buf) as usize
    };

    let mask_key = if masked {
        let mut key = [0u8; 4];
        tokio::time::timeout(timeout, stream.read_exact(&mut key))
            .await
            .ok()?
            .ok()?;
        Some(key)
    } else {
        None
    };

    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        tokio::time::timeout(timeout, stream.read_exact(&mut payload))
            .await
            .ok()?
            .ok()?;
    }
    if let Some(key) = mask_key {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= key[i % 4];
        }
    }
    Some((opcode, payload))
}

/// Send a Ping and wait for the matching Pong, confirming the proxy bridge
/// pipeline is fully operational before the test sends data frames.
///
/// Returns `Ok(())` on success, or `Err(message)` if the bridge sent a
/// Close frame or EOF before the Pong arrived (transient under CI load).
async fn ws_readiness_probe(stream: &mut tokio::net::TcpStream) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let probe = b"ready";
    let ping = build_masked_frame(0x9, probe);
    if let Err(e) = stream.write_all(&ping).await {
        return Err(format!("failed to send Ping: {e}"));
    }
    loop {
        match read_ws_frame(stream).await {
            Some((0xA, data)) if data == probe => return Ok(()),
            Some((0xA, _)) => continue, // stale or unrelated Pong
            Some((0x9, _)) => continue, // Ping from upstream
            Some((0x8, close_payload)) => {
                return Err(format!(
                    "server sent Close frame ({close_payload:?}) before Pong — \
                     bridge task likely not ready yet"
                ));
            }
            None => {
                return Err("connection closed (EOF) before Pong".into());
            }
            other => {
                return Err(format!("unexpected frame: {other:?}"));
            }
        }
    }
}

/// Helper: set up an upstream + route pointing at the mock's /ws/echo endpoint.
async fn setup_ws_upstream(h: &AppHarness, alias: &str) {
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
        .unwrap();

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
        .unwrap();
}

/// Helper: start an Axum server on a random port and return the address.
async fn start_oagw_server(h: &AppHarness) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = h.router().clone();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    (addr, handle)
}

// 14.5: Close frame is forwarded through the gateway with status code preserved.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_websocket_close_frame_propagated() {
    let h = AppHarness::builder().build().await;
    setup_ws_upstream(&h, "ws-close-prop").await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    ws_handshake(&mut stream, "/oagw/v1/proxy/ws-close-prop/ws/echo").await;

    // Send a text frame to confirm the connection works.
    let text_frame = build_masked_frame(0x1, b"alive");
    tokio::io::AsyncWriteExt::write_all(&mut stream, &text_frame)
        .await
        .unwrap();
    let echo = read_ws_frame(&mut stream).await.unwrap();
    assert_eq!(echo.0, 0x1); // text opcode
    assert_eq!(echo.1, b"alive");

    // Send Close 1000 (Normal Closure).
    let mut close_payload = 1000u16.to_be_bytes().to_vec();
    close_payload.extend_from_slice(b"Normal");
    let close_frame = build_masked_frame(0x8, &close_payload);
    tokio::io::AsyncWriteExt::write_all(&mut stream, &close_frame)
        .await
        .unwrap();

    // Should receive Close frame back (echoed by mock upstream through gateway).
    // The gateway may tear down the TCP connection immediately after the close
    // handshake, so we may get the Close frame or just EOF.
    match read_ws_frame(&mut stream).await {
        Some((opcode, payload)) => {
            assert_eq!(opcode, 0x8, "expected Close opcode");
            assert!(
                payload.len() >= 2,
                "close payload should contain status code"
            );
            let code = u16::from_be_bytes([payload[0], payload[1]]);
            assert_eq!(code, 1000, "close status code should be 1000");
        }
        None => {
            // EOF after close is acceptable — the close was processed and the
            // gateway tore down the connection before we could read the response.
        }
    }

    server_handle.abort();
}

// 14.8: Caller disconnect mid-session triggers upstream Close.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_websocket_caller_disconnect_mid_session() {
    let h = AppHarness::builder().build().await;
    setup_ws_upstream(&h, "ws-caller-drop").await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    ws_handshake(&mut stream, "/oagw/v1/proxy/ws-caller-drop/ws/echo").await;

    // Confirm connection works.
    let text_frame = build_masked_frame(0x1, b"alive");
    tokio::io::AsyncWriteExt::write_all(&mut stream, &text_frame)
        .await
        .unwrap();
    let echo = read_ws_frame(&mut stream).await.unwrap();
    assert_eq!(echo.0, 0x1);
    assert_eq!(echo.1, b"alive");

    // Drop the client connection without sending Close — simulates abrupt disconnect.
    drop(stream);

    // Allow the gateway time to detect the disconnect and clean up.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Verify the gateway didn't crash — open a new connection and perform a
    // successful handshake on a fresh upstream to confirm the server is healthy.
    let mut stream2 = tokio::net::TcpStream::connect(addr).await.unwrap();
    ws_handshake(&mut stream2, "/oagw/v1/proxy/ws-caller-drop/ws/echo").await;
    let frame = build_masked_frame(0x1, b"still alive");
    tokio::io::AsyncWriteExt::write_all(&mut stream2, &frame)
        .await
        .unwrap();
    let echo = read_ws_frame(&mut stream2).await.unwrap();
    assert_eq!(echo.0, 0x1);
    assert_eq!(echo.1, b"still alive");

    server_handle.abort();
}

// 14.9: Connection header with multiple upgrade tokens (e.g. "keep-alive, Upgrade")
// is handled correctly — the WebSocket upgrade is still detected and processed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_websocket_connection_header_multi_value() {
    let h = AppHarness::builder().build().await;
    setup_ws_upstream(&h, "ws-multi-conn").await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();

    // Handshake with a multi-value Connection header ("keep-alive, Upgrade").
    // Uses the retry-capable handshake helper so the bridge-not-ready race
    // (visible under coverage instrumentation) doesn't flake the data frame.
    let req = "GET /oagw/v1/proxy/ws-multi-conn/ws/echo HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Upgrade: websocket\r\n\
         Connection: keep-alive, Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n";
    ws_handshake_to_with_request(&mut stream, req, addr).await;

    // Confirm the connection works — send a text frame and receive echo.
    use tokio::io::AsyncWriteExt;
    let text_frame = build_masked_frame(0x1, b"multi-conn");
    stream.write_all(&text_frame).await.unwrap();
    let echo = read_ws_frame(&mut stream).await.unwrap();
    assert_eq!(echo.0, 0x1);
    assert_eq!(echo.1, b"multi-conn");

    server_handle.abort();
}

// 14.10: Ping/Pong frames forwarded through the gateway end-to-end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_websocket_ping_pong_forwarded() {
    let h = AppHarness::builder().build().await;
    setup_ws_upstream(&h, "ws-pingpong").await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    ws_handshake(&mut stream, "/oagw/v1/proxy/ws-pingpong/ws/echo").await;

    // Send a Ping frame (opcode 0x9). The echo mock may or may not reply
    // with a Pong, but the key assertion is that the frame reaches the
    // upstream without crashing the gateway.
    let ping_frame = build_masked_frame(0x9, b"ping");
    tokio::io::AsyncWriteExt::write_all(&mut stream, &ping_frame)
        .await
        .unwrap();

    // After Ping, verify the connection is still functional by sending a
    // text message and checking the echo.
    let text_frame = build_masked_frame(0x1, b"after-ping");
    tokio::io::AsyncWriteExt::write_all(&mut stream, &text_frame)
        .await
        .unwrap();

    // Read frames until we get the text echo (may have a Pong in between).
    loop {
        let frame = read_ws_frame(&mut stream).await;
        match frame {
            Some((0x1, data)) => {
                assert_eq!(data, b"after-ping");
                break;
            }
            Some((0xA, _)) => continue, // Pong — skip
            Some((0x9, _)) => continue, // Ping from upstream — skip
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    server_handle.abort();
}

// 14.11: Binary frames preserve opcode through the gateway end-to-end.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_websocket_binary_frame_opcode_preserved() {
    let h = AppHarness::builder().build().await;
    setup_ws_upstream(&h, "ws-binary-op").await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    ws_handshake(&mut stream, "/oagw/v1/proxy/ws-binary-op/ws/echo").await;

    // Send a binary frame (opcode 0x2).
    let payload: Vec<u8> = (0..=255).collect();
    let binary_frame = build_masked_frame(0x2, &payload);
    tokio::io::AsyncWriteExt::write_all(&mut stream, &binary_frame)
        .await
        .unwrap();

    // Echo should come back as binary (opcode 0x2), not text.
    let echo = read_ws_frame(&mut stream).await.unwrap();
    assert_eq!(
        echo.0, 0x2,
        "expected binary opcode 0x2, got 0x{:x}",
        echo.0
    );
    assert_eq!(echo.1, payload);

    server_handle.abort();
}

// 14.12: Sustained multi-message session with extended-length payloads.
// Sends 60 frames of varying sizes (including 2-byte and 8-byte extended length
// encodings) through the full Axum→engine→mock→relay→client stack, exercising
// the relay loop's long-running behavior.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_websocket_sustained_multi_message_extended_lengths() {
    let h = AppHarness::builder().build().await;
    setup_ws_upstream(&h, "ws-sustained").await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    ws_handshake(&mut stream, "/oagw/v1/proxy/ws-sustained/ws/echo").await;

    // Mix of payload sizes:
    // - tiny (< 126 bytes, 1-byte length encoding)
    // - medium (200–499 bytes, 2-byte extended length encoding)
    // - large (30 KiB, 2-byte extended length — comfortably within the
    //   65 KiB DuplexStream bridge buffer; 8-byte encoding is covered by
    //   unit tests in websocket.rs::extended_length_payloads)
    let sizes: Vec<usize> = (0..60)
        .map(|i| match i % 3 {
            0 => 10 + (i * 7) % 100,   // tiny: 10–109 bytes
            1 => 200 + (i * 13) % 300, // medium: 200–499 bytes
            _ => 30_000,               // large: 30 KiB
        })
        .collect();

    for (i, &size) in sizes.iter().enumerate() {
        // Alternate text and binary to exercise opcode preservation.
        // Text frames must contain valid UTF-8 (the mock's axum WebSocket
        // validates this), so use ASCII-printable chars for text and raw
        // bytes for binary.
        let opcode = if i % 2 == 0 { 0x1 } else { 0x2 };
        let payload: Vec<u8> = if opcode == 0x1 {
            // Valid ASCII text: cycle through printable chars (0x20–0x7E).
            (0..size).map(|j| (0x20 + ((i + j) % 95)) as u8).collect()
        } else {
            // Raw binary: full byte range.
            (0..size).map(|j| ((i + j) % 256) as u8).collect()
        };
        let frame = build_masked_frame(opcode, &payload);
        tokio::io::AsyncWriteExt::write_all(&mut stream, &frame)
            .await
            .unwrap();

        let echo = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_ws_frame(&mut stream),
        )
        .await
        .unwrap_or_else(|_| panic!("timeout waiting for echo of frame {i} (size {size})"));
        let (echo_op, echo_data) = echo.unwrap_or_else(|| panic!("EOF on frame {i}"));

        assert_eq!(
            echo_op, opcode,
            "frame {i}: opcode mismatch (expected 0x{opcode:x}, got 0x{echo_op:x})"
        );
        assert_eq!(echo_data.len(), size, "frame {i}: payload length mismatch");
        assert_eq!(
            echo_data, payload,
            "frame {i}: payload content mismatch (size {size})"
        );
    }

    server_handle.abort();
}

// 14.13: JSON-structured payload with multi-byte UTF-8 survives the proxy.
// Sends a realistic JSON message containing emoji, CJK, combining characters,
// and 4-byte UTF-8 sequences to verify the frame relay doesn't corrupt
// multi-byte boundaries.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_websocket_json_utf8_payload_integrity() {
    let h = AppHarness::builder().build().await;
    setup_ws_upstream(&h, "ws-json-utf8").await;
    let (addr, server_handle) = start_oagw_server(&h).await;

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    ws_handshake(&mut stream, "/oagw/v1/proxy/ws-json-utf8/ws/echo").await;

    // Realistic JSON payloads with multi-byte UTF-8.
    let payloads = [
        // OpenAI Realtime-style event with emoji and CJK
        r#"{"type":"response.audio_transcript.delta","event_id":"evt_001","delta":"こんにちは世界 🌍🔥 café résumé naïve"}"#,
        // 4-byte UTF-8: Mathematical Bold Script (U+1D4D0–U+1D503) and emoji
        r#"{"message":"𝓗𝓮𝓵𝓵𝓸 from 𝕋𝕖𝕤𝕥","emoji":"👨‍👩‍👧‍👦🏳️‍🌈","nulls":null,"nested":{"flag":true}}"#,
        // Combining characters and zero-width joiners
        r#"{"text":"e\u0301 n\u0303 o\u0308","zwj":"👩\u200d🔬","count":42}"#,
        // Large realistic message: chat completion chunk with mixed scripts
        r#"{"id":"chatcmpl-abc123","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4","choices":[{"index":0,"delta":{"content":"Привет мир • مرحبا بالعالم • 你好世界"},"finish_reason":null}]}"#,
    ];

    for (i, payload_str) in payloads.iter().enumerate() {
        let payload = payload_str.as_bytes();
        let frame = build_masked_frame(0x1, payload);
        tokio::io::AsyncWriteExt::write_all(&mut stream, &frame)
            .await
            .unwrap();

        let echo = read_ws_frame(&mut stream)
            .await
            .unwrap_or_else(|| panic!("EOF on JSON payload {i}"));
        assert_eq!(echo.0, 0x1, "payload {i}: expected text opcode");
        assert_eq!(
            echo.1, payload,
            "payload {i}: byte-level mismatch — UTF-8 corruption through proxy"
        );

        // Also validate that the echoed bytes are valid UTF-8 and round-trip
        // through serde_json without loss.
        let echoed_str = std::str::from_utf8(&echo.1).expect("echoed payload is not valid UTF-8");
        let parsed: serde_json::Value =
            serde_json::from_str(echoed_str).expect("echoed payload is not valid JSON");
        let original: serde_json::Value =
            serde_json::from_str(payload_str).expect("original payload is not valid JSON");
        assert_eq!(
            parsed, original,
            "payload {i}: JSON semantic mismatch after proxy round-trip"
        );
    }

    server_handle.abort();
}

// Response header rules: set/add/remove operations applied to upstream response.
#[tokio::test]
async fn proxy_response_header_rules_applied() {
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
            .alias("resp-rules-test")
            .headers(HeadersConfig {
                request: None,
                response: Some(ResponseHeaderRules {
                    set: [("x-custom-safe".into(), "overwritten".into())]
                        .into_iter()
                        .collect(),
                    add: [("x-injected".into(), "added-value".into())]
                        .into_iter()
                        .collect(),
                    remove: vec!["x-remove-target".into()],
                }),
            })
            .build(),
        )
        .await
        .unwrap();

    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/response-headers".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    let req = http::Request::builder()
        .method(Method::GET)
        .uri("/resp-rules-test/response-headers")
        .body(Body::Empty)
        .unwrap();
    let response = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let headers = response.headers();

    // "set" overwrites existing header from upstream mock.
    assert_eq!(
        headers.get("x-custom-safe").unwrap(),
        "overwritten",
        "response set rule should overwrite upstream header"
    );

    // "add" injects a new header.
    assert_eq!(
        headers.get("x-injected").unwrap(),
        "added-value",
        "response add rule should inject header"
    );
}

// Verify that ws_handshake retries when the bridge sends Close before the
// readiness Pong. This deterministically reproduces the CI race condition
// where the spawned bridge task hasn't started reading frames yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proxy_websocket_handshake_retries_on_early_close() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let attempt = Arc::new(AtomicUsize::new(0));
    let attempt_clone = attempt.clone();

    // Fake server: first connection gets 101 then an immediate Close 1001;
    // second connection gets a proper WebSocket echo (via the real OAGW).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fake_addr = listener.local_addr().unwrap();

    // Set up a real OAGW server for the successful retry.
    let h = AppHarness::builder().build().await;
    setup_ws_upstream(&h, "ws-retry-test").await;
    let (real_addr, server_handle) = start_oagw_server(&h).await;

    let proxy_handle = tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let n = attempt_clone.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // Drain the client's upgrade request before responding. On
                // Windows, closesocket() with unread data in the recv buffer
                // forces a TCP RST, which races the client's read of the 101
                // response and surfaces as `read error / unexpected EOF`
                // (WSAECONNABORTED 10053). Linux delivers buffered bytes
                // before signaling the error, so the test passes there even
                // without the drain.
                let mut drain = [0u8; 1024];
                let mut req = Vec::new();
                loop {
                    let nread = sock.read(&mut drain).await.unwrap_or(0);
                    if nread == 0 {
                        break;
                    }
                    req.extend_from_slice(&drain[..nread]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }

                // First connection: send 101 then Close 1001 (simulates bridge not ready).
                let resp = "HTTP/1.1 101 Switching Protocols\r\n\
                            Upgrade: websocket\r\n\
                            Connection: Upgrade\r\n\
                            Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\
                            \r\n";
                sock.write_all(resp.as_bytes()).await.unwrap();
                // Send unmasked Close frame: opcode 0x8, payload = 1001 + "Going Away"
                let close_payload = {
                    let mut p = 1001u16.to_be_bytes().to_vec();
                    p.extend_from_slice(b"Going Away");
                    p
                };
                let mut frame = vec![0x88]; // FIN + Close
                frame.push(close_payload.len() as u8); // no mask (server→client)
                frame.extend_from_slice(&close_payload);
                sock.write_all(&frame).await.unwrap();
                sock.shutdown().await.ok();
                // Linger until the client closes so the kernel delivers the
                // 101 + Close bytes before this socket is dropped (Windows).
                // Loop to EOF so any trailing bytes the client sends (e.g. a
                // Close echo) are consumed — a single read could return early
                // and leave data in the recv buffer, re-triggering the RST.
                loop {
                    match sock.read(&mut drain).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => continue,
                    }
                }
            } else {
                // Subsequent connections: proxy to the real OAGW server.
                let mut upstream = tokio::net::TcpStream::connect(real_addr).await.unwrap();
                let (mut ur, mut uw) = upstream.split();
                let (mut sr, mut sw) = sock.split();
                tokio::select! {
                    _ = tokio::io::copy(&mut sr, &mut uw) => {}
                    _ = tokio::io::copy(&mut ur, &mut sw) => {}
                }
            }
        }
    });

    let mut stream = tokio::net::TcpStream::connect(fake_addr).await.unwrap();
    ws_handshake(&mut stream, "/oagw/v1/proxy/ws-retry-test/ws/echo").await;

    // If we got here, the retry succeeded. Verify the connection works.
    let frame = build_masked_frame(0x1, b"retry-ok");
    tokio::io::AsyncWriteExt::write_all(&mut stream, &frame)
        .await
        .unwrap();
    let echo = read_ws_frame(&mut stream).await.expect("echo after retry");
    assert_eq!(echo.0, 0x1);
    assert_eq!(echo.1, b"retry-ok");

    // Confirm at least 2 connection attempts were made.
    assert!(
        attempt.load(Ordering::SeqCst) >= 2,
        "expected at least 2 attempts, got {}",
        attempt.load(Ordering::SeqCst),
    );

    proxy_handle.abort();
    server_handle.abort();
}

// ---------------------------------------------------------------------------
// Header boundaries (#4982)
// ---------------------------------------------------------------------------

/// An upstream on the mock with one POST route at `guard.path(path)`.
async fn header_boundary_upstream(
    h: &AppHarness,
    guard: &MockGuard,
    alias: &str,
    path: &str,
    headers: Option<HeadersConfig>,
    plugins: Option<PluginsConfig>,
    auth: Option<oagw_sdk::AuthConfig>,
) {
    let ctx = h.security_context().clone();
    let mut builder = CreateUpstreamRequest::builder(
        Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Http,
                host: "127.0.0.1".into(),
                port: h.mock_port(),
            }],
        },
        HTTP_PROTOCOL_ID,
    )
    .alias(alias);
    if let Some(headers) = headers {
        builder = builder.headers(headers);
    }
    if let Some(plugins) = plugins {
        builder = builder.plugins(plugins);
    }
    if let Some(auth) = auth {
        builder = builder.auth(auth);
    }
    let upstream = h
        .facade()
        .create_upstream(ctx.clone(), builder.build())
        .await
        .unwrap();
    h.facade()
        .create_route(
            ctx,
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: guard.path(path),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();
}

fn ok_response() -> MockResponse {
    MockResponse {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: MockBody::Json(json!({"ok": true})),
    }
}

/// Assert `err` is the required-headers guard's rejection.
fn assert_required_header_missing(err: CanonicalError) {
    match err {
        CanonicalError::InvalidArgument { ctx, .. } => match ctx {
            toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations } => {
                assert_eq!(
                    field_violations.first().map(|v| v.reason.as_str()),
                    Some("REQUIRED_HEADER_MISSING")
                );
            }
            other => panic!("expected FieldViolations, got {other:?}"),
        },
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

fn required_header_guard(header: &str) -> PluginsConfig {
    PluginsConfig {
        sharing: SharingMode::Private,
        items: vec![PluginBinding {
            plugin_ref: REQUIRED_HEADERS_GUARD_PLUGIN_ID.to_string(),
            config: [("required_request_headers".into(), header.into())].into(),
        }],
    }
}

/// Guards check the client's headers: under the default `passthrough: none`
/// the outbound map does not carry `x-correlation-id`, but the client sent it.
#[tokio::test]
async fn proxy_guard_checks_client_headers_under_default_passthrough() {
    let mut guard = MockGuard::new();
    guard.mock("POST", "/guard-inbound", ok_response());
    let h = AppHarness::builder().build().await;
    header_boundary_upstream(
        &h,
        &guard,
        "guard-inbound",
        "/guard-inbound",
        None,
        Some(required_header_guard("x-correlation-id")),
        None,
    )
    .await;

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/guard-inbound{}", guard.path("/guard-inbound")))
        .header("x-correlation-id", "test-123")
        .body(Body::Empty)
        .unwrap();
    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Without the header the guard rejects: it is bound and runs.
    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/guard-inbound{}", guard.path("/guard-inbound")))
        .body(Body::Empty)
        .unwrap();
    let err = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .expect_err("the guard must reject a request without the header");
    assert_required_header_missing(err);
}

/// The credential the auth plugin injects does not satisfy a guard that
/// requires the client to send `authorization`.
#[tokio::test]
async fn proxy_guard_does_not_see_injected_credential() {
    let mut guard = MockGuard::new();
    guard.mock("POST", "/guard-cred", ok_response());
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://guard-key".into(), "sk-guard".into())])
        .build()
        .await;
    header_boundary_upstream(
        &h,
        &guard,
        "guard-cred",
        "/guard-cred",
        None,
        Some(required_header_guard("authorization")),
        Some(oagw_sdk::AuthConfig {
            plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Private,
            config: Some(
                [
                    ("header".into(), "authorization".into()),
                    ("prefix".into(), "Bearer ".into()),
                    ("secret_ref".into(), "cred://guard-key".into()),
                ]
                .into_iter()
                .collect(),
            ),
        }),
    )
    .await;

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/guard-cred{}", guard.path("/guard-cred")))
        .body(Body::Empty)
        .unwrap();
    let err = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .expect_err("the guard must reject: the client sent no authorization");
    assert_required_header_missing(err);
    assert!(guard.recorded_requests().await.is_empty());
}

/// Client-sent forwarding headers do not reach the upstream under
/// `passthrough: all`, but do when the allowlist names them.
#[tokio::test]
async fn proxy_strips_client_forwarding_headers() {
    let mut guard = MockGuard::new();
    guard.mock("POST", "/fwd-all", ok_response());
    guard.mock("POST", "/fwd-allow", ok_response());
    let h = AppHarness::builder().build().await;
    let passthrough = |passthrough, allowlist: Vec<String>| {
        Some(HeadersConfig {
            request: Some(RequestHeaderRules {
                passthrough,
                passthrough_allowlist: allowlist,
                ..Default::default()
            }),
            response: None,
        })
    };
    header_boundary_upstream(
        &h,
        &guard,
        "fwd-all",
        "/fwd-all",
        passthrough(PassthroughMode::All, vec![]),
        None,
        None,
    )
    .await;
    header_boundary_upstream(
        &h,
        &guard,
        "fwd-allow",
        "/fwd-allow",
        passthrough(
            PassthroughMode::Allowlist,
            vec!["x-forwarded-for".into(), "x-custom".into()],
        ),
        None,
        None,
    )
    .await;

    for alias in ["fwd-all", "fwd-allow"] {
        let req = http::Request::builder()
            .method(Method::POST)
            .uri(format!("/{alias}{}", guard.path(&format!("/{alias}"))))
            .header("x-forwarded-for", "203.0.113.7")
            .header("x-forwarded-host", "evil.example")
            .header("x-real-ip", "203.0.113.8")
            .header("forwarded", "for=203.0.113.9;host=internal")
            .header("x-client-ip", "203.0.113.10")
            .header("x-custom", "keep")
            .body(Body::Empty)
            .unwrap();
        let response = h
            .facade()
            .proxy_request(h.security_context().clone(), req)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let recorded = guard.recorded_requests().await;
    let header = |alias: &str, name: &str| {
        recorded
            .iter()
            .find(|r| r.uri.ends_with(alias))
            .expect("request recorded")
            .headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(header("/fwd-all", "x-forwarded-for"), None);
    assert_eq!(header("/fwd-all", "x-forwarded-host"), None);
    assert_eq!(header("/fwd-all", "x-real-ip"), None);
    assert_eq!(header("/fwd-all", "forwarded"), None);
    assert_eq!(header("/fwd-all", "x-client-ip"), None);
    assert_eq!(header("/fwd-all", "x-custom").as_deref(), Some("keep"));
    assert_eq!(
        header("/fwd-allow", "x-forwarded-for").as_deref(),
        Some("203.0.113.7")
    );
    assert_eq!(header("/fwd-allow", "x-forwarded-host"), None);
}

/// Hop-by-hop headers an operator header rule sets do not reach the upstream:
/// OAGW strips them again after the header rules and transforms ran, and does
/// not rely on the engine for `Trailer` (#4982, scenario 11.3).
#[tokio::test]
async fn proxy_drops_hop_by_hop_headers_set_by_header_rules() {
    let mut guard = MockGuard::new();
    guard.mock("POST", "/hop-rules", ok_response());
    let h = AppHarness::builder().build().await;
    header_boundary_upstream(
        &h,
        &guard,
        "hop-rules",
        "/hop-rules",
        Some(HeadersConfig {
            request: Some(RequestHeaderRules {
                set: [
                    ("keep-alive".to_owned(), "timeout=5".to_owned()),
                    ("proxy-connection".to_owned(), "keep-alive".to_owned()),
                    ("trailer".to_owned(), "x-checksum".to_owned()),
                    ("x-kept".to_owned(), "1".to_owned()),
                ]
                .into(),
                ..Default::default()
            }),
            response: None,
        }),
        None,
        None,
    )
    .await;

    let req = http::Request::builder()
        .method(Method::POST)
        .uri(format!("/hop-rules{}", guard.path("/hop-rules")))
        .body(Body::Empty)
        .unwrap();
    let response = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let recorded = guard.recorded_requests().await;
    let names: Vec<&str> = recorded[0]
        .headers
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert!(!names.contains(&"keep-alive"), "{names:?}");
    assert!(!names.contains(&"proxy-connection"), "{names:?}");
    assert!(!names.contains(&"trailer"), "{names:?}");
    assert!(names.contains(&"x-kept"), "{names:?}");
}

/// A changed rate limit takes effect at once: the update drops the old
/// bucket in the limiter the proxy enforces (the Control Plane and the Data
/// Plane share it), so an exhausted caller is admitted again. Only a field
/// the limiter does not compare changes, so the limiter's own rebuild on a
/// capacity change cannot be what admits the request.
#[tokio::test]
async fn changing_a_rate_limit_drops_the_exhausted_bucket() {
    let h = AppHarness::builder().build().await;
    let ctx = h.security_context().clone();
    let limit = |response_headers| RateLimitConfig {
        sharing: SharingMode::Private,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 1,
            window: Window::Minute,
        },
        burst: Some(BurstConfig { capacity: 1 }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers,
        budget: None,
    };
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
            .alias("rate-raised")
            .rate_limit(limit(true))
            .build(),
        )
        .await
        .unwrap();
    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/v1/models".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Append,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();
    let get = || {
        http::Request::builder()
            .method(Method::GET)
            .uri("/rate-raised/v1/models")
            .body(Body::Empty)
            .unwrap()
    };

    h.facade().proxy_request(ctx.clone(), get()).await.unwrap();
    assert!(matches!(
        h.facade().proxy_request(ctx.clone(), get()).await,
        Err(CanonicalError::ResourceExhausted { .. })
    ));

    let update =
        oagw_sdk::UpdateUpstreamRequest::builder(upstream.server.clone(), HTTP_PROTOCOL_ID)
            .alias(upstream.alias.clone())
            .rate_limit(limit(false))
            .build();
    h.facade()
        .update_upstream(ctx.clone(), upstream.id, update)
        .await
        .unwrap();
    let response = h.facade().proxy_request(ctx.clone(), get()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// Calling two routes in turn does not refill the upstream's bucket: each
/// route's limit is checked on the route's bucket, and the upstream bucket
/// keeps the upstream's limit whichever route matched.
#[tokio::test]
async fn alternating_routes_do_not_refill_the_upstream_bucket() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/v1/embeddings",
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"object": "list", "data": []})),
        },
    );
    let h = AppHarness::builder().build().await;
    let ctx = h.security_context().clone();
    let limit = |rate, window| RateLimitConfig {
        sharing: SharingMode::Inherit,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate { rate, window },
        burst: None,
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: true,
        budget: None,
    };
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
            .alias("rl-alternate")
            .rate_limit(limit(2, Window::Minute))
            .build(),
        )
        .await
        .unwrap();
    // Two route limits stricter per second than the upstream's, with
    // different capacities: merged into the upstream's limit they used to
    // reconfigure (and refill) its bucket on every switch.
    let second_path = guard.path("/v1/embeddings");
    for (path, rate) in [("/v1/models".to_owned(), 10), (second_path.clone(), 20)] {
        h.facade()
            .create_route(
                ctx.clone(),
                CreateRouteRequest::builder(
                    upstream.id,
                    MatchRules {
                        http: Some(HttpMatch {
                            methods: vec![HttpMethod::Get],
                            path,
                            query_allowlist: vec![],
                            path_suffix_mode: PathSuffixMode::Append,
                        }),
                        grpc: None,
                    },
                )
                .rate_limit(limit(rate, Window::Hour))
                .build(),
            )
            .await
            .unwrap();
    }

    let mut admitted = Vec::new();
    for path in ["/v1/models", second_path.as_str(), "/v1/models"] {
        let req = http::Request::builder()
            .method(Method::GET)
            .uri(format!("/rl-alternate{path}"))
            .body(Body::Empty)
            .unwrap();
        admitted.push(match h.facade().proxy_request(ctx.clone(), req).await {
            Ok(resp) => resp.status() == StatusCode::OK,
            Err(CanonicalError::ResourceExhausted { .. }) => false,
            Err(err) => panic!("unexpected error: {err:?}"),
        });
    }
    assert_eq!(admitted, [true, true, false]);
}

// -- Circuit breaker (#4974) --

/// A breaker that opens after two failures, stays open `timeout_seconds` and
/// closes on one successful probe.
fn breaker(timeout_seconds: u32) -> serde_json::Value {
    json!({
        "failure_threshold": 2,
        "timeout_seconds": timeout_seconds,
        "success_threshold": 1
    })
}

/// An upstream at `endpoint` with `circuit_breaker`, and `GET /status` and
/// `GET /ws/echo` routes. `alias` only for an IP endpoint: a host name is
/// its own. Returns the upstream's JSON body.
async fn breaker_upstream_at(
    h: &AppHarness,
    alias: Option<&str>,
    endpoint: serde_json::Value,
    circuit_breaker: serde_json::Value,
) -> serde_json::Value {
    let mut body = json!({
        "server": {"endpoints": [endpoint]},
        "protocol": HTTP_PROTOCOL_ID,
        "circuit_breaker": circuit_breaker
    });
    if let Some(alias) = alias {
        body["alias"] = json!(alias);
    }
    let upstream = h
        .api_v1()
        .post_upstream()
        .with_body(body)
        .expect_status(201)
        .await
        .json();
    for path in ["/status", "/ws/echo"] {
        h.api_v1()
            .post_route()
            .with_body(json!({
                "upstream_id": upstream["id"],
                "match": {"http": {"methods": ["GET"], "path": path}}
            }))
            .expect_status(201)
            .await;
    }
    upstream
}

/// [`breaker_upstream_at`] the mock, with [`breaker`].
async fn circuit_breaker_upstream(
    h: &AppHarness,
    alias: &str,
    timeout_seconds: u32,
) -> serde_json::Value {
    breaker_upstream_at(
        h,
        Some(alias),
        json!({"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}),
        breaker(timeout_seconds),
    )
    .await
}

/// The answer of the upstream, not of the gateway.
fn assert_from_upstream(resp: &oagw::test_support::TestResponse, status: u16) {
    resp.assert_status(status);
    assert_eq!(resp.headers()["x-oagw-error-source"], "upstream");
}

/// The gateway's error with its own retry hint, not the breaker's.
fn assert_gateway_error(resp: &oagw::test_support::TestResponse, status: u16, retry_after: &str) {
    resp.assert_status(status);
    assert_eq!(resp.headers()["x-oagw-error-source"], "gateway");
    assert_eq!(resp.headers()["retry-after"], retry_after);
}

/// The gateway's fail-fast answer while the circuit is open, `retry_after`
/// seconds before a probe.
fn assert_circuit_open(resp: &oagw::test_support::TestResponse, retry_after: &str) {
    resp.assert_status(503);
    assert_eq!(resp.headers()["x-oagw-error-source"], "gateway");
    assert_eq!(resp.headers()["retry-after"], retry_after);
    let body = resp.json();
    assert_eq!(body["status"], 503, "{body}");
    assert!(
        body["type"]
            .as_str()
            .is_some_and(|t| t.contains("service_unavailable")),
        "{body}"
    );
}

/// Consecutive upstream 503s open the circuit: the next request fails fast
/// with 503 and `Retry-After` without reaching the upstream. After the
/// timeout one probe goes through; its success closes the circuit, and a
/// single failure no longer opens it.
#[tokio::test]
async fn circuit_breaker_opens_then_a_probe_closes_it() {
    let h = AppHarness::builder().build().await;
    let upstream = circuit_breaker_upstream(&h, "cb-open", 1).await;
    assert_eq!(
        upstream["circuit_breaker"],
        json!({
            "sharing": "private",
            "enabled": true,
            "failure_threshold": 2,
            "timeout_seconds": 1,
            "success_threshold": 1
        })
    );
    let get = |code: u16| {
        h.api_v1()
            .proxy_get("cb-open", &format!("status/{code}"))
            .send()
    };

    // A success in between resets the count.
    assert_from_upstream(&get(503).await, 503);
    get(200).await.assert_status(200);
    assert_from_upstream(&get(502).await, 502);
    assert_from_upstream(&get(504).await, 504);
    assert_circuit_open(&get(200).await, "1");

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    get(200).await.assert_status(200);
    assert_from_upstream(&get(503).await, 503);
    get(200).await.assert_status(200);
}

/// A failed probe opens the circuit again for another timeout.
#[tokio::test]
async fn circuit_breaker_failed_probe_reopens() {
    let h = AppHarness::builder().build().await;
    circuit_breaker_upstream(&h, "cb-reopen", 1).await;
    let get = |code: u16| {
        h.api_v1()
            .proxy_get("cb-reopen", &format!("status/{code}"))
            .send()
    };
    assert_from_upstream(&get(503).await, 503);
    assert_from_upstream(&get(503).await, 503);
    assert_circuit_open(&get(200).await, "1");

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert_from_upstream(&get(503).await, 503);
    assert_circuit_open(&get(200).await, "1");
}

/// A write that changes the breaker or only the endpoints resets it at
/// once; an unchanged write does not. Nothing here relies on the Control
/// Plane dropping the breaker: the Data Plane rebuilds it on the next
/// request, as another replica, which saw no write, does.
#[tokio::test]
async fn circuit_breaker_resets_on_config_or_endpoint_change() {
    let h = AppHarness::builder().build().await;
    let upstream = circuit_breaker_upstream(&h, "cb-reset", 60).await;
    let id = upstream["id"].as_str().unwrap().to_owned();
    let other = MockUpstream::start().await;
    let get = |code: u16| {
        h.api_v1()
            .proxy_get("cb-reset", &format!("status/{code}"))
            .send()
    };
    let put = |port: u16, failure_threshold: u32| {
        h.api_v1()
            .put_upstream(&id)
            .with_body(json!({
                "server": {
                    "endpoints": [{"host": "127.0.0.1", "port": port, "scheme": "http"}]
                },
                "protocol": HTTP_PROTOCOL_ID,
                "alias": "cb-reset",
                "tags": [],
                "enabled": true,
                "circuit_breaker": {
                    "failure_threshold": failure_threshold,
                    "timeout_seconds": 60,
                    "success_threshold": 1
                }
            }))
            .expect_status(200)
    };
    let open = async || {
        assert_from_upstream(&get(503).await, 503);
        assert_from_upstream(&get(503).await, 503);
        assert_circuit_open(&get(200).await, "60");
    };

    open().await;
    put(h.mock_port(), 2).await;
    assert_circuit_open(&get(200).await, "60");

    // Same breaker, another endpoint.
    put(other.addr().port(), 2).await;
    get(200).await.assert_status(200);

    open().await;
    let updated = put(other.addr().port(), 3).await.json();
    assert_eq!(updated["circuit_breaker"]["failure_threshold"], 3);
    get(200).await.assert_status(200);
}

/// An omitted field takes the ADR-0005 default.
#[tokio::test]
async fn circuit_breaker_defaults() {
    let h = AppHarness::builder().build().await;
    let upstream = breaker_upstream_at(
        &h,
        Some("cb-defaults"),
        json!({"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}),
        json!({}),
    )
    .await;
    assert_eq!(
        upstream["circuit_breaker"],
        json!({
            "sharing": "private",
            "enabled": true,
            "failure_threshold": 5,
            "timeout_seconds": 30,
            "success_threshold": 3
        })
    );
}

/// A misspelled field is refused like any other body that does not
/// deserialize (422), and a value above its maximum is a 400: not a silent
/// default or a breaker that stays open for months.
#[tokio::test]
async fn circuit_breaker_rejects_unknown_fields_and_values_too_large() {
    let h = AppHarness::builder().build().await;
    let post = |circuit_breaker: serde_json::Value| {
        h.api_v1().post_upstream().with_body(json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": HTTP_PROTOCOL_ID,
            "circuit_breaker": circuit_breaker
        }))
    };
    post(json!({"failure_treshold": 1}))
        .expect_status(422)
        .await;
    for (circuit_breaker, detail) in [
        (
            json!({"failure_threshold": 1001}),
            "circuit_breaker.failure_threshold must be at most 1000",
        ),
        (
            json!({"timeout_seconds": 86_401}),
            "circuit_breaker.timeout_seconds must be at most 86400",
        ),
        (
            json!({"success_threshold": 101}),
            "circuit_breaker.success_threshold must be at most 100",
        ),
    ] {
        let body = post(circuit_breaker.clone())
            .expect_status(400)
            .await
            .text();
        assert!(body.contains(detail), "{circuit_breaker}: {body}");
    }
}

/// A free local port nothing listens on.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// An upstream that refuses connections opens the circuit.
#[tokio::test]
async fn circuit_breaker_opens_on_refused_connections() {
    let h = AppHarness::builder().build().await;
    breaker_upstream_at(
        &h,
        Some("cb-refused"),
        json!({"host": "127.0.0.1", "port": closed_port(), "scheme": "http"}),
        breaker(60),
    )
    .await;
    // Refused: 502 with the transient hint.
    let get = || h.api_v1().proxy_get("cb-refused", "status/200").send();
    assert_gateway_error(&get().await, 502, "5");
    assert_gateway_error(&get().await, 502, "5");
    assert_circuit_open(&get().await, "60");
}

/// A host that does not resolve opens the circuit.
#[tokio::test]
async fn circuit_breaker_opens_on_dns_failures() {
    let h = AppHarness::builder().build().await;
    breaker_upstream_at(
        &h,
        None,
        json!({"host": "oagw-cb-test.invalid", "port": 80, "scheme": "http"}),
        breaker(60),
    )
    .await;
    // Link unavailable: 503 with the link hint.
    let get = || {
        h.api_v1()
            .proxy_get("oagw-cb-test.invalid", "status/200")
            .send()
    };
    assert_gateway_error(&get().await, 503, "10");
    assert_gateway_error(&get().await, 503, "10");
    assert_circuit_open(&get().await, "60");
}

/// The SSRF policy refusing the upstream's addresses is the gateway's
/// decision, not an upstream failure: the circuit stays closed.
#[tokio::test]
async fn circuit_breaker_ignores_ssrf_refusals() {
    let h = AppHarness::builder()
        .with_upstream_ssrf_policy()
        .build()
        .await;
    let upstream = breaker_upstream_at(
        &h,
        None,
        json!({"host": "localhost", "port": h.mock_port(), "scheme": "http"}),
        json!({"failure_threshold": 1, "timeout_seconds": 60, "success_threshold": 1}),
    )
    .await;
    let alias = upstream["alias"].as_str().unwrap();
    for _ in 0..3 {
        let resp = h.api_v1().proxy_get(alias, "status/200").send().await;
        assert_gateway_error(&resp, 503, "10");
    }
}

/// A client that trickles its body until the deadline is not an upstream
/// failure: the circuit stays closed for everyone else.
#[tokio::test]
async fn circuit_breaker_ignores_a_slow_upload() {
    use futures_util::StreamExt as _;
    let h = AppHarness::builder()
        .with_request_timeout(std::time::Duration::from_millis(300))
        .build()
        .await;
    let ctx = h.security_context().clone();
    let upstream = breaker_upstream_at(
        &h,
        Some("cb-slow-upload"),
        json!({"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}),
        json!({"failure_threshold": 1, "timeout_seconds": 60, "success_threshold": 1}),
    )
    .await;
    h.api_v1()
        .post_route()
        .with_body(json!({
            "upstream_id": upstream["id"],
            "match": {"http": {"methods": ["POST"], "path": "/echo"}}
        }))
        .expect_status(201)
        .await;
    for _ in 0..2 {
        // One chunk, then nothing until the deadline.
        let chunk: Result<bytes::Bytes, oagw_sdk::body::BoxError> =
            Ok(bytes::Bytes::from_static(b"part"));
        let stream: oagw_sdk::body::BodyStream =
            Box::pin(futures_util::stream::iter([chunk]).chain(futures_util::stream::pending()));
        let req = http::Request::builder()
            .method(Method::POST)
            .uri("/cb-slow-upload/echo")
            .header(http::header::CONTENT_TYPE, "application/octet-stream")
            .body(Body::Stream(stream))
            .unwrap();
        h.facade()
            .proxy_request(ctx.clone(), req)
            .await
            .expect_err("the upload never ends");
    }
    h.api_v1()
        .proxy_get("cb-slow-upload", "status/200")
        .send()
        .await
        .assert_status(200);
}

/// A request pinned with `X-OAGW-Target-Host` to one endpoint of several
/// says nothing about the others: failures on a dead endpoint leave the
/// circuit closed (an open one would answer every request with its own
/// 60 s hint).
#[tokio::test]
async fn circuit_breaker_ignores_requests_pinned_to_one_endpoint() {
    let h = AppHarness::builder().build().await;
    let upstream = h
        .api_v1()
        .post_upstream()
        .with_body(json!({
            "alias": "cb-pinned",
            "server": {"endpoints": [
                {"host": "localhost", "port": h.mock_port(), "scheme": "http"},
                {"host": "oagw-cb-pinned.invalid", "port": h.mock_port(), "scheme": "http"}
            ]},
            "protocol": HTTP_PROTOCOL_ID,
            "circuit_breaker": {"failure_threshold": 1, "timeout_seconds": 60, "success_threshold": 1}
        }))
        .expect_status(201)
        .await
        .json();
    h.api_v1()
        .post_route()
        .with_body(json!({
            "upstream_id": upstream["id"],
            "match": {"http": {"methods": ["GET"], "path": "/status"}}
        }))
        .expect_status(201)
        .await;
    let pinned = |host: &'static str| {
        h.api_v1()
            .proxy_get("cb-pinned", "status/200")
            .with_header(
                http::HeaderName::from_static("x-oagw-target-host"),
                http::HeaderValue::from_static(host),
            )
            .send()
    };
    for _ in 0..3 {
        // Link unavailable, not the open circuit.
        assert_gateway_error(&pinned("oagw-cb-pinned.invalid").await, 503, "10");
    }
}

/// Requests the open circuit rejects take no rate-limit tokens: once a
/// config change closes it, the whole budget is left.
#[tokio::test]
async fn circuit_breaker_rejections_take_no_rate_limit_tokens() {
    let h = AppHarness::builder().build().await;
    let body = |failure_threshold: u32| {
        json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": HTTP_PROTOCOL_ID,
            "alias": "cb-rate",
            "tags": [],
            "enabled": true,
            "rate_limit": {
                "algorithm": "token_bucket",
                "sustained": {"rate": 3, "window": "minute"},
                "burst": {"capacity": 3},
                "scope": "tenant",
                "strategy": "reject"
            },
            "circuit_breaker": {
                "failure_threshold": failure_threshold,
                "timeout_seconds": 60,
                "success_threshold": 1
            }
        })
    };
    let upstream = h
        .api_v1()
        .post_upstream()
        .with_body(body(1))
        .expect_status(201)
        .await
        .json();
    let id = upstream["id"].as_str().unwrap().to_owned();
    h.api_v1()
        .post_route()
        .with_body(json!({
            "upstream_id": upstream["id"],
            "match": {"http": {"methods": ["GET"], "path": "/status"}}
        }))
        .expect_status(201)
        .await;
    let get = |code: u16| {
        h.api_v1()
            .proxy_get("cb-rate", &format!("status/{code}"))
            .send()
    };

    assert_from_upstream(&get(503).await, 503);
    for _ in 0..5 {
        assert_circuit_open(&get(200).await, "60");
    }
    h.api_v1()
        .put_upstream(&id)
        .with_body(body(2))
        .expect_status(200)
        .await;
    get(200).await.assert_status(200);
    get(200).await.assert_status(200);
    get(200).await.assert_status(429);
}

/// A WebSocket upgrade request for `path` on `alias`.
fn ws_upgrade(alias: &str, path: &str) -> http::Request<Body> {
    http::Request::builder()
        .method(Method::GET)
        .uri(format!("/{alias}/{path}"))
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .body(Body::Empty)
        .unwrap()
}

/// A WebSocket handshake counts like a request: the upstream refusing it
/// with 503 is a failure, so two of them open the circuit and the next
/// upgrade fails fast. After the timeout a 101 is a successful probe that
/// closes the circuit. What happens on an open tunnel is not counted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn circuit_breaker_counts_websocket_handshakes() {
    let h = AppHarness::builder().build().await;
    let ctx = h.security_context().clone();
    circuit_breaker_upstream(&h, "cb-ws", 1).await;
    let refused = || ws_upgrade("cb-ws", "status/503");
    let echo = || ws_upgrade("cb-ws", "ws/echo");
    // The refused handshake is a 502 with the transient hint; the open
    // circuit a plain 503 with the breaker's.
    let retry_after = |err: CanonicalError| {
        let status = err.http_status_override();
        match err {
            CanonicalError::ServiceUnavailable { ctx, .. } => (status, ctx.retry_after_seconds),
            other => panic!("expected ServiceUnavailable, got {other:?}"),
        }
    };
    let proxy = |req| h.facade().proxy_request(ctx.clone(), req);
    for _ in 0..2 {
        let err = proxy(refused()).await.expect_err("the upstream refuses");
        assert_eq!(retry_after(err), (Some(502), Some(5)));
    }
    let err = proxy(echo()).await.expect_err("the circuit is open");
    assert_eq!(retry_after(err), (None, Some(1)));

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let tunnel = proxy(echo()).await.expect("the probe switches protocols");
    assert_eq!(tunnel.status(), StatusCode::SWITCHING_PROTOCOLS);

    // Closed: one refusal does not open it, and a tunnel cut off after it
    // does not count as the second failure.
    let err = proxy(refused()).await.expect_err("the upstream refuses");
    assert_eq!(retry_after(err), (Some(502), Some(5)));
    drop(tunnel);
    let tunnel = proxy(echo()).await.expect("the circuit is closed");
    assert_eq!(tunnel.status(), StatusCode::SWITCHING_PROTOCOLS);
}
