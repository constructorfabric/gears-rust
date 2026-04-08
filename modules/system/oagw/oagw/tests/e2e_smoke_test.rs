use std::sync::Arc;
use std::time::Duration;

use oagw::test_support::{
    APIKEY_AUTH_PLUGIN_ID, AppHarness, CapturingAuthZResolverClient, DenyingAuthZResolverClient,
    MockBody, MockGuard, MockResponse,
};

// 10.1: E2E — create upstream, create route, proxy chat completion, verify round-trip.
#[tokio::test]
async fn e2e_chat_completion_round_trip() {
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-e2e-test-key".into())])
        .build()
        .await;

    // Create upstream via Management API.
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-openai",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let upstream_id = resp.json()["id"].as_str().unwrap().to_string();

    // Create route via Management API.
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &upstream_id,
            "match": {
                "http": {
                    "methods": ["POST"],
                    "path": "/v1/chat/completions"
                }
            },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    // Proxy a chat completion request.
    let resp = h
        .api_v1()
        .proxy_post("e2e-openai", "v1/chat/completions")
        .with_body(serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        }))
        .expect_status(200)
        .await;

    let body = resp.json();
    assert!(body.get("id").is_some());
    assert!(body.get("choices").is_some());
}

// 10.2: E2E — SSE streaming round-trip via dynamic MockGuard route.
#[tokio::test]
async fn e2e_sse_streaming() {
    let mut guard = MockGuard::new();

    let chunks: Vec<String> = vec![
        serde_json::json!({"id":"chatcmpl-mock-stream","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}).to_string(),
        serde_json::json!({"id":"chatcmpl-mock-stream","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}).to_string(),
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

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-sse",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    let route_path = guard.path("/v1/chat/completions/stream");
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &uid,
            "match": {
                "http": {
                    "methods": ["POST"],
                    "path": route_path
                }
            },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    // Proxy streaming request via guard-prefixed path.
    let resp = h
        .api_v1()
        .proxy_post("e2e-sse", &route_path[1..])
        .with_body(serde_json::json!({"model": "gpt-4", "stream": true}))
        .expect_status(200)
        .await;

    resp.assert_body_contains("data: [DONE]");
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("text/event-stream"), "content-type: {ct}");
}

// 10.3: E2E — auth injection round-trip.
#[tokio::test]
async fn e2e_auth_injection() {
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-e2e-test-key".into())])
        .build()
        .await;

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-auth",
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
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &uid,
            "match": {
                "http": {
                    "methods": ["POST"],
                    "path": "/echo"
                }
            },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    // Proxy to echo and verify auth header.
    let resp = h
        .api_v1()
        .proxy_post("e2e-auth", "echo")
        .with_body(serde_json::json!({"test": true}))
        .expect_status(200)
        .await;

    let body = resp.json();
    let headers = body["headers"].as_object().unwrap();
    let auth = headers.get("authorization").unwrap().as_str().unwrap();
    assert_eq!(auth, "Bearer sk-e2e-test-key");
}

// 10.4: E2E — error scenarios.
#[tokio::test]
async fn e2e_nonexistent_alias_returns_404() {
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-e2e-test-key".into())])
        .build()
        .await;

    let resp = h
        .api_v1()
        .proxy_get("nonexistent", "v1/test")
        .expect_status(404)
        .await;

    resp.assert_header("x-oagw-error-source", "gateway");
}

#[tokio::test]
async fn e2e_disabled_upstream_returns_503() {
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-e2e-test-key".into())])
        .build()
        .await;

    h.api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-disabled",
            "enabled": false,
            "tags": []
        }))
        .expect_status(201)
        .await;

    h.api_v1()
        .proxy_get("e2e-disabled", "v1/test")
        .expect_status(503)
        .await;
}

#[tokio::test]
async fn e2e_upstream_500_passthrough() {
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-e2e-test-key".into())])
        .build()
        .await;

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-errors",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &uid,
            "match": {
                "http": {
                    "methods": ["GET"],
                    "path": "/error"
                }
            },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    let resp = h
        .api_v1()
        .proxy_get("e2e-errors", "error/500")
        .expect_status(500)
        .await;

    resp.assert_header("x-oagw-error-source", "upstream");
}

// 10.4: E2E — rate limit exceeded.
#[tokio::test]
async fn e2e_rate_limit_returns_429() {
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-e2e-test-key".into())])
        .build()
        .await;

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-rl",
            "enabled": true,
            "tags": [],
            "rate_limit": {
                "algorithm": "token_bucket",
                "sustained": {"rate": 1, "window": "minute"},
                "burst": {"capacity": 1},
                "scope": "tenant",
                "strategy": "reject",
                "cost": 1
            }
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &uid,
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

    // First request succeeds.
    h.api_v1()
        .proxy_get("e2e-rl", "v1/models")
        .expect_status(200)
        .await;

    // Second request should be rate limited.
    h.api_v1()
        .proxy_get("e2e-rl", "v1/models")
        .expect_status(429)
        .await;
}

// 10.5: E2E — management lifecycle.
#[tokio::test]
async fn e2e_management_lifecycle() {
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-e2e-test-key".into())])
        .build()
        .await;

    // Create.
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "lifecycle",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    // List (appears).
    let resp = h.api_v1().list_upstreams().expect_status(200).await;
    let list = resp.json();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"].as_str() == Some(uid.as_str()))
    );

    // Update alias (full replacement via PUT).
    h.api_v1()
        .put_upstream(&uid)
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "lifecycle-v2",
            "enabled": true,
            "tags": []
        }))
        .expect_status(200)
        .await;

    // Get (updated).
    let resp = h.api_v1().get_upstream(&uid).expect_status(200).await;
    assert_eq!(resp.json()["alias"].as_str().unwrap(), "lifecycle-v2");

    // Create route.
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &uid,
            "match": {
                "http": {
                    "methods": ["GET"],
                    "path": "/test"
                }
            },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    // Delete upstream (cascades routes).
    h.api_v1().delete_upstream(&uid).expect_status(204).await;

    // List (gone).
    let resp = h.api_v1().list_upstreams().expect_status(200).await;
    let list = resp.json();
    assert!(
        !list
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"].as_str() == Some(uid.as_str()))
    );
}

// 8.11: Content-Length with non-integer value returns 400.
#[tokio::test]
async fn e2e_invalid_content_length_returns_400() {
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-e2e-test-key".into())])
        .build()
        .await;

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-cl",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &uid,
            "match": {
                "http": {
                    "methods": ["POST"],
                    "path": "/v1/test"
                }
            },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    // Send request with non-integer Content-Length.
    h.api_v1()
        .proxy_post("e2e-cl", "v1/test")
        .with_body(serde_json::json!({"test": true}))
        .with_header(
            http::header::CONTENT_LENGTH,
            http::HeaderValue::from_static("not-a-number"),
        )
        .expect_status(400)
        .await;
}

// 8.11: Content-Length exceeding 100MB returns 413.
#[tokio::test]
async fn e2e_body_exceeding_limit_returns_413() {
    let h = AppHarness::builder()
        .with_credentials(vec![("cred://openai-key".into(), "sk-e2e-test-key".into())])
        .build()
        .await;

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-big",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &uid,
            "match": {
                "http": {
                    "methods": ["POST"],
                    "path": "/v1/test"
                }
            },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    // Send request with Content-Length exceeding 100MB.
    h.api_v1()
        .proxy_post("e2e-big", "v1/test")
        .with_body("small body")
        .with_header(
            http::header::CONTENT_LENGTH,
            http::HeaderValue::from_static("200000000"),
        )
        .expect_status(413)
        .await;
}

// 10.4: E2E — upstream timeout returns 504 via gated mock that never responds.
// Uses multi_thread runtime so the timer driver runs on a dedicated thread,
// preventing stalls when other test binaries compete for CPU.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_upstream_timeout_returns_504() {
    let mut guard = MockGuard::new();
    // Register a gated route that will never respond (sender kept alive but not signaled).
    let _gate = guard.mock_gated(
        "GET",
        "/timeout",
        MockResponse {
            status: 200,
            headers: vec![],
            body: MockBody::Json(serde_json::json!({"ok": true})),
        },
    );

    let h = AppHarness::builder()
        .with_request_timeout(std::time::Duration::from_millis(500))
        .build()
        .await;

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-timeout",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    let route_path = guard.path("/timeout");
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &uid,
            "match": {
                "http": {
                    "methods": ["GET"],
                    "path": route_path
                }
            },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    // Proxy to gated mock — should return 504.
    let resp = h
        .api_v1()
        .proxy_get("e2e-timeout", &route_path[1..])
        .expect_status(504)
        .await;

    resp.assert_header("x-oagw-error-source", "gateway");
}

// 10.11: E2E — proxy request denied by AuthZ returns 403 Forbidden.
#[tokio::test]
async fn e2e_authz_forbidden_returns_403() {
    let h = AppHarness::builder()
        .with_authz_client(Arc::new(DenyingAuthZResolverClient))
        .build()
        .await;

    // Create upstream and route so the denial is purely from AuthZ, not routing.
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-forbidden",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let upstream_id = resp.json()["id"].as_str().unwrap().to_string();

    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
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

    // Proxy request should be denied before reaching the upstream.
    let resp = h
        .api_v1()
        .proxy_get("e2e-forbidden", "v1/models")
        .expect_status(403)
        .await;

    resp.assert_header("x-oagw-error-source", "gateway");

    let body = resp.json();
    assert_eq!(body["status"], 403);
    assert_eq!(body["title"], "Forbidden");
    assert_eq!(
        body["type"],
        "gts.x.core.errors.err.v1~x.oagw.authz.forbidden.v1"
    );
}

// 10.12: E2E — proxy authz evaluation request carries caller's tenant context.
#[tokio::test]
async fn e2e_authz_request_carries_tenant_context() {
    let capturing = Arc::new(CapturingAuthZResolverClient::new());

    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/v1/test",
        MockResponse {
            status: 200,
            headers: vec![],
            body: MockBody::Json(serde_json::json!({"ok": true})),
        },
    );

    let h = AppHarness::builder()
        .with_authz_client(capturing.clone())
        .build()
        .await;

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-authz-ctx",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let upstream_id = resp.json()["id"].as_str().unwrap().to_string();

    let route_path = guard.path("/v1/test");
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &upstream_id,
            "match": {
                "http": {
                    "methods": ["GET"],
                    "path": route_path
                }
            },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    h.api_v1()
        .proxy_get("e2e-authz-ctx", &route_path[1..])
        .expect_status(200)
        .await;

    let requests = capturing.recorded();
    assert!(
        !requests.is_empty(),
        "expected at least one captured evaluation request"
    );

    let req = &requests[0];
    let tenant_ctx = req
        .context
        .tenant_context
        .as_ref()
        .expect("expected tenant_context in evaluation request");
    assert_eq!(
        tenant_ctx.root_id,
        Some(h.security_context().subject_tenant_id()),
        "tenant_context.root_id should match subject_tenant_id"
    );
    assert_eq!(req.resource.resource_type, "gts.x.core.oagw.proxy.v1~");
    assert_eq!(req.action.name, "invoke");
}

// ---------------------------------------------------------------------------
// Rate-limit response header tests (Scenario 18.1, 18.1.1, ADR-0004)
// ---------------------------------------------------------------------------

/// Helper: create upstream + route with the given rate_limit JSON, returning the alias for proxy calls.
async fn setup_rate_limited_upstream(
    h: &AppHarness,
    alias: &str,
    rate_limit: serde_json::Value,
    route_path: &str,
) -> String {
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": alias,
            "enabled": true,
            "tags": [],
            "rate_limit": rate_limit
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &uid,
            "match": { "http": { "methods": ["GET"], "path": route_path } },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    uid
}

// Scenario 18.1, inst-rl-12, dod-rate-limit-headers — success response includes X-RateLimit-* headers.
#[tokio::test]
async fn e2e_rate_limit_success_includes_response_headers() {
    let h = AppHarness::builder().build().await;

    setup_rate_limited_upstream(
        &h,
        "e2e-rl-headers",
        serde_json::json!({
            "algorithm": "token_bucket",
            "sustained": {"rate": 10, "window": "second"},
            "burst": {"capacity": 10},
            "scope": "tenant",
            "strategy": "reject",
            "cost": 1,
            "response_headers": true
        }),
        "/v1/models",
    )
    .await;

    let resp = h
        .api_v1()
        .proxy_get("e2e-rl-headers", "v1/models")
        .expect_status(200)
        .await;

    resp.assert_header("x-ratelimit-limit", "10");
    resp.assert_header("x-ratelimit-remaining", "9");
    assert!(
        resp.headers().get("x-ratelimit-reset").is_some(),
        "expected x-ratelimit-reset header on success"
    );
    assert!(
        resp.headers().get("retry-after").is_none(),
        "retry-after should not be present on success"
    );
}

// Scenario 18.1, inst-rl-6a1, dod-rate-limit-headers, dod-token-bucket — 429 includes all required headers.
#[tokio::test]
async fn e2e_rate_limit_429_includes_all_headers() {
    let h = AppHarness::builder().build().await;

    setup_rate_limited_upstream(
        &h,
        "e2e-rl-429h",
        serde_json::json!({
            "algorithm": "token_bucket",
            "sustained": {"rate": 1, "window": "minute"},
            "burst": {"capacity": 1},
            "scope": "tenant",
            "strategy": "reject",
            "cost": 1,
            "response_headers": true
        }),
        "/v1/models",
    )
    .await;

    // Exhaust the bucket.
    h.api_v1()
        .proxy_get("e2e-rl-429h", "v1/models")
        .expect_status(200)
        .await;

    // Second request triggers 429.
    let resp = h
        .api_v1()
        .proxy_get("e2e-rl-429h", "v1/models")
        .expect_status(429)
        .await;

    resp.assert_header("x-oagw-error-source", "gateway");
    assert!(
        resp.headers().get("retry-after").is_some(),
        "expected retry-after header on 429"
    );
    let retry_after: u64 = resp
        .headers()
        .get("retry-after")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .expect("retry-after should be a positive integer");
    assert!(retry_after > 0, "retry-after should be > 0");

    resp.assert_header("x-ratelimit-limit", "1");
    resp.assert_header("x-ratelimit-remaining", "0");
    assert!(
        resp.headers().get("x-ratelimit-reset").is_some(),
        "expected x-ratelimit-reset on 429"
    );

    // RFC 9457 Problem Details body.
    assert!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("application/problem+json"),
        "expected application/problem+json content type"
    );
    let body = resp.json();
    assert_eq!(body["status"], 429);
    assert!(
        body.get("type").is_some(),
        "expected 'type' in problem JSON"
    );
    assert!(
        body.get("detail").is_some(),
        "expected 'detail' in problem JSON"
    );
    assert!(
        body.get("instance").is_some(),
        "expected 'instance' in problem JSON"
    );
}

// Scenario 18.1.1, inst-rl-12 (negative) — response_headers=false suppresses headers on success.
#[tokio::test]
async fn e2e_rate_limit_response_headers_disabled_on_success() {
    let h = AppHarness::builder().build().await;

    setup_rate_limited_upstream(
        &h,
        "e2e-rl-nohdr",
        serde_json::json!({
            "algorithm": "token_bucket",
            "sustained": {"rate": 5, "window": "second"},
            "burst": {"capacity": 5},
            "scope": "tenant",
            "strategy": "reject",
            "cost": 1,
            "response_headers": false
        }),
        "/v1/models",
    )
    .await;

    let resp = h
        .api_v1()
        .proxy_get("e2e-rl-nohdr", "v1/models")
        .expect_status(200)
        .await;

    assert!(
        resp.headers().get("x-ratelimit-limit").is_none(),
        "x-ratelimit-limit should be absent when response_headers=false"
    );
    assert!(
        resp.headers().get("x-ratelimit-remaining").is_none(),
        "x-ratelimit-remaining should be absent when response_headers=false"
    );
    assert!(
        resp.headers().get("x-ratelimit-reset").is_none(),
        "x-ratelimit-reset should be absent when response_headers=false"
    );
}

// Scenario 18.1.1, inst-rl-6a1 — 429 still includes retry-after even when response_headers=false.
#[tokio::test]
async fn e2e_rate_limit_headers_disabled_429_still_has_retry_after() {
    let h = AppHarness::builder().build().await;

    setup_rate_limited_upstream(
        &h,
        "e2e-rl-nohdr429",
        serde_json::json!({
            "algorithm": "token_bucket",
            "sustained": {"rate": 1, "window": "minute"},
            "burst": {"capacity": 1},
            "scope": "tenant",
            "strategy": "reject",
            "cost": 1,
            "response_headers": false
        }),
        "/v1/models",
    )
    .await;

    h.api_v1()
        .proxy_get("e2e-rl-nohdr429", "v1/models")
        .expect_status(200)
        .await;

    let resp = h
        .api_v1()
        .proxy_get("e2e-rl-nohdr429", "v1/models")
        .expect_status(429)
        .await;

    resp.assert_header("x-oagw-error-source", "gateway");
    assert!(
        resp.headers().get("retry-after").is_some(),
        "retry-after must always be present on 429 regardless of response_headers flag"
    );
    assert!(
        resp.headers().get("x-ratelimit-limit").is_none(),
        "x-ratelimit-limit must be absent when response_headers is false"
    );
    assert!(
        resp.headers().get("x-ratelimit-remaining").is_none(),
        "x-ratelimit-remaining must be absent when response_headers is false"
    );
    assert!(
        resp.headers().get("x-ratelimit-reset").is_none(),
        "x-ratelimit-reset must be absent when response_headers is false"
    );
    assert!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("application/problem+json"),
        "expected application/problem+json content type on 429"
    );
}

// Scenario 18.1, inst-tb-1..5b, dod-token-bucket — full burst → exhaust → refill → recover flow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_rate_limit_burst_then_recover() {
    let h = AppHarness::builder().build().await;

    setup_rate_limited_upstream(
        &h,
        "e2e-rl-burst",
        serde_json::json!({
            "algorithm": "token_bucket",
            "sustained": {"rate": 10, "window": "second"},
            "burst": {"capacity": 3},
            "scope": "tenant",
            "strategy": "reject",
            "cost": 1,
            "response_headers": true
        }),
        "/v1/models",
    )
    .await;

    // Exhaust burst capacity (3 tokens).
    for expected_remaining in (0..=2).rev() {
        let resp = h
            .api_v1()
            .proxy_get("e2e-rl-burst", "v1/models")
            .expect_status(200)
            .await;
        resp.assert_header("x-ratelimit-remaining", &expected_remaining.to_string());
    }

    // 4th request should be rejected.
    let resp = h
        .api_v1()
        .proxy_get("e2e-rl-burst", "v1/models")
        .expect_status(429)
        .await;
    assert!(resp.headers().get("retry-after").is_some());

    // Wait for refill (10 tokens/sec, so 500ms yields ~5 tokens).
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Should succeed again after refill.
    let resp = h
        .api_v1()
        .proxy_get("e2e-rl-burst", "v1/models")
        .expect_status(200)
        .await;
    let remaining: u64 = resp
        .headers()
        .get("x-ratelimit-remaining")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(remaining > 0, "expected remaining > 0 after refill");
}

// ADR 0004 §response_headers default — response_headers defaults to true when omitted.
#[tokio::test]
async fn e2e_rate_limit_response_headers_default_true() {
    let h = AppHarness::builder().build().await;

    // Deliberately omit `response_headers` from the rate_limit JSON.
    setup_rate_limited_upstream(
        &h,
        "e2e-rl-default",
        serde_json::json!({
            "algorithm": "token_bucket",
            "sustained": {"rate": 5, "window": "second"},
            "burst": {"capacity": 5},
            "scope": "tenant",
            "strategy": "reject",
            "cost": 1
        }),
        "/v1/models",
    )
    .await;

    let resp = h
        .api_v1()
        .proxy_get("e2e-rl-default", "v1/models")
        .expect_status(200)
        .await;

    assert!(
        resp.headers().get("x-ratelimit-limit").is_some(),
        "x-ratelimit-limit should be present when response_headers defaults to true"
    );
}

// inst-rl-2, inst-rl-5, inst-merge-5a — route-level rate limit enforcement (no upstream limit).
#[tokio::test]
async fn e2e_rate_limit_route_level_enforcement() {
    let h = AppHarness::builder().build().await;

    // Create upstream WITHOUT rate_limit.
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-rl-route",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    // Create route WITH rate_limit.
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &uid,
            "match": { "http": { "methods": ["GET"], "path": "/v1/models" } },
            "enabled": true,
            "tags": [],
            "priority": 0,
            "rate_limit": {
                "algorithm": "token_bucket",
                "sustained": {"rate": 1, "window": "minute"},
                "burst": {"capacity": 1},
                "scope": "tenant",
                "strategy": "reject",
                "cost": 1,
                "response_headers": true
            }
        }))
        .expect_status(201)
        .await;

    let resp = h
        .api_v1()
        .proxy_get("e2e-rl-route", "v1/models")
        .expect_status(200)
        .await;
    resp.assert_header("x-ratelimit-limit", "1");
    resp.assert_header("x-ratelimit-remaining", "0");

    h.api_v1()
        .proxy_get("e2e-rl-route", "v1/models")
        .expect_status(429)
        .await;
}

// inst-rl-2, inst-rl-5, inst-merge-5a — both upstream and route rate limits applied, tighter route wins.
#[tokio::test]
async fn e2e_rate_limit_upstream_and_route_both_applied() {
    let h = AppHarness::builder().build().await;

    // Upstream: generous limit.
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{"host": "127.0.0.1", "port": h.mock_port(), "scheme": "http"}]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-rl-dual",
            "enabled": true,
            "tags": [],
            "rate_limit": {
                "algorithm": "token_bucket",
                "sustained": {"rate": 100, "window": "minute"},
                "burst": {"capacity": 100},
                "scope": "tenant",
                "strategy": "reject",
                "cost": 1,
                "response_headers": true
            }
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    // Route: tight limit.
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": &uid,
            "match": { "http": { "methods": ["GET"], "path": "/v1/models" } },
            "enabled": true,
            "tags": [],
            "priority": 0,
            "rate_limit": {
                "algorithm": "token_bucket",
                "sustained": {"rate": 2, "window": "minute"},
                "burst": {"capacity": 2},
                "scope": "tenant",
                "strategy": "reject",
                "cost": 1,
                "response_headers": true
            }
        }))
        .expect_status(201)
        .await;

    // First two requests succeed; headers reflect tighter route limit.
    let resp = h
        .api_v1()
        .proxy_get("e2e-rl-dual", "v1/models")
        .expect_status(200)
        .await;
    // After 1st request: upstream remaining=99, route remaining=1 → route wins (lower remaining).
    resp.assert_header("x-ratelimit-limit", "2");
    resp.assert_header("x-ratelimit-remaining", "1");

    let resp = h
        .api_v1()
        .proxy_get("e2e-rl-dual", "v1/models")
        .expect_status(200)
        .await;
    resp.assert_header("x-ratelimit-remaining", "0");

    // Third request exceeds route limit.
    h.api_v1()
        .proxy_get("e2e-rl-dual", "v1/models")
        .expect_status(429)
        .await;
}
