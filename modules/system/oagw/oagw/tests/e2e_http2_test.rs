//! E2E tests verifying HTTP/2 upstream support through the Pingora proxy.
//!
//! Spins up a local TLS server that only speaks HTTP/2 (via ALPN `h2`),
//! configures OAGW to proxy to it with cert verification disabled, and
//! asserts that request/response round-trips work correctly over H2.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use oagw::test_support::{AppHarness, parse_resource_gts};
use rcgen::generate_simple_self_signed;
use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_rustls::TlsAcceptor;

// ---------------------------------------------------------------------------
// H2-only TLS mock upstream
// ---------------------------------------------------------------------------

/// Recorded request from the H2 mock.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct H2RecordedRequest {
    method: String,
    uri: String,
    version: hyper::Version,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct H2MockState {
    recorded: Mutex<Vec<H2RecordedRequest>>,
}

/// Start a TLS server on a random port that only accepts HTTP/2 via ALPN.
/// Returns (addr, shared_state, join_handle).
async fn start_h2_mock() -> (SocketAddr, Arc<H2MockState>, tokio::task::JoinHandle<()>) {
    // Generate self-signed cert for localhost / 127.0.0.1.
    let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    let cert = generate_simple_self_signed(subject_alt_names).expect("cert generation");

    let cert_der = CertificateDer::from(cert.cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        cert.key_pair.serialize_der().to_vec(),
    ));

    let mut tls_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("TLS config");

    // Only advertise h2 — no http/1.1 fallback.
    tls_config.alpn_protocols = vec![b"h2".to_vec()];
    let tls_acceptor = TlsAcceptor::from(Arc::new(tls_config));

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind H2 mock");
    let addr = listener.local_addr().expect("local addr");

    let state = Arc::new(H2MockState {
        recorded: Mutex::new(Vec::new()),
    });

    let state_clone = state.clone();
    let handle = tokio::spawn(async move {
        loop {
            let (tcp_stream, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(_) => continue,
            };

            let tls_acceptor = tls_acceptor.clone();
            let state = state_clone.clone();

            tokio::spawn(async move {
                let tls_stream = match tls_acceptor.accept(tcp_stream).await {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("H2 mock TLS accept error: {e}");
                        return;
                    }
                };

                let io = TokioIo::new(tls_stream);

                let state_svc = state.clone();
                let service = service_fn(move |req: Request<Incoming>| {
                    let state = state_svc.clone();
                    async move {
                        let method = req.method().to_string();
                        let uri = req.uri().to_string();
                        let version = req.version();
                        let headers: Vec<(String, String)> = req
                            .headers()
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                            .collect();

                        let body_bytes = http_body_util::BodyExt::collect(req.into_body())
                            .await
                            .map(|b| b.to_bytes().to_vec())
                            .unwrap_or_default();

                        state.recorded.lock().await.push(H2RecordedRequest {
                            method,
                            uri: uri.clone(),
                            version,
                            headers,
                            body: body_bytes.clone(),
                        });

                        // Route: SSE streaming for /v1/chat/completions/stream
                        if uri.contains("/v1/chat/completions/stream") {
                            let chunks = vec![
                                serde_json::json!({"id":"chatcmpl-h2","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}),
                                serde_json::json!({"id":"chatcmpl-h2","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":" over H2"},"finish_reason":null}]}),
                                serde_json::json!({"id":"chatcmpl-h2","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                            ];
                            let mut frames: Vec<Result<Frame<Bytes>, hyper::Error>> = Vec::new();
                            for chunk in &chunks {
                                let sse_line = format!("data: {}\n\n", chunk);
                                frames.push(Ok(Frame::data(Bytes::from(sse_line))));
                            }
                            frames.push(Ok(Frame::data(Bytes::from("data: [DONE]\n\n"))));

                            let stream = futures_util::stream::iter(frames);
                            let body = StreamBody::new(stream);

                            let resp = Response::builder()
                                .status(200)
                                .header("content-type", "text/event-stream")
                                .body(http_body_util::Either::Right(body))
                                .unwrap();
                            return Ok::<_, hyper::Error>(resp);
                        }

                        // Default: echo response with version info.
                        let resp_body = serde_json::json!({
                            "uri": uri,
                            "http_version": format!("{:?}", version),
                            "body": String::from_utf8_lossy(&body_bytes),
                        });

                        Ok::<_, hyper::Error>(Response::new(http_body_util::Either::Left(
                            Full::new(Bytes::from(resp_body.to_string())),
                        )))
                    }
                });

                if let Err(e) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .http2_only()
                    .serve_connection(io, service)
                    .await
                {
                    eprintln!("H2 mock connection error: {e}");
                }
            });
        }
    });

    (addr, state, handle)
}

// ---------------------------------------------------------------------------
// E2E tests
// ---------------------------------------------------------------------------

/// E2E: OAGW proxies a POST request to an HTTPS/H2-only upstream and gets
/// a successful response. Proves that ALPN negotiation + H2 framing works
/// end-to-end through the Pingora bridge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_http2_upstream_round_trip() {
    let (mock_addr, mock_state, _handle) = start_h2_mock().await;

    let h = AppHarness::builder()
        .with_skip_upstream_tls_verify(true)
        .build()
        .await;

    // Create upstream pointing to the H2 mock (HTTPS scheme triggers TLS + ALPN::H2H1).
    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{
                    "host": "127.0.0.1",
                    "port": mock_addr.port(),
                    "scheme": "https"
                }]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-h2",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    let (_, upstream_uuid) = parse_resource_gts(&uid).unwrap();
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": upstream_uuid,
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

    // Proxy a request through OAGW → H2 upstream.
    let resp = h
        .api_v1()
        .proxy_post("e2e-h2", "v1/chat/completions")
        .with_body(serde_json::json!({"model": "gpt-4", "messages": []}))
        .expect_status(200)
        .await;

    let body = resp.json();
    assert_eq!(
        body["http_version"].as_str().unwrap(),
        "HTTP/2.0",
        "upstream should have received the request via HTTP/2"
    );

    // Verify the mock recorded exactly one request at the expected path.
    let recorded = mock_state.recorded.lock().await;
    assert_eq!(recorded.len(), 1, "expected one recorded request");
    assert_eq!(recorded[0].version, hyper::Version::HTTP_2);
    assert!(
        recorded[0].uri.contains("/v1/chat/completions"),
        "unexpected URI: {}",
        recorded[0].uri
    );
}

/// E2E: OAGW proxies a GET request with no body to an HTTPS/H2-only upstream.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_http2_upstream_get_no_body() {
    let (mock_addr, mock_state, _handle) = start_h2_mock().await;

    let h = AppHarness::builder()
        .with_skip_upstream_tls_verify(true)
        .build()
        .await;

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{
                    "host": "127.0.0.1",
                    "port": mock_addr.port(),
                    "scheme": "https"
                }]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-h2-get",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    let (_, upstream_uuid) = parse_resource_gts(&uid).unwrap();
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": upstream_uuid,
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

    let resp = h
        .api_v1()
        .proxy_get("e2e-h2-get", "v1/models")
        .expect_status(200)
        .await;

    let body = resp.json();
    assert_eq!(
        body["http_version"].as_str().unwrap(),
        "HTTP/2.0",
        "upstream should have received the request via HTTP/2"
    );

    let recorded = mock_state.recorded.lock().await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].method, "GET");
    assert_eq!(recorded[0].version, hyper::Version::HTTP_2);
}

/// E2E: OAGW proxies a POST with a larger body to verify H2 body framing works.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_http2_upstream_large_body() {
    let (mock_addr, mock_state, _handle) = start_h2_mock().await;

    let h = AppHarness::builder()
        .with_skip_upstream_tls_verify(true)
        .build()
        .await;

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{
                    "host": "127.0.0.1",
                    "port": mock_addr.port(),
                    "scheme": "https"
                }]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-h2-large",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    let (_, upstream_uuid) = parse_resource_gts(&uid).unwrap();
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": upstream_uuid,
            "match": {
                "http": {
                    "methods": ["POST"],
                    "path": "/v1/embeddings"
                }
            },
            "enabled": true,
            "tags": [],
            "priority": 0
        }))
        .expect_status(201)
        .await;

    // 64 KB payload to exercise H2 DATA frame splitting.
    let large_payload = "x".repeat(64 * 1024);
    let resp = h
        .api_v1()
        .proxy_post("e2e-h2-large", "v1/embeddings")
        .with_body(serde_json::json!({"input": large_payload}))
        .expect_status(200)
        .await;

    let body = resp.json();
    assert_eq!(body["http_version"].as_str().unwrap(), "HTTP/2.0");

    // Verify the upstream received the full body.
    let recorded = mock_state.recorded.lock().await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].version, hyper::Version::HTTP_2);
    let received_body: serde_json::Value =
        serde_json::from_slice(&recorded[0].body).expect("upstream body should be valid JSON");
    assert_eq!(
        received_body["input"].as_str().unwrap().len(),
        64 * 1024,
        "upstream should receive the full 64KB payload"
    );
}

/// E2E: SSE streaming response over HTTP/2. Verifies that chunked/streaming
/// responses from an H2-only upstream are correctly relayed through the
/// Pingora bridge back to the client.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_http2_upstream_sse_streaming() {
    let (mock_addr, mock_state, _handle) = start_h2_mock().await;

    let h = AppHarness::builder()
        .with_skip_upstream_tls_verify(true)
        .build()
        .await;

    let resp = h
        .api_v1()
        .post_upstream()
        .with_body(serde_json::json!({
            "server": {
                "endpoints": [{
                    "host": "127.0.0.1",
                    "port": mock_addr.port(),
                    "scheme": "https"
                }]
            },
            "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
            "alias": "e2e-h2-sse",
            "enabled": true,
            "tags": []
        }))
        .expect_status(201)
        .await;
    let uid = resp.json()["id"].as_str().unwrap().to_string();

    let (_, upstream_uuid) = parse_resource_gts(&uid).unwrap();
    h.api_v1()
        .post_route()
        .with_body(serde_json::json!({
            "upstream_id": upstream_uuid,
            "match": {
                "http": {
                    "methods": ["POST"],
                    "path": "/v1/chat/completions/stream"
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
        .proxy_post("e2e-h2-sse", "v1/chat/completions/stream")
        .with_body(serde_json::json!({"model": "gpt-4", "stream": true}))
        .expect_status(200)
        .await;

    // Verify content-type is event-stream.
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        ct.contains("text/event-stream"),
        "expected text/event-stream, got: {ct}"
    );

    // Verify the body contains all SSE data lines including [DONE].
    resp.assert_body_contains("data: [DONE]");
    resp.assert_body_contains("Hello");
    resp.assert_body_contains("over H2");

    // Verify the upstream recorded the request over H2.
    let recorded = mock_state.recorded.lock().await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].version, hyper::Version::HTTP_2);
}
