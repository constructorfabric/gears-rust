use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::infra::backend::{
    BackendRegistry, InMemoryBackend, LocalFsBackend, StorageBackend,
};
use file_storage::infra::metrics::NoopMetrics;
use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};

use super::{
    DEFAULT_MAX_BODY_BYTES, SidecarState, build_router, finalize_with_control_plane,
    interpret_finalize_response, report_part_with_control_plane, write_multipart_part_native,
    write_multipart_part_offset_object,
};

fn test_state() -> SidecarState {
    let issuer = Issuer::generate(60).expect("issuer generation");
    let backends = BackendRegistry::new(
        vec![Arc::new(InMemoryBackend::new("test")) as Arc<dyn StorageBackend>],
        "test",
    )
    .expect("build test backend registry");
    SidecarState {
        verifier: std::sync::Arc::new(issuer.verifier()),
        backends,
        control_base_url: String::new(),
        internal_token: "test-internal-token".to_owned(),
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
    }
}

#[tokio::test]
async fn sidecar_healthz_returns_200() {
    let router = build_router(test_state(), DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::get("/healthz")
                .body(Body::empty())
                .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn sidecar_readyz_returns_200_when_backends_ready() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let issuer = Issuer::generate(60).expect("issuer generation");
    let backend = Arc::new(LocalFsBackend::new("local-fs", dir.path()));
    let backends = BackendRegistry::new(
        vec![Arc::clone(&backend) as Arc<dyn StorageBackend>],
        "local-fs",
    )
    .expect("build test backend registry");
    let state = SidecarState {
        verifier: Arc::new(issuer.verifier()),
        backends,
        control_base_url: String::new(),
        internal_token: "test-internal-token".to_owned(),
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
    };

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::get("/readyz")
                .body(Body::empty())
                .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    assert_eq!(&body[..], b"ready");
}

#[tokio::test]
async fn sidecar_readyz_returns_503_when_backend_root_missing() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let missing_root = dir.path().join("does-not-exist");
    // `dir` is dropped too, so the parent of `missing_root` is gone as well.
    drop(dir);

    let issuer = Issuer::generate(60).expect("issuer generation");
    let backend = Arc::new(LocalFsBackend::new("local-fs", &missing_root));
    let backends = BackendRegistry::new(
        vec![Arc::clone(&backend) as Arc<dyn StorageBackend>],
        "local-fs",
    )
    .expect("build test backend registry");
    let state = SidecarState {
        verifier: Arc::new(issuer.verifier()),
        backends,
        control_base_url: String::new(),
        internal_token: "test-internal-token".to_owned(),
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
    };

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::get("/readyz")
                .body(Body::empty())
                .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let body_text = String::from_utf8(body.to_vec()).expect("valid utf8 body");
    assert!(
        body_text.contains("local-fs"),
        "body must name the failing backend id, got {body_text:?}"
    );
    assert!(
        !body_text.to_lowercase().contains("no such file")
            && !body_text.contains(missing_root.to_string_lossy().as_ref()),
        "body must not leak the underlying OS error or filesystem path, got {body_text:?}"
    );
}

/// A body over axum's default 2 MiB limit must reach the handler (401 for a missing token),
/// not be rejected with a bare `413` by the transport layer.
#[tokio::test]
async fn sidecar_body_limit_allows_bodies_over_2mib() {
    let router = build_router(test_state(), DEFAULT_MAX_BODY_BYTES);
    let body = vec![0u8; 3 * 1024 * 1024]; // 3 MiB, over axum's 2 MiB default.
    let response = router
        .oneshot(
            Request::put(
                "/api/file-storage-data/v1/upload/\
                 00000000-0000-0000-0000-000000000000/\
                 00000000-0000-0000-0000-000000000000",
            )
            .body(Body::from(body))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// A control plane that accepts but never responds must not hang the finalize callback:
/// per-attempt timeouts trip, retries are exhausted and `Err` is returned. The outer
/// `tokio::time::timeout` only makes a regression fail fast.
#[tokio::test]
async fn finalize_callback_times_out_within_configured_bound() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock listener");
    let addr = listener.local_addr().expect("local addr");

    // Accept connections but never respond, so the client read times out.
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });

    let http = reqwest::Client::builder()
        .timeout(Duration::from_millis(150))
        .connect_timeout(Duration::from_millis(150))
        .build()
        .expect("client build");
    let mut state = test_state();
    state.http = http;
    state.control_base_url = format!("http://{addr}");

    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        finalize_with_control_plane(
            &state,
            "dummy-token",
            "test-request-id",
            Uuid::nil(),
            Uuid::nil(),
            0,
            "deadbeef",
        ),
    )
    .await
    .expect(
        "finalize_with_control_plane must return within the test's own timeout budget \
         (production timeout regressed if this fires)",
    );

    assert!(
        outcome.is_err(),
        "finalize must fail when the control plane never responds"
    );
}

#[tokio::test]
async fn finalize_callback_retries_on_connection_refused_then_succeeds() {
    // Reserve a free port and release it: connecting then yields ECONNREFUSED.
    let probe = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind probe listener");
    let addr = probe.local_addr().expect("local addr");
    drop(probe);

    let accepted = Arc::new(AtomicUsize::new(0));
    let accepted_clone = Arc::clone(&accepted);

    // Let the first attempt fail before a real listener claims the address.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let listener = TcpListener::bind(addr)
            .await
            .expect("bind mock control plane");
        if let Ok((mut stream, _)) = listener.accept().await {
            accepted_clone.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 1024];
            if stream.read(&mut buf).await.is_ok() {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                    .await
                    .ok();
            }
        }
    });

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .connect_timeout(Duration::from_secs(2))
        .build()
        .expect("client build");
    let mut state = test_state();
    state.http = http;
    state.control_base_url = format!("http://{addr}");

    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        finalize_with_control_plane(
            &state,
            "dummy-token",
            "test-request-id",
            Uuid::nil(),
            Uuid::nil(),
            0,
            "deadbeef",
        ),
    )
    .await
    .expect("finalize_with_control_plane must return within the test's own timeout budget");

    assert!(
        outcome.is_ok(),
        "finalize must succeed once it retries past the connection-refused attempt"
    );
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "exactly one connection should reach the mock control plane (the retry)"
    );
}

/// `SidecarState` over a fresh `InMemoryBackend`, plus an `Issuer` its verifier accepts.
fn test_download_state() -> (SidecarState, Issuer, Arc<InMemoryBackend>) {
    let issuer = Issuer::generate(60).expect("issuer generation");
    let backend = Arc::new(InMemoryBackend::new("test"));
    let backends = BackendRegistry::new(
        vec![Arc::clone(&backend) as Arc<dyn StorageBackend>],
        "test",
    )
    .expect("build test backend registry");
    let state = SidecarState {
        verifier: Arc::new(issuer.verifier()),
        backends,
        control_base_url: String::new(),
        internal_token: "test-internal-token".to_owned(),
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
    };
    (state, issuer, backend)
}

/// Signed `op = get` token with no `content_type`/`etag` claims (old-token shape).
fn download_token(issuer: &Issuer, file_id: Uuid, version_id: Uuid, backend_path: &str) -> String {
    download_token_with_meta(issuer, file_id, version_id, backend_path, "", "")
}

/// Signed `op = get` token with `content_type`/`etag` claims; empty strings give an old token.
fn download_token_with_meta(
    issuer: &Issuer,
    file_id: Uuid,
    version_id: Uuid,
    backend_path: &str,
    content_type: &str,
    etag: &str,
) -> String {
    let claims = Claims {
        op: Op::Get,
        file_id,
        version_id,
        backend_id: "test".to_owned(),
        backend_path: backend_path.to_owned(),
        exp: OffsetDateTime::now_utc().unix_timestamp() + 60,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: content_type.to_owned(),
        etag: etag.to_owned(),
        bind_on_finalize: false,
    };
    issuer
        .issue(claims, OffsetDateTime::now_utc())
        .expect("issue download token")
}

/// A sub-range `GET` returns `206` with a correct `Content-Range` and the exact slice.
#[tokio::test]
async fn download_range_response_includes_content_range() {
    let (state, issuer, backend) = test_download_state();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    backend
        .put(&path, bytes::Bytes::from_static(b"hello world"))
        .await
        .expect("seed blob");
    let token = download_token(&issuer, file_id, version_id, &path);

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::get(format!(
                "/api/file-storage-data/v1/download/{file_id}/{version_id}?fs-token={token}"
            ))
            .header(header::RANGE, "bytes=0-4")
            .body(Body::empty())
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    let content_range = response
        .headers()
        .get(header::CONTENT_RANGE)
        .expect("Content-Range header present on 206")
        .to_str()
        .expect("valid header value");
    assert_eq!(content_range, "bytes 0-4/11");

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    assert_eq!(&body[..], b"hello");
}

#[tokio::test]
async fn download_missing_blob_returns_404_not_416() {
    let (state, issuer, _backend) = test_download_state();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}"); // never written
    let token = download_token(&issuer, file_id, version_id, &path);

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::get(format!(
                "/api/file-storage-data/v1/download/{file_id}/{version_id}?fs-token={token}"
            ))
            .header(header::RANGE, "bytes=0-4")
            .body(Body::empty())
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a missing blob must be 404, not 416"
    );
}

/// A range past the end of an existing blob is `416` with `Content-Range: bytes */{total}`.
#[tokio::test]
async fn download_unsatisfiable_range_returns_416_with_content_range() {
    let (state, issuer, backend) = test_download_state();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    backend
        .put(&path, bytes::Bytes::from_static(b"hello world")) // 11 bytes
        .await
        .expect("seed blob");
    let token = download_token(&issuer, file_id, version_id, &path);

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::get(format!(
                "/api/file-storage-data/v1/download/{file_id}/{version_id}?fs-token={token}"
            ))
            .header(header::RANGE, "bytes=100-200")
            .body(Body::empty())
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    let content_range = response
        .headers()
        .get(header::CONTENT_RANGE)
        .expect("Content-Range header present on 416")
        .to_str()
        .expect("valid header value");
    assert_eq!(content_range, "bytes */11");
}

/// The `200` download echoes the token's `content_type`/`etag` claims as headers (no DB access).
#[tokio::test]
async fn download_sets_content_type_and_etag_from_claims() {
    let (state, issuer, backend) = test_download_state();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    backend
        .put(&path, bytes::Bytes::from_static(b"hello world"))
        .await
        .expect("seed blob");
    let token = download_token_with_meta(
        &issuer,
        file_id,
        version_id,
        &path,
        "image/png",
        "\"abc123\"",
    );

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::get(format!(
                "/api/file-storage-data/v1/download/{file_id}/{version_id}?fs-token={token}"
            ))
            .body(Body::empty())
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("Content-Type header present")
            .to_str()
            .expect("valid header value"),
        "image/png"
    );
    assert_eq!(
        response
            .headers()
            .get(header::ETAG)
            .expect("ETag header present")
            .to_str()
            .expect("valid header value"),
        "\"abc123\""
    );
}

#[tokio::test]
async fn download_range_sets_content_type_and_etag_from_claims() {
    let (state, issuer, backend) = test_download_state();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    backend
        .put(&path, bytes::Bytes::from_static(b"hello world"))
        .await
        .expect("seed blob");
    let token = download_token_with_meta(
        &issuer,
        file_id,
        version_id,
        &path,
        "text/plain",
        "\"deadbeef\"",
    );

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::get(format!(
                "/api/file-storage-data/v1/download/{file_id}/{version_id}?fs-token={token}"
            ))
            .header(header::RANGE, "bytes=0-4")
            .body(Body::empty())
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("Content-Type header present")
            .to_str()
            .expect("valid header value"),
        "text/plain"
    );
    assert_eq!(
        response
            .headers()
            .get(header::ETAG)
            .expect("ETag header present")
            .to_str()
            .expect("valid header value"),
        "\"deadbeef\""
    );
}

/// Old tokens (empty claims) fall back to `FALLBACK_CONTENT_TYPE` and omit `ETag`.
#[tokio::test]
async fn download_without_meta_claims_falls_back_to_octet_stream_and_no_etag() {
    let (state, issuer, backend) = test_download_state();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    backend
        .put(&path, bytes::Bytes::from_static(b"hello world"))
        .await
        .expect("seed blob");
    let token = download_token(&issuer, file_id, version_id, &path);

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::get(format!(
                "/api/file-storage-data/v1/download/{file_id}/{version_id}?fs-token={token}"
            ))
            .body(Body::empty())
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("Content-Type header present")
            .to_str()
            .expect("valid header value"),
        "application/octet-stream"
    );
    assert!(
        response.headers().get(header::ETAG).is_none(),
        "old token carries no etag claim; ETag header must be absent, not empty"
    );
}

/// A control-plane finalize error must not leak the upstream status/body or address to the client.
#[tokio::test]
async fn finalize_failure_does_not_leak_control_plane_url() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock control plane");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            if stream.read(&mut buf).await.is_ok() {
                let body = "internal-upstream-secret-detail";
                let response = format!(
                    "HTTP/1.1 500 Internal Server Error\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.ok();
            }
        }
    });

    let mut state = test_state();
    state.control_base_url = format!("http://{addr}");

    let outcome = finalize_with_control_plane(
        &state,
        "dummy-token",
        "test-request-id",
        Uuid::nil(),
        Uuid::nil(),
        0,
        "deadbeef",
    )
    .await;

    let Err(rejection) = outcome else {
        panic!("finalize must fail when the control plane returns an error status");
    };
    let response = rejection.into_response();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let body_text = String::from_utf8_lossy(&body_bytes).to_lowercase();

    assert!(
        !body_text.contains("internal-upstream-secret-detail"),
        "client-facing body must not leak the upstream error body: {body_text}"
    );
    assert!(
        !body_text.contains(&addr.to_string()),
        "client-facing body must not leak the control-plane address: {body_text}"
    );
    assert!(
        !body_text.contains("500"),
        "client-facing body must not leak the raw upstream HTTP status: {body_text}"
    );
}

/// The callback builder must send the token in `x-fs-internal-token` (raw TCP listener check).
#[tokio::test]
async fn finalize_callback_sends_internal_token_header() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock control plane");
    let addr = listener.local_addr().expect("local addr");

    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request_text = String::from_utf8_lossy(&buf[..n]).into_owned();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await
                .ok();
            tx.send(request_text).ok();
        }
    });

    let mut state = test_state();
    state.control_base_url = format!("http://{addr}");
    state.internal_token = "interim-shared-secret".to_owned();

    let outcome = finalize_with_control_plane(
        &state,
        "dummy-token",
        "test-request-id",
        Uuid::nil(),
        Uuid::nil(),
        0,
        "deadbeef",
    )
    .await;
    assert!(
        outcome.is_ok(),
        "finalize must succeed against the mock 200 OK response"
    );

    let request_text = rx.await.expect("mock control plane must receive a request");
    assert!(
        request_text
            .to_lowercase()
            .contains("x-fs-internal-token: interim-shared-secret"),
        "finalize callback must carry the configured x-fs-internal-token header: {request_text}"
    );
}

fn upload_token(
    issuer: &Issuer,
    file_id: Uuid,
    version_id: Uuid,
    backend_id: &str,
    backend_path: &str,
) -> String {
    let claims = Claims {
        op: Op::Put,
        file_id,
        version_id,
        backend_id: backend_id.to_owned(),
        backend_path: backend_path.to_owned(),
        exp: OffsetDateTime::now_utc().unix_timestamp() + 60,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };
    issuer
        .issue(claims, OffsetDateTime::now_utc())
        .expect("issue upload token")
}

#[allow(clippy::too_many_arguments)]
fn multipart_part_token(
    issuer: &Issuer,
    file_id: Uuid,
    version_id: Uuid,
    backend_id: &str,
    backend_path: &str,
    upload_id: Uuid,
    part_number: u32,
    offset: u64,
    size: u64,
    backend_handle: &str,
) -> String {
    let claims = Claims {
        op: Op::MultipartPart,
        file_id,
        version_id,
        backend_id: backend_id.to_owned(),
        backend_path: backend_path.to_owned(),
        exp: OffsetDateTime::now_utc().unix_timestamp() + 60,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims {
            upload_id,
            part_number,
            offset,
            size,
            backend_handle: backend_handle.to_owned(),
        },
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };
    issuer
        .issue(claims, OffsetDateTime::now_utc())
        .expect("issue multipart part token")
}

/// `upload_multipart_part` must call the backend's native `upload_part` for `multipart_native`
/// backends, not the offset-object fallback (else `complete_multipart` would fail).
#[tokio::test]
async fn sidecar_multipart_native_backend_dispatches_to_upload_part() {
    let issuer = Issuer::generate(60).expect("issuer generation");
    let backend = Arc::new(InMemoryBackend::new("mem"));
    let backends =
        BackendRegistry::new(vec![Arc::clone(&backend) as Arc<dyn StorageBackend>], "mem")
            .expect("build test backend registry");
    let state = SidecarState {
        verifier: Arc::new(issuer.verifier()),
        backends,
        control_base_url: String::new(),
        internal_token: "test-internal-token".to_owned(),
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
    };

    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let backend_path = format!("/{file_id}/{version_id}");
    let upload_id = Uuid::now_v7();

    // Mirror `initiate_multipart_upload`: initiate on the backend, mint part tokens with it.
    let backend_handle = backend
        .initiate_multipart(&backend_path)
        .await
        .expect("initiate native multipart session");

    let part1 = b"first-part-bytes".to_vec();
    let part2 = b"second-part-payload".to_vec();

    let token1 = multipart_part_token(
        &issuer,
        file_id,
        version_id,
        "mem",
        &backend_path,
        upload_id,
        1,
        0,
        part1.len() as u64,
        &backend_handle,
    );
    let token2 = multipart_part_token(
        &issuer,
        file_id,
        version_id,
        "mem",
        &backend_path,
        upload_id,
        2,
        part1.len() as u64,
        part2.len() as u64,
        &backend_handle,
    );

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);

    let resp1 = router
        .clone()
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/1?fs-token={token1}"
            ))
            .body(Body::from(part1.clone()))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(resp1.status(), StatusCode::OK, "part 1 PUT must succeed");
    let resp1_body = axum::body::to_bytes(resp1.into_body(), usize::MAX)
        .await
        .expect("read part 1 response body");
    let resp1_json: serde_json::Value =
        serde_json::from_slice(&resp1_body).expect("part 1 response is JSON");
    let etag1 = resp1_json["etag"]
        .as_str()
        .expect("part 1 response has an etag")
        .to_owned();

    let resp2 = router
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/2?fs-token={token2}"
            ))
            .body(Body::from(part2.clone()))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(resp2.status(), StatusCode::OK, "part 2 PUT must succeed");
    let resp2_body = axum::body::to_bytes(resp2.into_body(), usize::MAX)
        .await
        .expect("read part 2 response body");
    let resp2_json: serde_json::Value =
        serde_json::from_slice(&resp2_body).expect("part 2 response is JSON");
    let etag2 = resp2_json["etag"]
        .as_str()
        .expect("part 2 response has an etag")
        .to_owned();

    // Completing the native session succeeds only if both parts landed via `upload_part`.
    let hash1 = file_storage::infra::content::hash::digest_to_array(
        file_storage::infra::content::hash::sha256(&part1),
    );
    let hash2 = file_storage::infra::content::hash::digest_to_array(
        file_storage::infra::content::hash::sha256(&part2),
    );
    let (manifest, root) = backend
        .complete_multipart(
            &backend_path,
            &backend_handle,
            &[(1, 0, hash1, etag1), (2, part1.len() as u64, hash2, etag2)],
        )
        .await
        .expect("complete native multipart session - both parts must be real");

    let assembled = backend
        .get(&backend_path)
        .await
        .expect("read assembled object");
    let mut expected = part1.clone();
    expected.extend_from_slice(&part2);
    assert_eq!(
        &assembled[..],
        &expected[..],
        "assembled object must be the exact concatenation of the two parts"
    );

    let expected_manifest = file_storage::infra::content::hash_mode::Manifest::new(vec![
        file_storage::infra::content::hash_mode::ManifestEntry {
            offset: 0,
            digest: hash1,
        },
        file_storage::infra::content::hash_mode::ManifestEntry {
            offset: part1.len() as u64,
            digest: hash2,
        },
    ])
    .unwrap();
    assert_eq!(
        manifest.to_wire_string(),
        expected_manifest.to_wire_string()
    );
    assert_eq!(
        root,
        expected_manifest.root(),
        "complete_multipart's returned root must be sha256(manifest)"
    );
}

/// An undersized part (fewer bytes than the token's `size`) is `400`, not `413`.
#[tokio::test]
async fn write_multipart_part_native_undersized_returns_400() {
    let backend = InMemoryBackend::new("mem");
    let backend_path = "/undersized-native";
    let backend_handle = backend
        .initiate_multipart(backend_path)
        .await
        .expect("initiate native multipart session");
    let claims = Claims {
        op: Op::MultipartPart,
        file_id: Uuid::now_v7(),
        version_id: Uuid::now_v7(),
        backend_id: "mem".to_owned(),
        backend_path: backend_path.to_owned(),
        exp: OffsetDateTime::now_utc().unix_timestamp() + 60,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims {
            upload_id: Uuid::now_v7(),
            part_number: 1,
            offset: 0,
            size: 10,
            backend_handle,
        },
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };

    let err = write_multipart_part_native(&backend, &claims, 1, Body::from(b"short".to_vec()))
        .await
        .expect_err("undersized part must be rejected");
    assert_eq!(
        err.status,
        StatusCode::BAD_REQUEST,
        "undersized part is a client size mismatch, not an over-limit body"
    );
}

#[tokio::test]
async fn write_multipart_part_offset_object_undersized_returns_400() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let backend = LocalFsBackend::new("local-fs", dir.path());
    let claims = Claims {
        op: Op::MultipartPart,
        file_id: Uuid::now_v7(),
        version_id: Uuid::now_v7(),
        backend_id: "local-fs".to_owned(),
        backend_path: "/undersized-offset".to_owned(),
        exp: OffsetDateTime::now_utc().unix_timestamp() + 60,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims {
            upload_id: Uuid::now_v7(),
            part_number: 1,
            offset: 0,
            size: 10,
            backend_handle: String::new(),
        },
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };

    let err =
        write_multipart_part_offset_object(&backend, &claims, 1, Body::from(b"short".to_vec()))
            .await
            .expect_err("undersized part must be rejected");
    assert_eq!(
        err.status,
        StatusCode::BAD_REQUEST,
        "undersized part is a client size mismatch, not an over-limit body"
    );
}

/// Each upload goes to the backend named by the token's `claims.backend_id`.
#[tokio::test]
async fn sidecar_resolves_backend_by_claims_backend_id() {
    let issuer = Issuer::generate(60).expect("issuer generation");
    let backend_a = Arc::new(InMemoryBackend::new("local-fs"));
    let backend_b = Arc::new(InMemoryBackend::new("other"));
    let backends = BackendRegistry::new(
        vec![
            Arc::clone(&backend_a) as Arc<dyn StorageBackend>,
            Arc::clone(&backend_b) as Arc<dyn StorageBackend>,
        ],
        "local-fs",
    )
    .expect("build two-backend registry");
    let state = SidecarState {
        verifier: Arc::new(issuer.verifier()),
        backends,
        control_base_url: String::new(),
        internal_token: "test-internal-token".to_owned(),
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
    };

    let file_id_a = Uuid::now_v7();
    let version_id_a = Uuid::now_v7();
    let path_a = format!("/{file_id_a}/{version_id_a}");
    let token_a = upload_token(&issuer, file_id_a, version_id_a, "local-fs", &path_a);

    let file_id_b = Uuid::now_v7();
    let version_id_b = Uuid::now_v7();
    let path_b = format!("/{file_id_b}/{version_id_b}");
    let token_b = upload_token(&issuer, file_id_b, version_id_b, "other", &path_b);

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);

    let response_a = router
        .clone()
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/upload/{file_id_a}/{version_id_a}?fs-token={token_a}"
            ))
            .body(Body::from(b"bytes-for-local-fs".to_vec()))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(response_a.status(), StatusCode::OK);

    let response_b = router
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/upload/{file_id_b}/{version_id_b}?fs-token={token_b}"
            ))
            .body(Body::from(b"bytes-for-other".to_vec()))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(response_b.status(), StatusCode::OK);

    let a_paths = backend_a.list_paths().await.expect("list local-fs paths");
    assert!(
        a_paths.contains(&path_a),
        "expected {path_a} in local-fs backend, got {a_paths:?}"
    );
    assert!(
        !a_paths.contains(&path_b),
        "path_b must not land in local-fs backend, got {a_paths:?}"
    );
    let got_a = backend_a
        .get(&path_a)
        .await
        .expect("get from local-fs backend");
    assert_eq!(&got_a[..], b"bytes-for-local-fs");

    let b_paths = backend_b.list_paths().await.expect("list other paths");
    assert!(
        b_paths.contains(&path_b),
        "expected {path_b} in other backend, got {b_paths:?}"
    );
    assert!(
        !b_paths.contains(&path_a),
        "path_a must not land in other backend, got {b_paths:?}"
    );
    let got_b = backend_b
        .get(&path_b)
        .await
        .expect("get from other backend");
    assert_eq!(&got_b[..], b"bytes-for-other");
}

async fn failing_control_plane() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock control plane");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            if stream.read(&mut buf).await.is_ok() {
                stream
                    .write_all(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\n\r\n")
                    .await
                    .ok();
            }
        }
    });
    format!("http://{addr}")
}

fn part_claims(backend_id: &str, backend_path: &str, size: u64, backend_handle: String) -> Claims {
    Claims {
        op: Op::MultipartPart,
        file_id: Uuid::now_v7(),
        version_id: Uuid::now_v7(),
        backend_id: backend_id.to_owned(),
        backend_path: backend_path.to_owned(),
        exp: OffsetDateTime::now_utc().unix_timestamp() + 60,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims {
            upload_id: Uuid::now_v7(),
            part_number: 1,
            offset: 0,
            size,
            backend_handle,
        },
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    }
}

#[tokio::test]
async fn write_multipart_part_native_oversized_returns_413() {
    let backend = InMemoryBackend::new("mem");
    let backend_handle = backend
        .initiate_multipart("/oversized-native")
        .await
        .expect("initiate native multipart session");
    let claims = part_claims("mem", "/oversized-native", 4, backend_handle);

    let err = write_multipart_part_native(&backend, &claims, 1, Body::from(b"longer".to_vec()))
        .await
        .expect_err("oversized part must be rejected");
    assert_eq!(err.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn write_multipart_part_native_body_read_error_returns_400() {
    let backend = InMemoryBackend::new("mem");
    let claims = part_claims("mem", "/broken-body", 8, String::new());
    let body = Body::from_stream(futures::stream::iter([
        Ok(bytes::Bytes::from_static(b"part")),
        Err(std::io::Error::other("client reset the stream")),
    ]));

    let err = write_multipart_part_native(&backend, &claims, 1, body)
        .await
        .expect_err("a body read error must be rejected");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(err.body, "body read error");
}

/// A native `upload_part` failure is a backend error, `500`, and names no
/// backend detail. Here the session handle was never initiated.
#[tokio::test]
async fn write_multipart_part_native_backend_failure_returns_500() {
    let backend = InMemoryBackend::new("mem");
    let claims = part_claims("mem", "/no-session", 4, "no-such-session".to_owned());

    let err = write_multipart_part_native(&backend, &claims, 1, Body::from(b"four".to_vec()))
        .await
        .expect_err("upload_part into an unknown session must fail");
    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(err.body, "backend error");
}

#[tokio::test]
async fn write_multipart_part_offset_object_oversized_returns_413() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let backend = LocalFsBackend::new("local-fs", dir.path());
    let claims = part_claims("local-fs", "/oversized-offset", 4, String::new());

    let err =
        write_multipart_part_offset_object(&backend, &claims, 1, Body::from(b"longer".to_vec()))
            .await
            .expect_err("oversized part must be rejected");
    assert_eq!(err.status, StatusCode::PAYLOAD_TOO_LARGE);
}

/// Any other offset-object write failure is a backend error, `500`. The
/// backend root is a regular file, so no part can be written under it.
#[tokio::test]
async fn write_multipart_part_offset_object_backend_failure_returns_500() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let root = dir.path().join("not-a-directory");
    std::fs::write(&root, b"").expect("create a file where the root should be");
    let backend = LocalFsBackend::new("local-fs", root);
    let claims = part_claims("local-fs", "/unwritable/part", 4, String::new());

    let err =
        write_multipart_part_offset_object(&backend, &claims, 1, Body::from(b"four".to_vec()))
            .await
            .expect_err("a part under a file root must fail");
    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(err.body, "backend error");
}

#[tokio::test]
async fn report_part_callback_unreachable_returns_502() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a port to release");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);

    let mut state = test_state();
    state.control_base_url = format!("http://{addr}");

    let err = report_part_with_control_plane(
        &state,
        "dummy-token",
        "test-request-id",
        Uuid::nil(),
        Uuid::nil(),
        Uuid::nil(),
        1,
        "etag",
        "deadbeef",
        4,
    )
    .await
    .expect_err("an unreachable control plane must fail the report");
    assert_eq!(err.status, StatusCode::BAD_GATEWAY);
    assert_eq!(err.body, "report failed");
}

#[tokio::test]
async fn upload_returns_502_when_finalize_fails() {
    let (mut state, issuer, _backend) = test_download_state();
    state.control_base_url = failing_control_plane().await;
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let token = upload_token(&issuer, file_id, version_id, "test", "/finalize-fails");

    let response = build_router(state, DEFAULT_MAX_BODY_BYTES)
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/upload/{file_id}/{version_id}?fs-token={token}"
            ))
            .body(Body::from(b"content".to_vec()))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

/// A part the sidecar refuses to write reaches the client with the write's
/// status, and no report-part callback is made.
#[tokio::test]
async fn multipart_part_returns_413_when_part_write_fails() {
    let (mut state, issuer, backend) = test_download_state();
    state.control_base_url = failing_control_plane().await;
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let backend_path = format!("/{file_id}/{version_id}");
    let backend_handle = backend
        .initiate_multipart(&backend_path)
        .await
        .expect("initiate native multipart session");
    let token = multipart_part_token(
        &issuer,
        file_id,
        version_id,
        "test",
        &backend_path,
        Uuid::now_v7(),
        1,
        0,
        4,
        &backend_handle,
    );

    let response = build_router(state, DEFAULT_MAX_BODY_BYTES)
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/1?fs-token={token}"
            ))
            .body(Body::from(b"longer than four".to_vec()))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

/// A written part whose report-part callback fails reaches the client as
/// `502`, so it retries; the write and the report are idempotent per part.
#[tokio::test]
async fn multipart_part_returns_502_when_report_fails() {
    let (mut state, issuer, backend) = test_download_state();
    state.control_base_url = failing_control_plane().await;
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let backend_path = format!("/{file_id}/{version_id}");
    let backend_handle = backend
        .initiate_multipart(&backend_path)
        .await
        .expect("initiate native multipart session");
    let part = b"part".to_vec();
    let token = multipart_part_token(
        &issuer,
        file_id,
        version_id,
        "test",
        &backend_path,
        Uuid::now_v7(),
        1,
        0,
        part.len() as u64,
        &backend_handle,
    );

    let response = build_router(state, DEFAULT_MAX_BODY_BYTES)
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/1?fs-token={token}"
            ))
            .body(Body::from(part))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    assert_eq!(&body[..], b"report failed");
}

/// Mock control plane answering finalize calls with `statuses` in order (the last one repeats).
/// Returns its base URL and the number of finalize calls received.
async fn scripted_control_plane(statuses: Vec<u16>) -> (String, Arc<AtomicUsize>) {
    scripted_control_plane_with_headers(statuses, "").await
}

/// Like [`scripted_control_plane`], adding `extra_headers` (raw header lines ending in CRLF) to
/// every response.
async fn scripted_control_plane_with_headers(
    statuses: Vec<u16>,
    extra_headers: &'static str,
) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock control plane");
    let addr = listener.local_addr().expect("local addr");
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_clone = Arc::clone(&calls);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            if stream.read(&mut buf).await.is_ok() {
                let n = calls_clone.fetch_add(1, Ordering::SeqCst);
                let status = statuses[n.min(statuses.len() - 1)];
                let response = format!(
                    "HTTP/1.1 {status} X\r\n{extra_headers}content-length: 0\r\nconnection: close\r\n\r\n"
                );
                stream.write_all(response.as_bytes()).await.ok();
            }
        }
    });
    (format!("http://{addr}"), calls)
}

/// State over a shared in-memory backend, plus an `Issuer` its verifier accepts.
fn upload_state(control_base_url: String) -> (SidecarState, Issuer, Arc<InMemoryBackend>) {
    let (mut state, issuer, backend) = test_download_state();
    state.control_base_url = control_base_url;
    (state, issuer, backend)
}

fn put_request(file_id: Uuid, version_id: Uuid, token: &str, body: &'static [u8]) -> Request<Body> {
    Request::put(format!(
        "/api/file-storage-data/v1/upload/{file_id}/{version_id}?fs-token={token}"
    ))
    .body(Body::from(body))
    .expect("valid request")
}

/// One upload token for a fresh `(file, version)` against a router wired to `control_base_url`,
/// so a test can `put` the same token repeatedly.
struct UploadFixture {
    router: axum::Router,
    backend: Arc<InMemoryBackend>,
    file_id: Uuid,
    version_id: Uuid,
    path: String,
    token: String,
}

impl UploadFixture {
    fn new(control_base_url: String) -> Self {
        let (state, issuer, backend) = upload_state(control_base_url);
        let file_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();
        let path = format!("/{file_id}/{version_id}");
        let token = upload_token(&issuer, file_id, version_id, "test", &path);
        Self {
            router: build_router(state, DEFAULT_MAX_BODY_BYTES),
            backend,
            file_id,
            version_id,
            path,
            token,
        }
    }

    async fn put(&self, body: &'static [u8]) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(put_request(
                self.file_id,
                self.version_id,
                &self.token,
                body,
            ))
            .await
            .expect("router call succeeds")
    }

    async fn stored(&self) -> bytes::Bytes {
        self.backend.get(&self.path).await.expect("stored blob")
    }
}

#[tokio::test]
async fn upload_replay_after_publish_is_rejected_with_409() {
    // The control plane accepts the first finalize, then rejects the replay (version already
    // `available` with different content).
    let (cp_url, calls) = scripted_control_plane(vec![204, 409]).await;
    let fx = UploadFixture::new(cp_url);

    let first = fx.put(b"original-bytes").await;
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "first PUT to a fresh backend path must publish successfully"
    );

    let second = fx.put(b"replayed-different-bytes").await;
    assert_eq!(
        second.status(),
        StatusCode::CONFLICT,
        "a replayed PUT rejected by the control plane must get 409, never overwrite"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the replay must still call finalize with its own size/hash"
    );
    assert_eq!(
        &fx.stored().await[..],
        b"original-bytes",
        "a replayed PUT must never overwrite the already-published content"
    );
}

/// A retry with identical content (e.g. the first finalize never landed) converges to `200`
/// with the same bind headers as the original, since the control plane replays its decision.
#[tokio::test]
async fn upload_replay_with_identical_content_and_successful_finalize_returns_200() {
    let (cp_url, calls) = scripted_control_plane_with_headers(
        vec![204],
        "x-fs-bound: true\r\netag: \"content-etag\"\r\n",
    )
    .await;
    let fx = UploadFixture::new(cp_url);

    for attempt in 1..=2 {
        let response = fx.put(b"same-bytes").await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "attempt {attempt}: identical content with a successful finalize must be 200"
        );
        assert_eq!(
            response
                .headers()
                .get("x-fs-bound")
                .expect("X-FS-Bound echoed on every attempt"),
            "true",
            "attempt {attempt}: a converged replay must carry the original bind headers"
        );
        assert_eq!(
            response
                .headers()
                .get(header::ETAG)
                .expect("ETag echoed on every attempt"),
            "\"content-etag\"",
            "attempt {attempt}: a converged replay must carry the original ETag"
        );
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "finalize runs on every attempt"
    );
    assert_eq!(&fx.stored().await[..], b"same-bytes");
}

/// A control-plane 5xx on a replay is not a rejection: it keeps the first-upload status (`502`).
#[tokio::test]
async fn upload_replay_with_control_plane_5xx_returns_502_not_409() {
    let (cp_url, _calls) = scripted_control_plane(vec![500]).await;
    let fx = UploadFixture::new(cp_url);

    // First PUT publishes but its finalize fails: 502. The replay hits `created: false`.
    for attempt in 1..=2 {
        assert_eq!(
            fx.put(b"some-bytes").await.status(),
            StatusCode::BAD_GATEWAY,
            "attempt {attempt}: a finalize 5xx must stay 502 on both the first PUT and the replay"
        );
    }
}

#[tokio::test]
async fn upload_exact_size_mismatch_returns_400_and_deletes_created_object() {
    let (state, issuer, backend) = test_download_state();

    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    let claims = Claims {
        op: Op::Put,
        file_id,
        version_id,
        backend_id: "test".to_owned(),
        backend_path: path.clone(),
        exp: OffsetDateTime::now_utc().unix_timestamp() + 60,
        upload: UploadConstraints {
            exact_size: Some(999), // deliberately wrong: the body below is 11 bytes
            ..UploadConstraints::default()
        },
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };
    let token = issuer
        .issue(claims, OffsetDateTime::now_utc())
        .expect("issue upload token");

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(put_request(file_id, version_id, &token, b"hello world"))
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let exists = backend
        .exists(&path)
        .await
        .expect("backend exists check succeeds");
    assert!(
        !exists,
        "the object this request itself created must be cleaned up after a \
         post-validation failure, not left poisoning the immutable path"
    );
}

/// Without a control plane a first upload is `200`, but a replay is `409`: nothing can confirm
/// it matches the stored object, and the replay took no effect.
#[tokio::test]
async fn upload_replay_without_control_plane_returns_409() {
    let fx = UploadFixture::new(String::new());

    assert_eq!(fx.put(b"original-bytes").await.status(), StatusCode::OK);
    assert_eq!(
        fx.put(b"original-bytes").await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(&fx.stored().await[..], b"original-bytes");
}

async fn mock_http_response(raw_response: Vec<u8>) -> reqwest::Response {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _n = stream.read(&mut buf).await;
            stream.write_all(&raw_response).await.ok();
        }
    });

    reqwest::Client::new()
        .get(format!("http://{addr}"))
        .send()
        .await
        .expect("mock server responds")
}

/// `interpret_finalize_response`: success statuses echo the bind headers (`bound`, `etag`,
/// `current_etag`) and anything else maps to `502`.
#[tokio::test]
async fn interpret_finalize_response_echoes_bind_headers_or_maps_error_to_bad_gateway() {
    // (row, raw response, expected `(bound, etag, current_etag)`; `None` = expect Err(502))
    type Echo = (
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
    );
    type Row = (&'static str, Vec<u8>, Option<Echo>);
    let mut non_utf8 = b"HTTP/1.1 204 No Content\r\nx-fs-bound: ".to_vec();
    non_utf8.extend_from_slice(&[0xFF, 0xFE]); // not valid UTF-8
    non_utf8.extend_from_slice(b"\r\ncontent-length: 0\r\n\r\n");
    let rows: Vec<Row> = vec![
        (
            // Won auto-bind CAS: bound + etag echoed, no current_etag.
            "bound forwards bound and etag",
            b"HTTP/1.1 204 No Content\r\nx-fs-bound: true\r\netag: \"abc123\"\r\ncontent-length: 0\r\n\r\n"
                .to_vec(),
            Some((Some("true"), Some("\"abc123\""), None)),
        ),
        (
            // Lost auto-bind CAS: bound + current_etag echoed, no new etag (nothing was bound).
            "conflict forwards current etag",
            b"HTTP/1.1 204 No Content\r\nx-fs-bound: conflict\r\nx-fs-current-etag: \"xyz789\"\r\n\
              content-length: 0\r\n\r\n"
                .to_vec(),
            Some((Some("conflict"), None, Some("\"xyz789\""))),
        ),
        (
            "manual mode has no bind headers",
            b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n".to_vec(),
            Some((None, None, None)),
        ),
        (
            // A header value that fails to_str() must read as absent, not propagate garbage.
            "non-utf8 header value is treated as absent",
            non_utf8,
            Some((None, None, None)),
        ),
        (
            "error status maps to bad gateway",
            b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 5\r\n\r\noops!".to_vec(),
            None,
        ),
    ];

    for (row, raw, expected) in rows {
        let resp = mock_http_response(raw).await;
        let result = interpret_finalize_response(resp, Uuid::nil(), Uuid::nil()).await;
        match (result, expected) {
            (Ok(echo), Some((bound, etag, current_etag))) => {
                assert_eq!(echo.bound.as_deref(), bound, "{row}: bound");
                assert_eq!(echo.etag.as_deref(), etag, "{row}: etag");
                assert_eq!(
                    echo.current_etag.as_deref(),
                    current_etag,
                    "{row}: current_etag"
                );
            }
            (Err(err), None) => assert_eq!(err.status, StatusCode::BAD_GATEWAY, "{row}"),
            (Ok(_), None) => panic!("{row}: a non-success status must be interpreted as Err"),
            (Err(err), Some(_)) => panic!(
                "{row}: a success status must be interpreted as Ok, got Err({})",
                err.status
            ),
        }
    }
}

#[tokio::test]
async fn upload_forwards_bind_conflict_headers_from_finalize_callback() {
    let (cp_url, _calls) = scripted_control_plane_with_headers(
        vec![204],
        "x-fs-bound: conflict\r\nx-fs-current-etag: \"current-etag-value\"\r\n",
    )
    .await;
    let fx = UploadFixture::new(cp_url);

    let response = fx.put(b"hello world").await;

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the upload itself succeeds even when the auto-bind CAS is lost -- only the bind, \
         not the upload, conflicted"
    );
    assert_eq!(
        response
            .headers()
            .get("x-fs-bound")
            .expect("X-FS-Bound header present")
            .to_str()
            .expect("valid header value"),
        "conflict"
    );
    assert_eq!(
        response
            .headers()
            .get("x-fs-current-etag")
            .expect("X-FS-Current-ETag header present")
            .to_str()
            .expect("valid header value"),
        "\"current-etag-value\""
    );
    assert!(
        response.headers().get(header::ETAG).is_none(),
        "a lost CAS carries no new ETag -- only X-FS-Current-ETag"
    );
}
