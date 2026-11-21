//! The gateway error contract through the REST router (#4985): exact
//! statuses and full problem bodies for the transport failures, a disabled
//! upstream and a missing secret, and upstream errors passed through with
//! their own headers.
//!
//! Skipped under `--features fips`: `AppHarness` wires its token HTTP client
//! via `HttpClientConfig::for_testing()` (plaintext) against plaintext mocks
//! and does not install the FIPS crypto provider.

#![cfg(not(feature = "fips"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use credstore_sdk::test_util::MockCredStoreClient;
use credstore_sdk::{CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef};
use oagw::test_support::{AppHarness, MockBody, MockGuard, MockResponse, TestResponse};
use oagw_sdk::reason::unavailable::{
    REQUEST_MAY_HAVE_BEEN_SENT, REQUEST_NOT_SENT, UPSTREAM_DISABLED,
};
use oagw_sdk::{HTTP_PROTOCOL_ID, ServiceGatewayError};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;
use toolkit_security::SecurityContext;

const SERVICE_UNAVAILABLE: &str =
    "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~";
const UNAUTHENTICATED: &str = "gts://gts.cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~";
const APIKEY: &str = "gts.cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1";

/// Create an upstream at `host:port` and a `GET /v1/test` route on it.
async fn upstream(h: &AppHarness, alias: &str, scheme: &str, port: u16, extra: Value) -> String {
    let mut body = json!({
        "server": {"endpoints": [{"host": "127.0.0.1", "port": port, "scheme": scheme}]},
        "protocol": HTTP_PROTOCOL_ID,
        "alias": alias,
        "enabled": true,
        "tags": [],
    });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(body)
        .expect_status(201)
        .await;
    let id = resp.json()["id"].as_str().unwrap().to_owned();
    route(h, &id, "/v1/test").await;
    id
}

async fn route(h: &AppHarness, upstream_id: &str, path: &str) {
    h.api_v1()
        .post_route()
        .with_body(json!({
            "upstream_id": upstream_id,
            "match": {"http": {"methods": ["GET"], "path": path}},
            "enabled": true,
            "tags": [],
            "priority": 0,
        }))
        .expect_status(201)
        .await;
}

/// The problem body without the fields the middleware adds per request.
fn body_of(resp: &TestResponse) -> Value {
    resp.assert_header("x-oagw-error-source", "gateway");
    let ct = resp.headers()["content-type"].to_str().unwrap();
    assert!(ct.starts_with("application/problem+json"), "{ct}");
    let mut body = resp.json();
    let obj = body.as_object_mut().unwrap();
    obj.remove("trace_id");
    let instance = obj.remove("instance");
    assert!(
        instance.as_ref().and_then(Value::as_str).is_some(),
        "{instance:?}"
    );
    body
}

fn unavailable(status: u16, retry: u64, reason: &str) -> Value {
    json!({
        "type": SERVICE_UNAVAILABLE,
        "title": "Service Unavailable",
        "status": status,
        "detail": "Service temporarily unavailable",
        "context": {"retry_after_seconds": retry, "reason": reason},
    })
}

/// A port nothing listens on: bound, then released.
fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A refused connection is a 502: nothing was sent.
#[tokio::test]
async fn connection_refused_is_502_request_not_sent() {
    let h = AppHarness::builder().build().await;
    upstream(&h, "ec-refused", "http", closed_port(), json!({})).await;

    let resp = h
        .api_v1()
        .proxy_get("ec-refused", "v1/test")
        .expect_status(502)
        .await;
    assert_eq!(body_of(&resp), unavailable(502, 5, REQUEST_NOT_SENT));
    resp.assert_header("retry-after", "5");
}

/// A TLS handshake against a server that does not speak TLS fails before
/// the request is sent: 502, not contacted.
#[tokio::test]
async fn tls_handshake_failure_is_502_request_not_sent() {
    let h = AppHarness::builder().build().await;
    // The mock upstream speaks cleartext HTTP only.
    upstream(&h, "ec-tls", "https", h.mock_port(), json!({})).await;

    let resp = h
        .api_v1()
        .proxy_get("ec-tls", "v1/test")
        .expect_status(502)
        .await;
    assert_eq!(body_of(&resp), unavailable(502, 5, REQUEST_NOT_SENT));
}

/// An upstream that accepts the connection, reads the request and closes
/// without answering: 502, the request may have been sent.
#[tokio::test]
async fn peer_closed_after_the_request_is_502_request_may_have_been_sent() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 4096];
            // Read the request head, then drop the connection.
            let _ = socket.read(&mut buf).await;
        }
    });

    let h = AppHarness::builder().build().await;
    upstream(&h, "ec-dropped", "http", port, json!({})).await;

    let resp = h
        .api_v1()
        .proxy_get("ec-dropped", "v1/test")
        .expect_status(502)
        .await;
    assert_eq!(
        body_of(&resp),
        unavailable(502, 5, REQUEST_MAY_HAVE_BEEN_SENT)
    );
    server.abort();
}

/// The SDK sees the same reasons as typed values.
#[tokio::test]
async fn sdk_projection_carries_the_dispatch_reason() {
    use oagw_sdk::reason::unavailable::UnavailableReason;

    let h = AppHarness::builder().build().await;
    upstream(&h, "ec-sdk", "http", closed_port(), json!({})).await;
    let req = http::Request::builder()
        .method(http::Method::GET)
        .uri("/ec-sdk/v1/test")
        .body(oagw_sdk::Body::Empty)
        .unwrap();
    let err = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 502);
    assert!(
        matches!(
            ServiceGatewayError::from(err),
            ServiceGatewayError::Unavailable {
                retry_after_secs: Some(5),
                reason: Some(UnavailableReason::RequestNotSent),
            }
        ),
        "wrong projection"
    );
}

/// A disabled upstream is a 503 that says so, with a long retry hint:
/// only an operator can enable it again.
#[tokio::test]
async fn disabled_upstream_is_503_upstream_disabled() {
    let h = AppHarness::builder().build().await;
    upstream(
        &h,
        "ec-disabled",
        "http",
        h.mock_port(),
        json!({"enabled": false}),
    )
    .await;

    let resp = h
        .api_v1()
        .proxy_get("ec-disabled", "v1/test")
        .expect_status(503)
        .await;
    assert_eq!(body_of(&resp), unavailable(503, 300, UPSTREAM_DISABLED));
    resp.assert_header("retry-after", "300");
}

/// A credential store whose secrets can be removed after the upstream was
/// written.
struct RemovableSecrets {
    inner: MockCredStoreClient,
    gone: AtomicBool,
}

#[async_trait]
impl CredStoreClientV1 for RemovableSecrets {
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        if self.gone.load(Ordering::SeqCst) {
            return Ok(None);
        }
        self.inner.get(ctx, key).await
    }
}

/// A secret deleted after the upstream was written is a 401 with its own
/// reason (it was 500); the body names neither the key nor the reference.
#[tokio::test]
async fn deleted_secret_is_401_auth_credential_not_found() {
    let store = Arc::new(RemovableSecrets {
        inner: MockCredStoreClient::with_secrets(vec![(
            "cred://ec-gone-key".into(),
            "sk-gone".into(),
        )]),
        gone: AtomicBool::new(false),
    });
    let h = AppHarness::builder()
        .with_credstore(store.clone())
        .build()
        .await;
    upstream(
        &h,
        "ec-gone",
        "http",
        h.mock_port(),
        json!({"auth": {
            "type": APIKEY,
            "sharing": "private",
            "config": {"header": "authorization", "prefix": "Bearer ", "secret_ref": "cred://ec-gone-key"},
        }}),
    )
    .await;
    store.gone.store(true, Ordering::SeqCst);

    let resp = h
        .api_v1()
        .proxy_get("ec-gone", "v1/test")
        .expect_status(401)
        .await;
    assert_eq!(
        body_of(&resp),
        json!({
            "type": UNAUTHENTICATED,
            "title": "Unauthenticated",
            "status": 401,
            "detail": "Authentication required",
            "context": {"reason": oagw_sdk::reason::auth::CREDENTIAL_NOT_FOUND},
        })
    );
    let text = resp.text();
    assert!(!text.contains("ec-gone-key"), "{text}");
    assert!(!text.contains("secret_ref"), "{text}");
}

/// Upstream errors pass through with their own `Retry-After` and
/// `WWW-Authenticate`; the gateway adds only its error source.
#[tokio::test]
async fn upstream_retry_after_and_www_authenticate_pass_through() {
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        "/busy",
        MockResponse {
            status: 503,
            headers: vec![("retry-after".into(), "120".into())],
            body: MockBody::Json(json!({"error": "busy"})),
        },
    );
    guard.mock(
        "GET",
        "/login",
        MockResponse {
            status: 401,
            headers: vec![(
                "www-authenticate".into(),
                r#"Bearer realm="upstream", error="invalid_token""#.into(),
            )],
            body: MockBody::Json(json!({"error": "invalid_token"})),
        },
    );

    let h = AppHarness::builder().build().await;
    let id = upstream(&h, "ec-pass", "http", h.mock_port(), json!({})).await;
    route(&h, &id, &guard.path("/busy")).await;
    route(&h, &id, &guard.path("/login")).await;

    let path = |p: &str| guard.path(p).trim_start_matches('/').to_owned();
    let resp = h
        .api_v1()
        .proxy_get("ec-pass", &path("/busy"))
        .expect_status(503)
        .await;
    resp.assert_header("x-oagw-error-source", "upstream");
    resp.assert_header("retry-after", "120");
    assert_eq!(resp.json(), json!({"error": "busy"}));

    let resp = h
        .api_v1()
        .proxy_get("ec-pass", &path("/login"))
        .expect_status(401)
        .await;
    resp.assert_header("x-oagw-error-source", "upstream");
    resp.assert_header(
        "www-authenticate",
        r#"Bearer realm="upstream", error="invalid_token""#,
    );
    assert_eq!(resp.json(), json!({"error": "invalid_token"}));
}
