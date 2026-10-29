use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::domain::error::DomainError;
use file_storage::infra::backend::{
    BackendRegistry, InMemoryBackend, LocalFsBackend, StorageBackend,
};
use file_storage::infra::metrics::NoopMetrics;
use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};

use super::{
    CALLBACK_MAX_ATTEMPTS, DEFAULT_MAX_BODY_BYTES, Rejection, SidecarState, backend_failure,
    build_router, finalize_rejection_response, finalize_with_control_plane,
    interpret_finalize_response, report_part_with_control_plane, truncate_utf8,
    write_multipart_part_native, write_multipart_part_offset_object,
};

/// `SidecarState` over `backends` (the first is the registry default, no control plane), plus an
/// `Issuer` its verifier accepts.
fn state_with(backends: Vec<Arc<dyn StorageBackend>>) -> (SidecarState, Issuer) {
    let issuer = Issuer::generate(60).expect("issuer generation");
    let default_id = backends[0].id().to_owned();
    let backends =
        BackendRegistry::new(backends, &default_id).expect("build test backend registry");
    let state = SidecarState {
        verifier: Arc::new(issuer.verifier()),
        backends,
        control_base_url: String::new(),
        internal_token: "test-internal-token".to_owned(),
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
    };
    (state, issuer)
}

fn test_state() -> SidecarState {
    state_with(vec![Arc::new(InMemoryBackend::new("test"))]).0
}

/// State over a fresh in-memory backend `"test"`, calling back to `control_base_url` (empty: no
/// control plane).
fn mem_state(control_base_url: &str) -> (SidecarState, Issuer, Arc<InMemoryBackend>) {
    let backend = Arc::new(InMemoryBackend::new("test"));
    let (mut state, issuer) = state_with(vec![Arc::clone(&backend) as Arc<dyn StorageBackend>]);
    state.control_base_url = control_base_url.to_owned();
    (state, issuer, backend)
}

fn local_fs_state(root: &Path) -> SidecarState {
    state_with(vec![Arc::new(LocalFsBackend::new("local-fs", root))]).0
}

fn sign(issuer: &Issuer, claims: Claims) -> String {
    issuer
        .issue(claims, OffsetDateTime::now_utc())
        .expect("issue token")
}

/// A `(file, version)` pair and its backend path; builds claims, tokens and requests for it.
struct Obj {
    file_id: Uuid,
    version_id: Uuid,
    path: String,
}

impl Obj {
    fn new() -> Self {
        let (file_id, version_id) = (Uuid::now_v7(), Uuid::now_v7());
        Self {
            file_id,
            version_id,
            path: format!("/{file_id}/{version_id}"),
        }
    }

    /// Claims for `op` on `backend_id`, valid for a minute, with every optional claim empty.
    fn claims(&self, op: Op, backend_id: &str) -> Claims {
        Claims {
            op,
            file_id: self.file_id,
            version_id: self.version_id,
            backend_id: backend_id.to_owned(),
            backend_path: self.path.clone(),
            exp: OffsetDateTime::now_utc().unix_timestamp() + 60,
            upload: UploadConstraints::default(),
            multipart: MultipartClaims::default(),
            request_id: "test-request-id".to_owned(),
            content_type: String::new(),
            etag: String::new(),
            bind_on_finalize: false,
        }
    }

    fn part_claims(
        &self,
        backend_id: &str,
        part_number: u32,
        offset: u64,
        size: u64,
        backend_handle: &str,
    ) -> Claims {
        Claims {
            multipart: MultipartClaims {
                upload_id: Uuid::now_v7(),
                part_number,
                offset,
                size,
                backend_handle: backend_handle.to_owned(),
            },
            ..self.claims(Op::MultipartPart, backend_id)
        }
    }

    fn put_request(&self, token: &str, body: &'static [u8]) -> Request<Body> {
        Request::put(format!(
            "/api/file-storage-data/v1/upload/{}/{}?fs-token={token}",
            self.file_id, self.version_id
        ))
        .body(Body::from(body))
        .expect("valid request")
    }

    fn part_request(&self, token: &str, part_number: u32, body: &'static [u8]) -> Request<Body> {
        Request::put(format!(
            "/api/file-storage-data/v1/multipart/{}/{}/parts/{part_number}?fs-token={token}",
            self.file_id, self.version_id
        ))
        .body(Body::from(body))
        .expect("valid request")
    }

    fn download_request(&self, token: &str, range: Option<&str>) -> Request<Body> {
        let mut request = Request::get(format!(
            "/api/file-storage-data/v1/download/{}/{}?fs-token={token}",
            self.file_id, self.version_id
        ));
        if let Some(range) = range {
            request = request.header(header::RANGE, range);
        }
        request.body(Body::empty()).expect("valid request")
    }
}

async fn get_text(router: &axum::Router, uri: &str) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(
            Request::get(uri)
                .body(Body::empty())
                .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    (
        status,
        String::from_utf8(body.to_vec()).expect("valid utf8 body"),
    )
}

#[tokio::test]
async fn sidecar_healthz_and_readyz_return_200_when_backends_ready() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let router = build_router(local_fs_state(dir.path()), DEFAULT_MAX_BODY_BYTES);

    assert_eq!(get_text(&router, "/healthz").await.0, StatusCode::OK);
    assert_eq!(
        get_text(&router, "/readyz").await,
        (StatusCode::OK, "ready".to_owned())
    );
}

#[tokio::test]
async fn sidecar_readyz_returns_503_when_backend_root_missing() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let missing_root = dir.path().join("does-not-exist");
    // `dir` is dropped too, so the parent of `missing_root` is gone as well.
    drop(dir);

    let router = build_router(local_fs_state(&missing_root), DEFAULT_MAX_BODY_BYTES);
    let (status, body_text) = get_text(&router, "/readyz").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
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

/// A client whose per-attempt timeout is short, so a control plane that never answers trips it.
/// The only waiting in these tests is this client's own timeout (and the callback's fixed retry
/// delay); the tests set no timers of their own.
fn impatient_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_millis(200))
        .build()
        .expect("client build")
}

/// A timed-out attempt is retried: the control plane hangs on the first request and answers the
/// second.
#[tokio::test]
async fn finalize_callback_retries_a_timed_out_attempt() {
    let cp = mock_control_plane(vec![
        Reply {
            hang: true,
            ..Reply::default()
        },
        Reply {
            status: 204,
            ..Reply::default()
        },
    ])
    .await;
    let mut state = test_state();
    state.http = impatient_client();
    state.control_base_url = cp.url.clone();

    finalize_callback(&state)
        .await
        .unwrap_or_else(|e| panic!("finalize must succeed once it retries: {}", e.status));
    assert!(cp.calls() >= 2, "the hung attempt must have been retried");
}

/// A control plane that never answers does not hang the callback: every attempt times out,
/// the retries are exhausted and the client-facing error is a generic `502`.
#[tokio::test]
async fn finalize_callback_gives_up_after_every_attempt_times_out() {
    let cp = mock_control_plane(vec![Reply {
        hang: true,
        ..Reply::default()
    }])
    .await;
    let mut state = test_state();
    state.http = impatient_client();
    state.control_base_url = cp.url.clone();

    let Err(rejection) = finalize_callback(&state).await else {
        panic!("finalize must fail when the control plane never responds");
    };
    assert_eq!(rejection.status, StatusCode::BAD_GATEWAY);
    assert_eq!(rejection.body, "finalize failed");
    assert_eq!(cp.calls(), CALLBACK_MAX_ATTEMPTS as usize);
}

/// The finalize callback with dummy ids and hash.
async fn finalize_callback(state: &SidecarState) -> Result<super::FinalizeEcho, super::Rejection> {
    finalize_with_control_plane(
        state,
        "dummy-token",
        "test-request-id",
        Uuid::nil(),
        Uuid::nil(),
        0,
        "deadbeef",
    )
    .await
}

/// One canned control-plane response.
#[derive(Clone, Copy, Default)]
struct Reply {
    status: u16,
    /// Raw header lines, each ending in CRLF.
    headers: &'static str,
    body: &'static str,
    /// A complete raw response, sent as-is instead of the fields above.
    raw: Option<&'static [u8]>,
    /// Accept the request but never answer (the connection stays open).
    hang: bool,
}

/// A mock control plane answering requests with its replies in order (the last one repeats).
struct MockControlPlane {
    url: String,
    /// Raw text of each request received.
    requests: Arc<Mutex<Vec<String>>>,
}

impl MockControlPlane {
    fn calls(&self) -> usize {
        self.requests.lock().expect("lock").len()
    }
}

async fn mock_control_plane(replies: Vec<Reply>) -> MockControlPlane {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock control plane");
    let addr = listener.local_addr().expect("local addr");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    tokio::spawn(async move {
        let mut hung = Vec::new();
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            if let Ok(n) = stream.read(&mut buf).await {
                let reply = {
                    let mut seen = seen.lock().expect("lock");
                    let reply = replies[seen.len().min(replies.len() - 1)];
                    seen.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                    reply
                };
                if reply.hang {
                    hung.push(stream);
                    continue;
                }
                let response = reply.raw.map_or_else(
                    || {
                        format!(
                            "HTTP/1.1 {} X\r\n{}content-length: {}\r\nconnection: close\r\n\r\n{}",
                            reply.status,
                            reply.headers,
                            reply.body.len(),
                            reply.body
                        )
                        .into_bytes()
                    },
                    <[u8]>::to_vec,
                );
                stream.write_all(&response).await.ok();
            }
        }
    });
    MockControlPlane {
        url: format!("http://{addr}"),
        requests,
    }
}

/// Mock control plane answering with `statuses` in order (the last one repeats), no headers or body.
async fn scripted_control_plane(statuses: Vec<u16>) -> MockControlPlane {
    mock_control_plane(
        statuses
            .into_iter()
            .map(|status| Reply {
                status,
                ..Reply::default()
            })
            .collect(),
    )
    .await
}

/// A download request through the router: the status, headers and body.
async fn get_download(
    seeded: bool,
    range: Option<&str>,
    content_type: &str,
    etag: &str,
) -> (StatusCode, axum::http::HeaderMap, bytes::Bytes) {
    let (state, issuer, backend) = mem_state("");
    let obj = Obj::new();
    if seeded {
        backend
            .put(&obj.path, bytes::Bytes::from_static(b"hello world")) // 11 bytes
            .await
            .expect("seed blob");
    }
    let token = sign(
        &issuer,
        Claims {
            content_type: content_type.to_owned(),
            etag: etag.to_owned(),
            ..obj.claims(Op::Get, "test")
        },
    );
    let response = build_router(state, DEFAULT_MAX_BODY_BYTES)
        .oneshot(obj.download_request(&token, range))
        .await
        .expect("router call succeeds");
    let (status, headers) = (response.status(), response.headers().clone());
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    (status, headers, body)
}

/// A download row: the request (blob present?, `Range`, token `content_type`/`etag` claims) and the
/// expected response (a `None` `content_range` / empty `content_type` is not checked; `etag` is
/// checked on `200`/`206` only, where `None` means absent).
struct DownloadCase {
    name: &'static str,
    seeded: bool,
    range: Option<&'static str>,
    token_content_type: &'static str,
    token_etag: &'static str,
    status: StatusCode,
    content_range: Option<&'static str>,
    body: Option<&'static [u8]>,
    content_type: &'static str,
    etag: Option<&'static str>,
}

/// Download: `206`/`416`/`404` range handling, and the `content_type`/`etag` claims echoed as
/// headers (old tokens with empty claims fall back to `application/octet-stream` and no `ETag`).
#[tokio::test]
async fn download_handles_ranges_and_echoes_claims() {
    let cases = [
        DownloadCase {
            name: "sub-range with claims",
            seeded: true,
            range: Some("bytes=0-4"),
            token_content_type: "text/plain",
            token_etag: "\"deadbeef\"",
            status: StatusCode::PARTIAL_CONTENT,
            content_range: Some("bytes 0-4/11"),
            body: Some(b"hello"),
            content_type: "text/plain",
            etag: Some("\"deadbeef\""),
        },
        DownloadCase {
            name: "missing blob is 404, not 416",
            seeded: false,
            range: Some("bytes=0-4"),
            token_content_type: "",
            token_etag: "",
            status: StatusCode::NOT_FOUND,
            content_range: None,
            body: None,
            content_type: "",
            etag: None,
        },
        DownloadCase {
            name: "range past the end",
            seeded: true,
            range: Some("bytes=100-200"),
            token_content_type: "",
            token_etag: "",
            status: StatusCode::RANGE_NOT_SATISFIABLE,
            content_range: Some("bytes */11"),
            body: None,
            content_type: "",
            etag: None,
        },
        DownloadCase {
            name: "full download with claims",
            seeded: true,
            range: None,
            token_content_type: "image/png",
            token_etag: "\"abc123\"",
            status: StatusCode::OK,
            content_range: None,
            body: Some(b"hello world"),
            content_type: "image/png",
            etag: Some("\"abc123\""),
        },
        DownloadCase {
            name: "old token without claims",
            seeded: true,
            range: None,
            token_content_type: "",
            token_etag: "",
            status: StatusCode::OK,
            content_range: None,
            body: Some(b"hello world"),
            content_type: "application/octet-stream",
            etag: None,
        },
    ];

    for case in cases {
        let name = case.name;
        let (status, headers, body) = get_download(
            case.seeded,
            case.range,
            case.token_content_type,
            case.token_etag,
        )
        .await;
        assert_eq!(status, case.status, "{name}: status");
        let header_str = |h| {
            headers
                .get(h)
                .map(|v| v.to_str().expect("valid header value"))
        };
        if let Some(content_range) = case.content_range {
            assert_eq!(
                header_str(header::CONTENT_RANGE),
                Some(content_range),
                "{name}: Content-Range"
            );
        }
        if let Some(expected) = case.body {
            assert_eq!(&body[..], expected, "{name}: body");
        }
        if status.is_success() {
            assert_eq!(
                header_str(header::CONTENT_TYPE),
                Some(case.content_type),
                "{name}: Content-Type"
            );
            assert_eq!(header_str(header::ETAG), case.etag, "{name}: ETag");
        }
    }
}

/// The callback builder must send the token in `x-fs-internal-token`.
#[tokio::test]
async fn finalize_callback_sends_internal_token_header() {
    let cp = scripted_control_plane(vec![200]).await;
    let mut state = test_state();
    state.control_base_url = cp.url.clone();
    state.internal_token = "interim-shared-secret".to_owned();

    assert!(
        finalize_callback(&state).await.is_ok(),
        "finalize must succeed against the mock 200 OK response"
    );

    let requests = cp.requests.lock().expect("lock");
    let request_text = requests.first().expect("mock control plane got a request");
    assert!(
        request_text
            .to_lowercase()
            .contains("x-fs-internal-token: interim-shared-secret"),
        "finalize callback must carry the configured x-fs-internal-token header: {request_text}"
    );
}

/// `upload_multipart_part` must call the backend's native `upload_part` for `multipart_native`
/// backends, not the offset-object fallback (else `complete_multipart` would fail).
#[tokio::test]
async fn sidecar_multipart_native_backend_dispatches_to_upload_part() {
    let backend = Arc::new(InMemoryBackend::new("mem"));
    let (state, issuer) = state_with(vec![Arc::clone(&backend) as Arc<dyn StorageBackend>]);
    let obj = Obj::new();
    // Mirror `initiate_multipart_upload`: initiate on the backend, mint the part token with it.
    let handle = backend
        .initiate_multipart(&obj.path)
        .await
        .expect("initiate native multipart session");
    let part: &'static [u8] = b"first-part-bytes";
    let token = sign(
        &issuer,
        obj.part_claims("mem", 1, 0, part.len() as u64, &handle),
    );

    let response = build_router(state, DEFAULT_MAX_BODY_BYTES)
        .oneshot(obj.part_request(&token, 1, part))
        .await
        .expect("router call succeeds");
    assert_eq!(response.status(), StatusCode::OK, "part PUT must succeed");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read part response body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("part response is JSON");
    let etag = json["etag"].as_str().expect("part response has an etag");

    // Completing the native session succeeds only if the part landed via `upload_part`.
    let hash = file_storage::infra::content::hash::digest_to_array(
        file_storage::infra::content::hash::sha256(part),
    );
    backend
        .complete_multipart(&obj.path, &handle, &[(1, 0, hash, etag.to_owned())])
        .await
        .expect("complete native multipart session - the part must be real");
    let assembled = backend.get(&obj.path).await.expect("read assembled object");
    assert_eq!(&assembled[..], part);
}

/// A body whose client resets the stream after four bytes.
fn reset_stream_body() -> Body {
    Body::from_stream(futures::stream::iter([
        Ok(bytes::Bytes::from_static(b"part")),
        Err(std::io::Error::other("client reset the stream")),
    ]))
}

/// What a part write is fed.
#[derive(Clone, Copy)]
enum Payload {
    Bytes(&'static [u8]),
    /// A client that resets the stream after four bytes.
    ResetStream,
}

/// `write_multipart_part_*` rejections, on both the native and the offset-object path: an
/// undersized part is `400` (a client size mismatch, not an over-limit body), an oversized one
/// `413`, a body read error `400`, and a backend failure `500` naming no backend detail.
#[tokio::test]
async fn write_multipart_part_rejections() {
    // (row, token size, payload, backend broken?, expected status, expected body (None: not checked))
    type Row = (
        &'static str,
        u64,
        Payload,
        bool,
        StatusCode,
        Option<&'static str>,
    );
    let rows: [Row; 4] = [
        (
            "undersized",
            10,
            Payload::Bytes(b"short"),
            false,
            StatusCode::BAD_REQUEST,
            None,
        ),
        (
            "oversized",
            4,
            Payload::Bytes(b"longer"),
            false,
            StatusCode::PAYLOAD_TOO_LARGE,
            None,
        ),
        (
            "body read error",
            8,
            Payload::ResetStream,
            false,
            StatusCode::BAD_REQUEST,
            Some("body read error"),
        ),
        (
            "backend failure",
            4,
            Payload::Bytes(b"four"),
            true,
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("backend error"),
        ),
    ];

    let dir = tempfile::tempdir().expect("create temp dir");
    // A regular file as the root: no part can be written under it.
    let file_root = dir.path().join("not-a-directory");
    std::fs::write(&file_root, b"").expect("create a file where the root should be");

    for native in [true, false] {
        for (row, size, payload, broken, status, expected_body) in rows {
            let label = format!(
                "{} / {row}",
                if native { "native" } else { "offset-object" }
            );
            let obj = Obj::new();
            let backend: Arc<dyn StorageBackend> = match (native, broken) {
                (true, _) => Arc::new(InMemoryBackend::new("mem")),
                (false, false) => Arc::new(LocalFsBackend::new("local-fs", dir.path())),
                (false, true) => Arc::new(LocalFsBackend::new("local-fs", &file_root)),
            };
            // The native session is real, except for the backend-failure row (unknown session).
            let handle = match (native, broken) {
                (true, false) => backend
                    .initiate_multipart(&obj.path)
                    .await
                    .expect("initiate native multipart session"),
                (true, true) => "no-such-session".to_owned(),
                (false, _) => String::new(),
            };
            let claims = obj.part_claims(backend.id(), 1, 0, size, &handle);
            let body = match payload {
                Payload::Bytes(bytes) => Body::from(bytes),
                Payload::ResetStream => reset_stream_body(),
            };

            let result = if native {
                write_multipart_part_native(backend.as_ref(), &claims, 1, body).await
            } else {
                write_multipart_part_offset_object(backend.as_ref(), &claims, 1, body).await
            };
            let err = result.expect_err(&label);
            assert_eq!(err.status, status, "{label}");
            if let Some(expected_body) = expected_body {
                assert_eq!(err.body, expected_body, "{label}");
            }
        }
    }
}

/// Each upload goes to the backend named by the token's `claims.backend_id`.
#[tokio::test]
async fn sidecar_resolves_backend_by_claims_backend_id() {
    let backend_a = Arc::new(InMemoryBackend::new("local-fs"));
    let backend_b = Arc::new(InMemoryBackend::new("other"));
    let (state, issuer) = state_with(vec![
        Arc::clone(&backend_a) as Arc<dyn StorageBackend>,
        Arc::clone(&backend_b) as Arc<dyn StorageBackend>,
    ]);
    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);

    let (obj_a, obj_b) = (Obj::new(), Obj::new());
    let token_a = sign(&issuer, obj_a.claims(Op::Put, "local-fs"));
    let token_b = sign(&issuer, obj_b.claims(Op::Put, "other"));
    for (obj, token, body) in [
        (&obj_a, &token_a, b"bytes-for-local-fs" as &'static [u8]),
        (&obj_b, &token_b, b"bytes-for-other"),
    ] {
        let response = router
            .clone()
            .oneshot(obj.put_request(token, body))
            .await
            .expect("router call succeeds");
        assert_eq!(response.status(), StatusCode::OK);
    }

    // Each blob is in its own backend only.
    assert_eq!(
        &backend_a.get(&obj_a.path).await.expect("get a")[..],
        b"bytes-for-local-fs"
    );
    assert_eq!(
        &backend_b.get(&obj_b.path).await.expect("get b")[..],
        b"bytes-for-other"
    );
    assert!(!backend_a.exists(&obj_b.path).await.expect("exists"));
    assert!(!backend_b.exists(&obj_a.path).await.expect("exists"));
}

/// A control plane that drops the connection without answering fails the report: a generic `502`.
#[tokio::test]
async fn report_part_callback_transport_failure_returns_502() {
    let cp = mock_control_plane(vec![Reply {
        raw: Some(b""),
        ..Reply::default()
    }])
    .await;
    let mut state = test_state();
    state.control_base_url = cp.url.clone();

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

/// PUTs `body` to a native-session part route (token size 4) wired to a control plane that answers
/// `500`; returns the response status and body, and how many callbacks the control plane got.
async fn put_part_via_router(body: &'static [u8]) -> (StatusCode, bytes::Bytes, usize) {
    let cp = scripted_control_plane(vec![500]).await;
    let (state, issuer, backend) = mem_state(&cp.url);
    let obj = Obj::new();
    let handle = backend
        .initiate_multipart(&obj.path)
        .await
        .expect("initiate native multipart session");
    let token = sign(&issuer, obj.part_claims("test", 1, 0, 4, &handle));

    let response = build_router(state, DEFAULT_MAX_BODY_BYTES)
        .oneshot(obj.part_request(&token, 1, body))
        .await
        .expect("router call succeeds");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    (status, body, cp.calls())
}

/// A part the sidecar refuses to write reaches the client with the write's status and no
/// report-part callback is made; a written part whose report-part callback fails reaches the client
/// as `502`, so it retries (the write and the report are idempotent per part).
#[tokio::test]
async fn multipart_part_maps_write_and_report_failures() {
    let (status, _, callbacks) = put_part_via_router(b"longer than four").await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(callbacks, 0, "no report-part callback for a refused write");

    let (status, body, _) = put_part_via_router(b"part").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(&body[..], b"report failed");
}

/// One upload token for a fresh `(file, version)` against a router wired to `control_base_url`,
/// so a test can `put` the same token repeatedly.
struct UploadFixture {
    router: axum::Router,
    backend: Arc<InMemoryBackend>,
    obj: Obj,
    token: String,
}

impl UploadFixture {
    fn new(control_base_url: &str) -> Self {
        let (state, issuer, backend) = mem_state(control_base_url);
        let obj = Obj::new();
        let token = sign(&issuer, obj.claims(Op::Put, "test"));
        Self {
            router: build_router(state, DEFAULT_MAX_BODY_BYTES),
            backend,
            obj,
            token,
        }
    }

    async fn put(&self, body: &'static [u8]) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(self.obj.put_request(&self.token, body))
            .await
            .expect("router call succeeds")
    }

    async fn stored(&self) -> bytes::Bytes {
        self.backend.get(&self.obj.path).await.expect("stored blob")
    }
}

#[tokio::test]
async fn upload_replay_after_publish_is_rejected_with_409() {
    // The control plane accepts the first finalize. The replay carries different bytes, so the
    // sidecar refuses it itself, before any finalize call.
    let cp = scripted_control_plane(vec![204, 409]).await;
    let fx = UploadFixture::new(&cp.url);

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
        cp.calls(),
        1,
        "a replay with different bytes is refused before finalize (no finalize call)"
    );
    assert_eq!(
        &fx.stored().await[..],
        b"original-bytes",
        "a replayed PUT must never overwrite the already-published content"
    );
}

/// A retry with identical content (e.g. the first finalize never landed) converges to `200`
/// with the same bind headers as the original, since the control plane replays its decision. The
/// bind headers the finalize callback returns are forwarded: `x-fs-bound` + `etag` when the bind
/// won, `x-fs-bound` + `x-fs-current-etag` (and no new `etag`) when the CAS was lost -- the upload
/// itself still succeeds.
#[tokio::test]
async fn upload_replay_with_identical_content_and_successful_finalize_returns_200() {
    // (row, control-plane bind headers, expected forwarded (header, value) pairs, header that must be absent)
    type Row = (
        &'static str,
        &'static str,
        [(&'static str, &'static str); 2],
        &'static str,
    );
    let rows: [Row; 2] = [
        (
            "bound",
            "x-fs-bound: bound\r\netag: \"content-etag\"\r\n",
            [("x-fs-bound", "bound"), ("etag", "\"content-etag\"")],
            "x-fs-current-etag",
        ),
        (
            "conflict",
            "x-fs-bound: conflict\r\nx-fs-current-etag: \"current-etag-value\"\r\n",
            [
                ("x-fs-bound", "conflict"),
                ("x-fs-current-etag", "\"current-etag-value\""),
            ],
            "etag",
        ),
    ];

    for (row, headers, expected, absent) in rows {
        let cp = mock_control_plane(vec![Reply {
            status: 204,
            headers,
            ..Reply::default()
        }])
        .await;
        let fx = UploadFixture::new(&cp.url);

        for attempt in 1..=2 {
            let response = fx.put(b"same-bytes").await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{row}, attempt {attempt}: identical content with a successful finalize must be 200"
            );
            for (name, value) in expected {
                assert_eq!(
                    response.headers().get(name),
                    Some(&header::HeaderValue::from_static(value)),
                    "{row}, attempt {attempt}: a converged replay must carry the original {name}"
                );
            }
            assert!(
                response.headers().get(absent).is_none(),
                "{row}, attempt {attempt}: {absent} must not be forwarded"
            );
        }
        assert_eq!(cp.calls(), 2, "{row}: finalize runs on every attempt");
        assert_eq!(&fx.stored().await[..], b"same-bytes");
    }
}

/// A control-plane 5xx on a replay is not a rejection: it keeps the first-upload status (`502`).
#[tokio::test]
async fn upload_replay_with_control_plane_5xx_returns_502_not_409() {
    let cp = scripted_control_plane(vec![500]).await;
    let fx = UploadFixture::new(&cp.url);

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
async fn upload_body_read_error_returns_400_and_publishes_nothing() {
    let (state, issuer, backend) = mem_state("");
    let obj = Obj::new();
    let token = sign(&issuer, obj.claims(Op::Put, "test"));
    let mut request = obj.put_request(&token, b"");
    *request.body_mut() = reset_stream_body();

    let response = build_router(state, DEFAULT_MAX_BODY_BYTES)
        .oneshot(request)
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    assert_eq!(&body[..], b"body read error");
    assert!(
        !backend.exists(&obj.path).await.expect("exists check"),
        "a broken body must not leave a published object"
    );
}

#[tokio::test]
async fn upload_exact_size_mismatch_returns_400_and_deletes_created_object() {
    let (state, issuer, backend) = mem_state("");
    let obj = Obj::new();
    let token = sign(
        &issuer,
        Claims {
            upload: UploadConstraints {
                exact_size: Some(999), // deliberately wrong: the body below is 11 bytes
                ..UploadConstraints::default()
            },
            ..obj.claims(Op::Put, "test")
        },
    );

    let response = build_router(state, DEFAULT_MAX_BODY_BYTES)
        .oneshot(obj.put_request(&token, b"hello world"))
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let exists = backend
        .exists(&obj.path)
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
    let fx = UploadFixture::new("");

    assert_eq!(fx.put(b"original-bytes").await.status(), StatusCode::OK);
    assert_eq!(
        fx.put(b"original-bytes").await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(&fx.stored().await[..], b"original-bytes");
}

/// `interpret_finalize_response`: success statuses read the bind headers (`bound`, `etag`,
/// `current_etag`; the router-level forwarding is covered by the replay tests) and anything else
/// maps to `502`.
#[tokio::test]
async fn interpret_finalize_response_reads_bind_headers_or_maps_error_to_bad_gateway() {
    // (row, raw response, expected `(bound, etag, current_etag)`; `None` = expect Err(502))
    type Echo = (
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
    );
    #[allow(clippy::type_complexity)]
    let rows: [(&str, &'static [u8], Option<Echo>); 4] = [
        (
            "bound headers are read",
            b"HTTP/1.1 204 No Content\r\nx-fs-bound: bound\r\netag: \"abc123\"\r\ncontent-length: 0\r\n\r\n",
            Some((Some("bound"), Some("\"abc123\""), None)),
        ),
        (
            "manual mode has no bind headers",
            b"HTTP/1.1 204 No Content\r\ncontent-length: 0\r\n\r\n",
            Some((None, None, None)),
        ),
        (
            // A header value that fails to_str() must read as absent, not propagate garbage.
            "non-utf8 header value is treated as absent",
            b"HTTP/1.1 204 No Content\r\nx-fs-bound: \xFF\xFE\r\ncontent-length: 0\r\n\r\n",
            Some((None, None, None)),
        ),
        (
            "error status maps to bad gateway",
            b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 5\r\n\r\noops!",
            None,
        ),
    ];

    for (row, raw, expected) in rows {
        let cp = mock_control_plane(vec![Reply {
            raw: Some(raw),
            ..Reply::default()
        }])
        .await;
        let resp = reqwest::Client::new()
            .get(&cp.url)
            .send()
            .await
            .expect("mock server responds");
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

/// The status and text body of a response.
async fn status_and_text(response: axum::response::Response) -> (StatusCode, String) {
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// A control plane that answers `status` (a `400` carries a short text body).
fn reply_with(status: u16) -> Reply {
    Reply {
        status,
        body: if status == 400 {
            "size not allowed"
        } else {
            ""
        },
        ..Reply::default()
    }
}

/// Control plane `400` on the first write: the client gets `400` with its message and the object
/// is removed; control plane `404` (version deleted concurrently): `404`, object removed;
/// `403`: `502`, the object is kept and the same bytes retried once the control plane accepts
/// finalize give `200`.
#[tokio::test]
async fn upload_finalize_rejection_maps_status_and_object_fate_on_first_write() {
    // (control-plane replies: the first rejection, then the retry; expected client status;
    // whether the object survives the first PUT)
    for (statuses, expected, kept) in [
        ([400u16, 200], StatusCode::BAD_REQUEST, false),
        ([404, 200], StatusCode::NOT_FOUND, false),
        ([403, 200], StatusCode::BAD_GATEWAY, true),
    ] {
        let cp = mock_control_plane(statuses.iter().map(|s| reply_with(*s)).collect()).await;
        let fx = UploadFixture::new(&cp.url);

        let (status, body) = status_and_text(fx.put(b"same").await).await;
        assert_eq!(
            status, expected,
            "first write, control plane {}",
            statuses[0]
        );
        if statuses[0] == 400 {
            assert_eq!(
                body, "size not allowed",
                "the control-plane message is relayed"
            );
        }
        assert_eq!(
            fx.backend.exists(&fx.obj.path).await.expect("exists"),
            kept,
            "control plane {}: object fate",
            statuses[0]
        );

        // A corrected retry (the object is gone) or the same bytes (kept) succeed.
        assert_eq!(fx.put(b"same").await.status(), StatusCode::OK);
        assert!(fx.backend.exists(&fx.obj.path).await.expect("exists"));
    }
}

/// A replay (object already stored) whose finalize gets `429` is `502`, not `409`; a `400`
/// on the replay is the `409` and the object stays.
#[tokio::test]
async fn upload_replay_finalize_429_is_502_and_400_is_409() {
    let cp = scripted_control_plane(vec![429, 400]).await;
    let fx = UploadFixture::new(&cp.url);
    fx.backend
        .put(&fx.obj.path, bytes::Bytes::from_static(b"same"))
        .await
        .expect("seed");

    assert_eq!(fx.put(b"same").await.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(fx.put(b"same").await.status(), StatusCode::CONFLICT);
    assert_eq!(&fx.stored().await[..], b"same");
}

/// A transient backend error is `503` + `Retry-After: 5`; a permanent one stays `500`.
#[test]
fn backend_failure_maps_transient_to_503_with_retry_after() {
    let transient = backend_failure(&DomainError::backend_unavailable("s3", "timeout"), "test");
    assert_eq!(transient.status, StatusCode::SERVICE_UNAVAILABLE);
    let resp = transient.into_response();
    assert_eq!(
        resp.headers().get(header::RETRY_AFTER).expect("header"),
        "5"
    );

    let permanent = backend_failure(&DomainError::backend("s3", "bad config"), "test");
    assert_eq!(permanent.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(permanent.body, "backend error");
    assert!(
        permanent
            .into_response()
            .headers()
            .get(header::RETRY_AFTER)
            .is_none()
    );
}

/// The client response for a control plane answering `status` with `body` to a finalize of an
/// object this request created.
async fn finalize_rejection_body(status: u16, body: &'static str) -> (StatusCode, String) {
    let cp = mock_control_plane(vec![Reply {
        status,
        body,
        ..Reply::default()
    }])
    .await;
    let mut state = test_state();
    state.control_base_url = cp.url;
    let Err(rejection) = finalize_callback(&state).await else {
        panic!("finalize must fail");
    };
    let backend = InMemoryBackend::new("test");
    let obj = Obj::new();
    let claims = obj.part_claims("test", 1, 0, 1, "");
    status_and_text(finalize_rejection_response(&backend, &claims, true, rejection).await).await
}

#[tokio::test]
async fn finalize_400_relays_problem_detail_only() {
    let (status, body) = finalize_rejection_body(
        400,
        r#"{"type":"x","title":"Bad","status":400,"detail":"size exceeds policy","trace_id":"t"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, "size exceeds policy");

    let (status, body) = finalize_rejection_body(400, "plain text reason").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, "plain text reason");
}

#[tokio::test]
async fn finalize_other_statuses_do_not_leak_control_plane_body() {
    for status in [401u16, 403, 500, 503] {
        let (code, body) = finalize_rejection_body(status, "control-plane-secret").await;
        assert_eq!(code, StatusCode::BAD_GATEWAY, "status {status}");
        assert_eq!(body, "finalize failed", "status {status}");
    }
}

/// A lost-but-not-created upload whose version vanished (`404`) is not turned into a `404`
/// for the client: the object belongs to another request, so it stays a generic `502`.
#[tokio::test]
async fn finalize_404_without_created_is_502_with_generic_body() {
    let backend = InMemoryBackend::new("test");
    let claims = Obj::new().part_claims("test", 1, 0, 1, "");
    let rejection = Rejection {
        status: StatusCode::BAD_GATEWAY,
        body: "finalize failed".into(),
        control_plane_status: Some(StatusCode::NOT_FOUND),
        retry_after_secs: None,
    };
    let response = finalize_rejection_response(&backend, &claims, false, rejection).await;
    let (status, body) = status_and_text(response).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body, "finalize failed");
}

#[test]
fn truncate_utf8_cuts_on_a_char_boundary() {
    // Each U+044F is 2 bytes: a cut at 3 bytes must back off to 2.
    let s = "\u{44f}".repeat(3);
    assert_eq!(truncate_utf8(s.clone(), 3), "\u{44f}");
    assert_eq!(truncate_utf8(s, 4), "\u{44f}\u{44f}");
    assert_eq!(truncate_utf8("abc".to_owned(), 10), "abc");
}
