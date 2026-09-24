//! Unit tests for the data-plane sidecar binary ([`super`]).
//!
//! Kept in a sibling `_tests.rs` file per the `de1101_tests_in_separate_files`
//! repo lint. Linked into `sidecar.rs` via
//! `#[path = "sidecar_tests.rs"] mod tests;`, so the module sees `sidecar.rs`
//! as `super`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use futures::StreamExt;
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use toolkit_utils::SecretString;
use tower::ServiceExt;
use uuid::Uuid;

use file_storage::domain::error::DomainError;
use file_storage::infra::backend::{
    BackendCapabilities, BackendRegistry, InMemoryBackend, LocalFsBackend, StorageBackend,
};
use file_storage::infra::metrics::NoopMetrics;
use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};

use super::{
    DEFAULT_MAX_BODY_BYTES, DEFAULT_MAX_CONCURRENT_PART_UPLOADS, SidecarState, TokenQuery,
    build_config, build_router, check_part_buffer_budget, dedupe_public_keys, extract_token,
    finalize_with_control_plane, idle_timeout_stream, parse_optional, parse_public_key_list,
    write_multipart_part_native, write_multipart_part_offset_object,
};

/// A part-upload concurrency semaphore sized at the production default
/// -- every `SidecarState` literal below needs this field, and most
/// tests don't care about its value, only the concurrency-limiter tests
/// further down do (they build their own with a size of `1`).
fn test_part_upload_semaphore() -> Arc<tokio::sync::Semaphore> {
    Arc::new(tokio::sync::Semaphore::new(
        DEFAULT_MAX_CONCURRENT_PART_UPLOADS,
    ))
}

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
        internal_token: None,
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
        part_upload_semaphore: test_part_upload_semaphore(),
        body_idle_timeout: None,
        callback_retry_budget: Duration::from_secs(10),
    }
}

fn token_query(token: Option<&str>) -> TokenQuery {
    TokenQuery {
        fs_token: token.map(SecretString::new),
    }
}

fn header_map_with_token(token: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(token) = token {
        headers.insert(
            "x-fs-token",
            HeaderValue::from_str(token).expect("valid header value"),
        );
    }
    headers
}

/// Neither transport carries a token: unchanged `401` behavior.
#[test]
fn extract_token_missing_both_is_unauthorized() {
    let err = extract_token(&token_query(None), &header_map_with_token(None))
        .expect_err("must reject when neither transport carries a token");
    assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
}

/// Only the query param carries a token: used as-is, same as before this fix.
#[test]
fn extract_token_query_only_is_used() {
    let token = extract_token(
        &token_query(Some("query-token")),
        &header_map_with_token(None),
    )
    .expect("a lone query token must be accepted");
    assert_eq!(token, "query-token");
}

/// Only the header carries a token: used as-is, same as before this fix.
#[test]
fn extract_token_header_only_is_used() {
    let token = extract_token(
        &token_query(None),
        &header_map_with_token(Some("header-token")),
    )
    .expect("a lone header token must be accepted");
    assert_eq!(token, "header-token");
}

/// Both transports present and agree: accepted, using the shared value.
#[test]
fn extract_token_both_present_and_matching_is_accepted() {
    let token = extract_token(
        &token_query(Some("same-token")),
        &header_map_with_token(Some("same-token")),
    )
    .expect("matching query and header tokens must be accepted");
    assert_eq!(token, "same-token");
}

/// Both transports present but *disagree*: rejected with `400`, not silently
/// resolved by query-wins-header-loses precedence.
#[test]
fn extract_token_both_present_and_conflicting_is_bad_request() {
    let err = extract_token(
        &token_query(Some("query-token")),
        &header_map_with_token(Some("header-token")),
    )
    .expect_err("conflicting query/header tokens must be rejected");
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
}

/// Route-level regression: a real `download` request carrying two different,
/// individually well-formed tokens via query and header must be refused
/// `400` before either one ever reaches the verifier.
#[tokio::test]
async fn download_rejects_conflicting_query_and_header_tokens() {
    let issuer = Issuer::generate(60).expect("issuer generation");
    let backends = BackendRegistry::new(
        vec![Arc::new(InMemoryBackend::new("test")) as Arc<dyn StorageBackend>],
        "test",
    )
    .expect("build test backend registry");
    let mut state = test_state();
    state.verifier = Arc::new(issuer.verifier());
    state.backends = backends;

    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let base_claims = Claims {
        op: Op::Get,
        file_id,
        version_id,
        backend_id: "test".to_owned(),
        backend_path: format!("/{file_id}/{version_id}"),
        exp: OffsetDateTime::now_utc().unix_timestamp() + 60,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };
    let query_token = issuer
        .issue(base_claims.clone(), OffsetDateTime::now_utc())
        .expect("issue query token");
    // A second, distinctly-signed token for the same operation -- validly
    // signed on its own (differs only in `request_id`, so it doesn't
    // serialize identically and sign to the same bytes), but not equal to
    // `query_token`.
    let header_token = issuer
        .issue(
            Claims {
                request_id: "other-request-id".to_owned(),
                ..base_claims
            },
            OffsetDateTime::now_utc(),
        )
        .expect("issue header token");
    assert_ne!(
        query_token, header_token,
        "test premise: the two tokens must actually differ"
    );

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::get(format!(
                "/api/file-storage-data/v1/download/{file_id}/{version_id}?fs-token={query_token}"
            ))
            .header("x-fs-token", header_token)
            .body(Body::empty())
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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

/// P2 1.6: `/readyz` must report `200 "ready"` when every configured
/// backend's `is_ready` succeeds — here a `LocalFsBackend` rooted at a real,
/// existing temp directory.
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
        internal_token: None,
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
        part_upload_semaphore: test_part_upload_semaphore(),
        body_idle_timeout: None,
        callback_retry_budget: Duration::from_secs(10),
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

/// P2 1.6: `/readyz` must report `503` naming the failing backend id (and
/// only the id — never the underlying OS error string) when a backend's root
/// has gone missing (e.g. an unmounted volume).
#[tokio::test]
async fn sidecar_readyz_returns_503_when_backend_root_missing() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let missing_root = dir.path().join("does-not-exist");
    // `dir` itself is dropped here too, so `missing_root`'s parent is gone as
    // well — belt-and-braces against the root ever accidentally existing.
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
        internal_token: None,
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
        part_upload_semaphore: test_part_upload_semaphore(),
        body_idle_timeout: None,
        callback_retry_budget: Duration::from_secs(10),
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

/// Regression guard for step 1.2(a): a body over axum's blanket 2 MiB
/// `DefaultBodyLimit` must reach the handler (and be rejected there for an
/// unrelated reason — missing token) rather than being rejected by the
/// transport layer with a bare `413` before any handler code runs.
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
    // No `fs-token` supplied: the handler itself rejects with 401. If the
    // `DefaultBodyLimit` layer were still capped at 2 MiB, this would be a
    // `413` from axum's extractor instead, before `extract_token` ever runs.
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// P2 1.5: a control plane that accepts the TCP connection but never
/// responds must not hang the finalize callback indefinitely — each
/// attempt's client-configured `reqwest` timeout must trip and, since a
/// timeout is itself retried up to `CALLBACK_MAX_ATTEMPTS` times, the
/// call must still return `Err` well within the test's own budget (a
/// small per-attempt timeout keeps `attempts * timeout + retry delays`
/// comfortably under that budget). The `tokio::time::timeout` wrapping
/// the call belongs to the *test*, not production: it exists so this
/// test fails fast (instead of hanging the suite) if the production
/// timeout regresses.
#[tokio::test]
async fn finalize_callback_times_out_within_configured_bound() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock listener");
    let addr = listener.local_addr().expect("local addr");

    // Accept connections but never write a response, so the client's
    // read times out rather than erroring immediately.
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

/// P2 1.5: a transient connection-refused failure on the first attempt
/// must be retried, and the callback must succeed once the control plane
/// becomes reachable — without the caller ever seeing the transient
/// failure.
#[tokio::test]
async fn finalize_callback_retries_on_connection_refused_then_succeeds() {
    // Reserve a free port, then release it immediately: connecting to it
    // while nothing is listening reliably yields ECONNREFUSED on loopback.
    let probe = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind probe listener");
    let addr = probe.local_addr().expect("local addr");
    drop(probe);

    let accepted = Arc::new(AtomicUsize::new(0));
    let accepted_clone = Arc::clone(&accepted);

    // Give the first (connection-refused) attempt time to fail before a
    // real listener claims the same address and answers 200 OK.
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

/// Build a `SidecarState` wired to a fresh `InMemoryBackend`, plus the
/// `Issuer` that must be used to mint tokens the state's verifier accepts
/// (P2 1.11's download tests need to mint real `op = get` tokens, unlike
/// the pre-existing tests above which only exercise the missing-token
/// path).
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
        internal_token: None,
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
        part_upload_semaphore: test_part_upload_semaphore(),
        body_idle_timeout: None,
        callback_retry_budget: Duration::from_secs(10),
    };
    (state, issuer, backend)
}

/// Mint a signed `op = get` download token for `(file_id, version_id, backend_path)`,
/// carrying no `content_type`/`etag` claims (P2 1.11 old-token-compat shape;
/// most pre-existing download tests only care about range/status behavior).
fn download_token(issuer: &Issuer, file_id: Uuid, version_id: Uuid, backend_path: &str) -> String {
    download_token_with_meta(issuer, file_id, version_id, backend_path, "", "")
}

/// Mint a signed `op = get` download token for `(file_id, version_id,
/// backend_path)`, additionally carrying `content_type`/`etag` claims (P2
/// 1.11). Passing empty strings for both reproduces a pre-1.11 token.
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

/// Which error [`FaultyReadBackend`]'s `get_stream`/`get_range_stream`
/// return instead of delegating to the real inner backend.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadFault {
    /// The race `download_range`/`download_whole` must map to `503` +
    /// `Retry-After`.
    Conflict,
    /// Any other backend error -- must still map to `500`.
    Other,
}

/// A [`StorageBackend`] wrapper around a real [`InMemoryBackend`] that makes
/// `get_stream`/`get_range_stream` fail with a chosen [`ReadFault`], while
/// every other method -- crucially `stat`, which `download`'s handler calls
/// first to resolve existence/size before ever reaching `get_stream`/
/// `get_range_stream` -- delegates to the inner backend unchanged. Used to
/// exercise `download_range`/`download_whole`'s `Conflict`-vs-other-error
/// response mapping without needing a real stat/read race.
struct FaultyReadBackend {
    inner: InMemoryBackend,
    fault: ReadFault,
}

impl FaultyReadBackend {
    fn fault_error(&self) -> DomainError {
        match self.fault {
            ReadFault::Conflict => DomainError::conflict(
                "object changed size before it could be read (test fault)".to_owned(),
            ),
            ReadFault::Other => {
                DomainError::backend(self.inner.id(), "simulated I/O fault (test fault)")
            }
        }
    }
}

#[async_trait]
impl StorageBackend for FaultyReadBackend {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.inner.capabilities()
    }

    async fn put(&self, path: &str, bytes: Bytes) -> Result<(), DomainError> {
        self.inner.put(path, bytes).await
    }

    async fn get(&self, path: &str) -> Result<Bytes, DomainError> {
        self.inner.get(path).await
    }

    async fn delete(&self, path: &str) -> Result<(), DomainError> {
        self.inner.delete(path).await
    }

    async fn exists(&self, path: &str) -> Result<bool, DomainError> {
        self.inner.exists(path).await
    }

    async fn stat(&self, path: &str) -> Result<Option<u64>, DomainError> {
        self.inner.stat(path).await
    }

    async fn get_stream(
        &self,
        _path: &str,
        _expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        Err(self.fault_error())
    }

    async fn get_range_stream(
        &self,
        _path: &str,
        _range: file_storage_sdk::ByteRange,
        _expected_len: u64,
    ) -> Result<futures::stream::BoxStream<'static, std::io::Result<Bytes>>, DomainError> {
        Err(self.fault_error())
    }
}

/// Build a download-ready `SidecarState` whose single "test" backend is
/// [`FaultyReadBackend`] wrapping a real [`InMemoryBackend`] seeded with
/// `path` -> `body`, so `stat` (and thus the download handler's existence
/// check) succeeds while the actual read fails with `fault`.
async fn test_faulty_download_state(
    path: &str,
    body: &'static [u8],
    fault: ReadFault,
) -> (SidecarState, Issuer) {
    let issuer = Issuer::generate(60).expect("issuer generation");
    let inner = InMemoryBackend::new("test");
    inner
        .put(path, Bytes::from_static(body))
        .await
        .expect("seed blob into inner backend");
    let backend: Arc<dyn StorageBackend> = Arc::new(FaultyReadBackend { inner, fault });
    let backends =
        BackendRegistry::new(vec![backend], "test").expect("build test backend registry");
    let state = SidecarState {
        verifier: Arc::new(issuer.verifier()),
        backends,
        control_base_url: String::new(),
        internal_token: None,
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
        part_upload_semaphore: test_part_upload_semaphore(),
        body_idle_timeout: None,
        callback_retry_budget: Duration::from_secs(10),
    };
    (state, issuer)
}

/// A `Conflict` from the backend's `get_stream` (whole-object `GET`, no
/// `Range` header) -- the object changed between the download handler's
/// `stat` and `download_whole`'s own read -- must map to `503 Service
/// Unavailable` with `Retry-After`, not the old blanket `500`.
#[tokio::test]
async fn download_whole_conflict_from_backend_returns_503_with_retry_after() {
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    let (state, issuer) =
        test_faulty_download_state(&path, b"hello world", ReadFault::Conflict).await;
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

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .expect("Retry-After header present on 503")
            .to_str()
            .expect("valid header value"),
        "1"
    );
}

/// The range-request counterpart: a `Conflict` from `get_range_stream` must
/// also map to `503` + `Retry-After`.
#[tokio::test]
async fn download_range_conflict_from_backend_returns_503_with_retry_after() {
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    let (state, issuer) =
        test_faulty_download_state(&path, b"hello world", ReadFault::Conflict).await;
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

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .expect("Retry-After header present on 503")
            .to_str()
            .expect("valid header value"),
        "1"
    );
}

/// Any *other* backend error (not `Conflict`) must still map to the old
/// blanket `500`, for both the whole-object and range paths -- the `503`
/// carve-out above must not swallow genuine I/O faults.
#[tokio::test]
async fn download_other_backend_error_still_returns_500() {
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    let (state, issuer) = test_faulty_download_state(&path, b"hello world", ReadFault::Other).await;
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

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        response.headers().get(header::RETRY_AFTER).is_none(),
        "a genuine backend error must not carry a Retry-After hint"
    );
}

/// P2 1.11: a sub-range `GET` must come back as `206` with a correct
/// `Content-Range: bytes {start}-{end}/{total}` and the exact byte slice
/// requested — previously the sidecar returned `206` with no
/// `Content-Range` at all, which corrupts resumable-download reassembly.
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

/// t15: a whole-object `GET` (no `Range` header) must send a body whose
/// actual length exactly matches its own already-set `Content-Length`
/// header — the header and the streamed body must never be able to
/// disagree, since `download_whole` now threads the same `total` it puts in
/// the header through to `backend.get_stream` as `expected_len`.
#[tokio::test]
async fn download_whole_content_length_matches_actual_body_length() {
    let (state, issuer, backend) = test_download_state();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    let content = b"whole-object download body used to check Content-Length";
    backend
        .put(&path, bytes::Bytes::from_static(content))
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
    let content_length: usize = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .expect("Content-Length header present on 200")
        .to_str()
        .expect("valid header value")
        .parse()
        .expect("Content-Length is a valid integer");
    assert_eq!(content_length, content.len());

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    assert_eq!(
        body.len(),
        content_length,
        "actual body length must equal the already-sent Content-Length"
    );
    assert_eq!(&body[..], content);
}

/// P2 1.11: a range request against a blob that was never written must be
/// `404`, not the pre-fix behavior of folding every backend error
/// (including a missing blob) into `416`.
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

/// P2 1.11: a range past the end of a blob that *does* exist is a genuine
/// RFC 9110 §14.4 unsatisfiable-range condition — `416` with a
/// `Content-Range: bytes */{total}` header, distinct from the
/// missing-blob `404` case above.
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
    // api.md: "every download response includes Accept-Ranges" — 416 is not
    // an exception.
    let accept_ranges = response
        .headers()
        .get(header::ACCEPT_RANGES)
        .expect("Accept-Ranges header present on 416")
        .to_str()
        .expect("valid header value");
    assert_eq!(accept_ranges, "bytes");
}

/// RFC 9110 §14.1.1: `bytes=5-2` (last-byte-pos < first-byte-pos) is
/// syntactically invalid, not merely unsatisfiable — it MUST be ignored by
/// the recipient, i.e. served as a full-body `200`, never a `416`.
#[tokio::test]
async fn download_inverted_range_is_ignored_and_returns_full_body() {
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
            .header(header::RANGE, "bytes=5-2")
            .body(Body::empty())
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    assert_eq!(&body[..], b"hello world");
}

/// P2 1.11: a whole-file (`200`) download response must echo the
/// `content_type`/`etag` claims the control plane stamped onto the token at
/// download-URL-issuance time, as real `Content-Type`/`ETag` headers — the
/// sidecar has no DB access, so the token is its only source for either.
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

/// Same assertion as above, on the `206 Partial Content` path
/// (`download_range`) — the two response builders must not diverge on how
/// they resolve `Content-Type`/`ETag` from the claims.
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

/// Old-token compatibility (P2 1.11): a token minted before `content_type`/
/// `etag` existed (both empty, the shape `download_token` — and every
/// pre-1.11 token — produces) must fall back to
/// [`super::FALLBACK_CONTENT_TYPE`] and omit `ETag` entirely, not error out.
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

/// P2 1.11: when the control plane's finalize endpoint returns an error
/// response, the sidecar must not forward the raw upstream status/body or
/// the internal control-plane address to the uploading client — only the
/// server-side `tracing::error!` (asserted indirectly here by checking
/// what does *not* appear in the client-facing body) may carry that
/// detail.
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

    let Err(response) = outcome else {
        panic!("finalize must fail when the control plane returns an error status");
    };
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

/// P2 0.1 remaining: when `SidecarState::internal_token` (the
/// `FS_SIDECAR_INTERNAL_TOKEN`-derived field) is set, the callback request
/// builder (`post_with_retry`, shared by `finalize_with_control_plane` and
/// `report_part_with_control_plane`) must attach it as the
/// `x-fs-internal-token` header. Captured off a raw mock TCP listener since
/// this is a wire-level assertion, not a `reqwest`-side one.
#[tokio::test]
async fn finalize_callback_sends_internal_token_header_when_configured() {
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
    state.internal_token = Some("interim-shared-secret".to_owned());

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

/// Companion negative control: with `internal_token` unset (the default —
/// no `FS_SIDECAR_INTERNAL_TOKEN` configured), the callback must not send the
/// header at all, so it works unmodified against a control plane that has
/// the internal-credential check disabled.
#[tokio::test]
async fn finalize_callback_omits_internal_token_header_when_not_configured() {
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

    // `test_state()` leaves `internal_token: None`.
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
    assert!(
        outcome.is_ok(),
        "finalize must succeed against the mock 200 OK response"
    );

    let request_text = rx.await.expect("mock control plane must receive a request");
    assert!(
        !request_text.to_lowercase().contains("x-fs-internal-token"),
        "finalize callback must not send x-fs-internal-token when unconfigured: {request_text}"
    );
}

/// Regression test: the callback retry loop (`post_with_retry`) must bound
/// its *total* wall-clock time by `SidecarState::callback_retry_budget`,
/// not let each retry attempt take its own full window (which would let a
/// hung/unreachable control plane hold a client's upload request open for up
/// to `CALLBACK_MAX_ATTEMPTS` times the configured budget).
///
/// The mock control plane below accepts every TCP connection the retry loop
/// opens but never writes a response, so the request genuinely hangs --
/// `test_state()`'s plain `reqwest::Client::new()` carries no per-request
/// timeout of its own, so nothing but the retry loop's own overall budget
/// can ever end this call. Before the budget wrap this would hang
/// indefinitely (bounded only by the OS's own socket keepalive, if any);
/// with it, the call returns within a small margin over
/// `callback_retry_budget`.
#[tokio::test]
async fn finalize_callback_total_time_bounded_by_retry_budget() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock control plane");
    let addr = listener.local_addr().expect("local addr");

    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            // Never write a response and never let `stream` drop (a drop
            // would close the connection, surfacing as a read/reset error
            // instead of the genuine hang this test needs) -- leaking the
            // fd for the remainder of this short-lived test process is fine.
            std::mem::forget(stream);
        }
    });

    let mut state = test_state();
    state.control_base_url = format!("http://{addr}");
    let budget = Duration::from_millis(300);
    state.callback_retry_budget = budget;

    let start = std::time::Instant::now();
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
    let elapsed = start.elapsed();

    let Err(response) = outcome else {
        panic!(
            "finalize must fail once the retry budget is exhausted against a hung control plane"
        );
    };
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        elapsed < budget * 3,
        "total retry time must stay close to the configured budget ({budget:?}), not blow up to \
         several times it by giving every attempt its own full window; took {elapsed:?}"
    );
}

/// Mint a signed `op = put` upload token for `(file_id, version_id, backend_id, backend_path)`.
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

/// Mint a signed `op = multipart_part` token.
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

/// P2 1.7 Stage 6 regression: `upload_multipart_part` must dispatch to the
/// backend's own `upload_part` (native multipart) for a
/// `multipart_native` backend, instead of unconditionally falling back to
/// the local-fs-style offset-object model. That bug was silent until the
/// S3 e2e suite (`testing/e2e/suites/file_storage/lifecycle_s3/`) surfaced
/// it: `CompleteMultipartUpload` 500s against a real S3-compatible
/// endpoint because no part was ever uploaded via a real `UploadPart`
/// call. `InMemoryBackend` is `multipart_native: true` too, so this
/// regression is caught here without needing a live S3 test double: if
/// `upload_multipart_part` used the offset-object fallback instead, the
/// final `complete_multipart` call below would fail (zero real parts
/// would exist in the backend's native multipart session).
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
        internal_token: None,
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
        part_upload_semaphore: test_part_upload_semaphore(),
        body_idle_timeout: None,
        callback_retry_budget: Duration::from_secs(10),
    };

    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let backend_path = format!("/{file_id}/{version_id}");
    let upload_id = Uuid::now_v7();

    // Mirrors `initiate_multipart_upload` (domain service): call the
    // backend's own `initiate_multipart` up front and mint each per-part
    // token with the resulting handle (`MultipartClaims::backend_handle`).
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

    // Complete the native multipart session directly against the
    // backend (mirrors what `complete_multipart_upload` does
    // server-side) — this only succeeds if both parts above actually
    // landed via `upload_part`, proving the dispatch fix. ADR-0006:
    // `complete_multipart` takes `(part_number, offset, part_hash, etag)`
    // and returns the offset-manifest + its root.
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

    // The returned root is the offset-manifest composite (ADR-0006 mode 2),
    // independently reproducible from the per-part digests/offsets.
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

/// Review nitpick fix (PR #4184): an *undersized* multipart part
/// (client streamed fewer bytes than the token's `size` claim) is a
/// client mismatch — `400 Bad Request` — not `413 Payload Too Large`
/// (413 is reserved for a body that *exceeds* a limit, which the
/// mid-stream guard above already catches). Covers the `multipart_native`
/// write path (`write_multipart_part_native`).
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

    let err =
        write_multipart_part_native(&backend, &claims, 1, Body::from(b"short".to_vec()), None)
            .await
            .expect_err("undersized part must be rejected");
    assert_eq!(
        err.status(),
        StatusCode::BAD_REQUEST,
        "undersized part is a client size mismatch, not an over-limit body"
    );
}

/// Same fix as above, for the non-native offset-object write path
/// (`write_multipart_part_offset_object`, e.g. `LocalFsBackend`).
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

    let err = write_multipart_part_offset_object(
        &backend,
        &claims,
        1,
        Body::from(b"short".to_vec()),
        None,
    )
    .await
    .expect_err("undersized part must be rejected");
    assert_eq!(
        err.status(),
        StatusCode::BAD_REQUEST,
        "undersized part is a client size mismatch, not an over-limit body"
    );
}

/// P2 remediation (replay-`PUT` overwrite fix, HIGH-severity immutability
/// bug): a valid signed `PUT` token stays usable until `exp`, but replaying
/// it after the backend object already exists must never overwrite the
/// live bytes. The sidecar's backend publish is create-exclusive, so the
/// second `PUT` (even with completely different bytes, using the SAME
/// still-valid token) must be rejected with `409 Conflict` — not silently
/// accepted with `200`, and not a `502`, which would misleadingly suggest a
/// transient failure rather than "this write never took effect". This test
/// runs in the sidecar's dev/no-control-plane mode (`control_base_url`
/// empty), which is the conservative branch: with no control plane to
/// consult, a rejected publish always reports `409` (see `upload`'s doc
/// comment for the full decision table covering a real control plane too).
#[tokio::test]
async fn upload_replay_after_publish_is_rejected_with_409() {
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
        internal_token: None,
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
        part_upload_semaphore: test_part_upload_semaphore(),
        body_idle_timeout: None,
        callback_retry_budget: Duration::from_secs(10),
    };

    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    let token = upload_token(&issuer, file_id, version_id, "test", &path);

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);

    let first = router
        .clone()
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/upload/{file_id}/{version_id}?fs-token={token}"
            ))
            .body(Body::from(b"original-bytes".to_vec()))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(
        first.status(),
        StatusCode::OK,
        "first PUT to a fresh backend path must publish successfully"
    );

    // Replay the SAME still-valid token with DIFFERENT bytes.
    let second = router
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/upload/{file_id}/{version_id}?fs-token={token}"
            ))
            .body(Body::from(b"replayed-different-bytes".to_vec()))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(
        second.status(),
        StatusCode::CONFLICT,
        "a replayed PUT against an already-published path must be rejected with 409, \
         never overwrite it"
    );

    // The backend must still hold the FIRST attempt's bytes, untouched.
    let stored = backend.get(&path).await.expect("blob must still exist");
    assert_eq!(
        &stored[..],
        b"original-bytes",
        "a replayed PUT must never overwrite the already-published content"
    );
}

/// Stage 5 regression test (P2 1.7.2): the sidecar must dispatch each
/// upload to the backend named by the verified token's `claims.backend_id`
/// — not always the same hardcoded backend, which was the bug this stage
/// fixes (`SidecarState` previously held a single `backend` field, ignored
/// by every handler's `claims.backend_id`). Uses two differently-tagged
/// in-memory backends so no S3 test double is needed.
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
        internal_token: None,
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
        part_upload_semaphore: test_part_upload_semaphore(),
        body_idle_timeout: None,
        callback_retry_budget: Duration::from_secs(10),
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

    // Assert the bytes landed in the backend the TOKEN named, not always
    // the same one — via each backend's own `list_paths()`/`get()`.
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

// ── `parse_optional` env-var parsing (fail-fast on typos) ───────────────────

/// Unset (`raw = None`) must fall back to the default without error.
#[test]
fn parse_optional_unset_uses_default() {
    let value: u64 = parse_optional("FS_SIDECAR_TEST_VAR", None, 10).expect("unset uses default");
    assert_eq!(value, 10);
}

/// A well-formed value overrides the default.
#[test]
fn parse_optional_valid_value_overrides_default() {
    let value: u64 = parse_optional("FS_SIDECAR_TEST_VAR", Some("42".to_owned()), 10)
        .expect("valid value parses");
    assert_eq!(value, 42);
}

/// A malformed value (e.g. `"5GB"` for a byte count, or any non-numeric
/// typo) must fail fast with an error naming the variable, NOT silently fall
/// back to the default — that used to be exactly how a typo like
/// `FS_SIDECAR_MAX_BODY_BYTES=5GB` disappeared without a trace.
#[test]
fn parse_optional_invalid_value_errors() {
    let err = parse_optional::<u64>("FS_SIDECAR_MAX_BODY_BYTES", Some("5GB".to_owned()), 10)
        .expect_err("malformed value must fail fast, not fall back to the default");
    let msg = err.to_string();
    assert!(
        msg.contains("FS_SIDECAR_MAX_BODY_BYTES"),
        "error should name the offending variable: {msg}"
    );
    assert!(
        msg.contains("5GB"),
        "error should include the offending value: {msg}"
    );
}

/// An empty string is also not a valid `u64` and must error, not silently
/// become the default.
#[test]
fn parse_optional_empty_string_errors() {
    parse_optional::<u64>("FS_SIDECAR_TEST_VAR", Some(String::new()), 10)
        .expect_err("empty string is not a valid u64");
}

// ── `idle_timeout_stream` (T3: idle-timeout on the request body) ───────────

/// A stream that yields one chunk and then hangs forever must time out on
/// the *next* poll once no further chunk arrives within `idle` — the
/// wrapper hands back exactly one `Err(ErrorKind::TimedOut)` and sets the
/// `timed_out` flag, then ends.
#[tokio::test]
async fn idle_timeout_stream_times_out_when_the_stream_goes_quiet() {
    let chunk = bytes::Bytes::from_static(b"hello");
    let stream = futures::stream::once(async move { Ok::<_, std::io::Error>(chunk) })
        .chain(futures::stream::pending::<std::io::Result<bytes::Bytes>>());
    let timed_out = Arc::new(AtomicBool::new(false));
    let mut wrapped = idle_timeout_stream(
        stream,
        Some(Duration::from_millis(50)),
        Arc::clone(&timed_out),
    );

    let first = wrapped
        .next()
        .await
        .expect("stream has a first item")
        .expect("first item is Ok");
    assert_eq!(first, bytes::Bytes::from_static(b"hello"));
    assert!(
        !timed_out.load(Ordering::SeqCst),
        "flag must not be set before the idle deadline"
    );

    let second = wrapped
        .next()
        .await
        .expect("wrapper yields the timeout error instead of hanging forever");
    let err = second.expect_err("must be an idle-timeout error, not a chunk");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        timed_out.load(Ordering::SeqCst),
        "flag must be set once the idle timeout fires"
    );

    // The wrapper terminates the stream right after the timeout error --
    // it never keeps polling a stream that already went idle past the
    // deadline.
    assert!(wrapped.next().await.is_none());
}

/// `idle = None` must disable the guard entirely: the stream passes through
/// unwrapped and the flag is never touched, no matter how long a caller
/// waits between polls.
#[tokio::test]
async fn idle_timeout_stream_disabled_passes_stream_through_unchanged() {
    let items: Vec<std::io::Result<bytes::Bytes>> = vec![
        Ok(bytes::Bytes::from_static(b"a")),
        Ok(bytes::Bytes::from_static(b"b")),
    ];
    let timed_out = Arc::new(AtomicBool::new(false));
    let mut wrapped =
        idle_timeout_stream(futures::stream::iter(items), None, Arc::clone(&timed_out));

    assert_eq!(
        wrapped.next().await.unwrap().unwrap(),
        bytes::Bytes::from_static(b"a")
    );
    assert_eq!(
        wrapped.next().await.unwrap().unwrap(),
        bytes::Bytes::from_static(b"b")
    );
    assert!(wrapped.next().await.is_none());
    assert!(!timed_out.load(Ordering::SeqCst));
}

/// A stream whose pauses between chunks all stay comfortably under `idle`
/// must run to completion with every chunk delivered and no error at all --
/// this is the "slow but alive" case the design deliberately protects (no
/// absolute deadline on the whole stream, only on the gap between chunks).
#[tokio::test]
async fn idle_timeout_stream_tolerates_pauses_under_the_limit() {
    let stream = futures::stream::unfold(0u8, |i| async move {
        if i >= 3 {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        Some((Ok::<_, std::io::Error>(bytes::Bytes::from(vec![i])), i + 1))
    });
    let timed_out = Arc::new(AtomicBool::new(false));
    let mut wrapped = idle_timeout_stream(
        stream,
        Some(Duration::from_millis(200)),
        Arc::clone(&timed_out),
    );

    let mut collected = Vec::new();
    while let Some(item) = wrapped.next().await {
        collected.push(item.expect("no idle timeout expected -- pauses stay under the limit"));
    }
    assert_eq!(
        collected,
        vec![
            bytes::Bytes::from(vec![0u8]),
            bytes::Bytes::from(vec![1u8]),
            bytes::Bytes::from(vec![2u8]),
        ]
    );
    assert!(!timed_out.load(Ordering::SeqCst));
}

// ── `parse_public_key_list` (T5: rotation without outage) ──────────────────

#[test]
fn parse_public_key_list_empty_string_is_empty_vec() {
    assert_eq!(parse_public_key_list("").unwrap(), Vec::<Vec<u8>>::new());
}

/// Elements are trimmed and empty elements (e.g. a stray double/trailing
/// comma) are silently skipped rather than rejected.
#[test]
fn parse_public_key_list_trims_and_skips_empty_elements() {
    let a = URL_SAFE_NO_PAD.encode([1, 2, 3]);
    let b = URL_SAFE_NO_PAD.encode([4, 5, 6]);
    let c = URL_SAFE_NO_PAD.encode([7, 8, 9]);
    let raw = format!("{a}, {b},,{c}");

    let parsed = parse_public_key_list(&raw).expect("valid entries parse");
    assert_eq!(parsed, vec![vec![1, 2, 3], vec![4, 5, 6], vec![7, 8, 9]]);
}

/// An entry that fails to base64url-decode fails the whole parse, naming
/// its 0-based position so a misconfiguration is easy to locate.
#[test]
fn parse_public_key_list_invalid_element_names_its_position() {
    let a = URL_SAFE_NO_PAD.encode([1, 2, 3]);
    let raw = format!("{a}, not-valid-base64!!!");

    let err = parse_public_key_list(&raw).expect_err("second entry is not valid base64url");
    let msg = err.to_string();
    assert!(
        msg.contains("entry #1"),
        "error should name the 0-based position of the bad entry: {msg}"
    );
}

// ── `dedupe_public_keys` ────────────────────────────────────────────────────

/// No duplicates anywhere: nothing is dropped and order is preserved
/// (primary first, then `previous` in order).
#[test]
fn dedupe_public_keys_no_duplicates_drops_nothing() {
    let primary = vec![1, 2, 3];
    let previous = vec![vec![4, 5, 6], vec![7, 8, 9]];

    let (keys, dropped) = dedupe_public_keys(primary.clone(), previous.clone());
    assert_eq!(
        keys,
        vec![primary, previous[0].clone(), previous[1].clone()]
    );
    assert_eq!(dropped, 0);
}

/// The primary repeated inside `previous` is dropped as a duplicate of the
/// primary, which always leads the set.
#[test]
fn dedupe_public_keys_drops_primary_repeated_in_previous() {
    let primary = vec![1, 2, 3];
    let previous = vec![vec![4, 5, 6], primary.clone()];

    let (keys, dropped) = dedupe_public_keys(primary.clone(), previous);
    assert_eq!(keys, vec![primary, vec![4, 5, 6]]);
    assert_eq!(dropped, 1);
}

/// A key repeated within `previous` itself (not just against the primary)
/// is also de-duplicated -- only the first occurrence survives.
#[test]
fn dedupe_public_keys_drops_duplicate_within_previous() {
    let primary = vec![1, 2, 3];
    let b = vec![4, 5, 6];
    let previous = vec![b.clone(), b.clone()];

    let (keys, dropped) = dedupe_public_keys(primary.clone(), previous);
    assert_eq!(keys, vec![primary, b]);
    assert_eq!(dropped, 1);
}

/// Combined case: primary repeated AND a `previous` entry repeated, in the
/// same list -- `[A(primary), B, A, B]` collapses to `[A, B]`, dropping 2.
#[test]
fn dedupe_public_keys_combined_primary_and_previous_repeats() {
    let a = vec![1, 2, 3];
    let b = vec![4, 5, 6];
    let previous = vec![b.clone(), a.clone(), b.clone()];

    let (keys, dropped) = dedupe_public_keys(a.clone(), previous);
    assert_eq!(keys, vec![a, b]);
    assert_eq!(dropped, 2);
}

// ── `build_config` (T10: full startup-config assembly) ──────────────────────
//
// `parse_optional`/`parse_public_key_list`/`dedupe_public_keys` above are
// each unit-tested against their own narrow inputs; the tests below instead
// drive `build_config` itself -- the same env-var-to-`SidecarConfig`
// assembly `main()` calls -- so a wiring regression there (e.g. a zero-value
// check that stops firing once threaded through the real function, or a
// rotation key that stops reaching the `Verifier` it constructs) fails here
// even if each constituent helper still passes on its own.

/// A minimal valid `lookup` env map: just `FS_SIDECAR_PUBLIC_KEY`, set to a
/// freshly generated key -- `build_config`'s one genuinely required
/// variable. Tests clone this and add/override entries for the scenario
/// under test, so a missing case here can never be mistaken for the one this
/// module actually cares about.
fn base_config_env() -> (HashMap<&'static str, String>, Issuer) {
    let issuer = Issuer::generate(60).expect("issuer generation");
    let mut env = HashMap::new();
    env.insert(
        "FS_SIDECAR_PUBLIC_KEY",
        URL_SAFE_NO_PAD.encode(issuer.public_key()),
    );
    (env, issuer)
}

/// Build the `lookup` closure `build_config` expects, backed by an in-memory
/// map instead of the real process environment -- this is the whole point of
/// `build_config` taking `lookup` as a parameter rather than reading
/// `std::env::var` itself.
fn lookup_fn(env: HashMap<&'static str, String>) -> impl Fn(&str) -> Option<String> {
    move |name| env.get(name).cloned()
}

/// `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS=0` must disable the guard (`None`),
/// not become a real (zero-length) timeout that fires on every request.
#[test]
fn build_config_zero_idle_timeout_disables_guard() {
    let (mut env, _issuer) = base_config_env();
    env.insert("FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS", "0".to_owned());

    let config = build_config(lookup_fn(env)).expect("valid config");
    assert!(
        config.body_idle_timeout.is_none(),
        "0 must disable the idle-timeout guard entirely"
    );
}

/// A nonzero `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS` becomes exactly that
/// duration, not silently rounded or ignored.
#[test]
fn build_config_nonzero_idle_timeout_is_some_duration() {
    let (mut env, _issuer) = base_config_env();
    env.insert("FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS", "5".to_owned());

    let config = build_config(lookup_fn(env)).expect("valid config");
    assert_eq!(config.body_idle_timeout, Some(Duration::from_secs(5)));
}

/// `FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS=0` must fail sidecar startup
/// outright -- left to build a zero-permit semaphore, it would silently turn
/// every multipart part upload into an unconditional `503`.
#[test]
fn build_config_zero_concurrency_is_rejected() {
    let (mut env, _issuer) = base_config_env();
    env.insert("FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS", "0".to_owned());

    let err = build_config(lookup_fn(env))
        .expect_err("zero concurrency must fail startup, not silently reject every request");
    assert!(
        err.to_string()
            .contains("FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS"),
        "error should name the offending variable: {err}"
    );
}

/// A `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` entry that decodes to the wrong
/// length must fail startup with an understandable error -- `Verifier`'s own
/// length check, reached through `build_config`, not a silently-dropped or
/// silently-accepted malformed key.
#[test]
fn build_config_malformed_length_previous_key_is_rejected() {
    let (mut env, _issuer) = base_config_env();
    // 8 bytes -- well short of the 32 an Ed25519 public key requires; valid
    // base64, so `parse_public_key_list` lets it through and `Verifier::
    // from_public_keys`'s length check is what actually rejects it.
    env.insert(
        "FS_SIDECAR_PREVIOUS_PUBLIC_KEYS",
        URL_SAFE_NO_PAD.encode([1, 2, 3, 4, 5, 6, 7, 8]),
    );

    let err = build_config(lookup_fn(env))
        .expect_err("a previous key of the wrong length must fail startup");
    let msg = err.to_string();
    assert!(
        msg.contains("length") && msg.contains('8'),
        "error should name the length mismatch, got: {msg}"
    );
}

/// A duplicate key in `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS` (here, a repeat of
/// the primary) must be silently deduped -- accepted, not rejected as a
/// configuration error.
#[test]
fn build_config_duplicate_previous_key_is_deduped_not_rejected() {
    let (mut env, issuer) = base_config_env();
    let primary_b64 = URL_SAFE_NO_PAD.encode(issuer.public_key());
    env.insert(
        "FS_SIDECAR_PREVIOUS_PUBLIC_KEYS",
        format!("{primary_b64},{primary_b64}"),
    );

    let config =
        build_config(lookup_fn(env)).expect("duplicate keys must be deduped, not rejected");
    assert_eq!(
        config.accepted_key_count, 1,
        "the primary and its duplicate collapse to a single accepted key"
    );
    assert_eq!(config.dropped_duplicate_keys, 2);
}

/// End-to-end rotation, through `build_config`'s actual `Verifier` output
/// (not just the constituent parse/dedupe helpers in isolation): a token
/// signed by an old key is accepted while that key is still listed in
/// `FS_SIDECAR_PREVIOUS_PUBLIC_KEYS`, and rejected the moment it is removed.
#[test]
fn build_config_rotation_accepts_old_key_while_listed_then_rejects_once_removed() {
    let (new_env, _new_issuer) = base_config_env();
    let old_issuer = Issuer::generate(60).expect("issuer generation");
    let old_key_b64 = URL_SAFE_NO_PAD.encode(old_issuer.public_key());

    let token = old_issuer
        .issue(
            Claims {
                op: Op::Get,
                file_id: Uuid::now_v7(),
                version_id: Uuid::now_v7(),
                backend_id: "test".to_owned(),
                backend_path: "/irrelevant".to_owned(),
                exp: OffsetDateTime::now_utc().unix_timestamp() + 60,
                upload: UploadConstraints::default(),
                multipart: MultipartClaims::default(),
                request_id: "test-request-id".to_owned(),
                content_type: String::new(),
                etag: String::new(),
                bind_on_finalize: false,
            },
            OffsetDateTime::now_utc(),
        )
        .expect("issue token with old key");

    // During rotation: the old key is still listed as a previous key.
    let mut env_during_rotation = new_env.clone();
    env_during_rotation.insert("FS_SIDECAR_PREVIOUS_PUBLIC_KEYS", old_key_b64);
    let config_during_rotation =
        build_config(lookup_fn(env_during_rotation)).expect("valid config");
    config_during_rotation
        .verifier
        .verify(&token, OffsetDateTime::now_utc())
        .expect("a token signed by the old key must verify while it is still listed");

    // After rotation completes: the old key is removed from the list.
    let config_after_rotation = build_config(lookup_fn(new_env)).expect("valid config");
    config_after_rotation
        .verifier
        .verify(&token, OffsetDateTime::now_utc())
        .expect_err("a token signed by the old key must be rejected once it is no longer listed");
}

// ── `check_part_buffer_budget` (T-worst-case-memory-vs-cgroup-limit) ────────

/// A limit comfortably above the worst case passes.
#[test]
fn check_part_buffer_budget_accepts_limit_above_worst_case() {
    // 2 * 5 GiB = 10 GiB worst case, 32 GiB limit.
    let result = check_part_buffer_budget(2, 5 * 1024 * 1024 * 1024, Some(32 * 1024 * 1024 * 1024));
    assert!(result.is_ok(), "expected Ok, got {result:?}");
}

/// The exact boundary (`worst_case == limit`) must be accepted, not rejected
/// — only a strict excess is a real overcommit.
#[test]
fn check_part_buffer_budget_accepts_limit_equal_to_worst_case() {
    let result = check_part_buffer_budget(2, 5 * 1024 * 1024 * 1024, Some(10 * 1024 * 1024 * 1024));
    assert!(result.is_ok(), "expected Ok, got {result:?}");
}

/// A limit below the worst case (the documented default: 2 * 5 GiB = 10 GiB
/// against a modest 4 GiB sidecar container) must be rejected with a message
/// naming both the worst case and the limit.
#[test]
fn check_part_buffer_budget_rejects_limit_below_worst_case() {
    let result = check_part_buffer_budget(2, 5 * 1024 * 1024 * 1024, Some(4 * 1024 * 1024 * 1024));
    let err = result.expect_err("expected Err for a 10 GiB worst case against a 4 GiB limit");
    assert!(
        err.contains("10737418240"),
        "message should carry the worst-case byte count: {err}"
    );
    assert!(
        err.contains("4294967296"),
        "message should carry the limit byte count: {err}"
    );
}

/// `limit: None` (no cgroup limit could be read, or it reported unlimited)
/// never fails the check, however large the worst case is — this is a
/// best-effort guard, not an authoritative one.
#[test]
fn check_part_buffer_budget_never_fails_with_no_known_limit() {
    let result = check_part_buffer_budget(1000, u64::MAX, None);
    assert!(result.is_ok(), "expected Ok, got {result:?}");
}

/// `max_concurrent * part_size` must not silently wrap on overflow (a
/// pathological configuration, e.g. an operator-supplied huge
/// `FS_SIDECAR_MAX_CONCURRENT_PART_UPLOADS`) — it must saturate instead, so
/// an absurdly large product still compares as "exceeds any real limit"
/// rather than wrapping around to a small, falsely-passing value.
#[test]
fn check_part_buffer_budget_saturates_instead_of_overflowing() {
    let result = check_part_buffer_budget(usize::MAX, u64::MAX, Some(1));
    assert!(
        result.is_err(),
        "an overflowing worst case must saturate to u64::MAX and still exceed any real limit, \
         not wrap around to something that passes"
    );
}

// -- HEAD download ---------------------------------------------------------

/// `HEAD` on an existing object must answer `200` with
/// `Content-Length`/`Accept-Ranges`/`Content-Type` set from the token's
/// claims and the backend's stat, and an EMPTY body -- `download_head` must
/// never stream any content, only stat it.
#[tokio::test]
async fn download_head_existing_object_returns_200_with_empty_body() {
    let (state, issuer, backend) = test_download_state();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    backend
        .put(&path, bytes::Bytes::from_static(b"hello world")) // 11 bytes
        .await
        .expect("seed blob");
    let token = download_token_with_meta(
        &issuer,
        file_id,
        version_id,
        &path,
        "text/plain",
        "\"etag123\"",
    );

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri(format!(
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
            .get(header::CONTENT_LENGTH)
            .expect("Content-Length header present")
            .to_str()
            .expect("valid header value"),
        "11"
    );
    assert_eq!(
        response
            .headers()
            .get(header::ACCEPT_RANGES)
            .expect("Accept-Ranges header present")
            .to_str()
            .expect("valid header value"),
        "bytes"
    );
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .expect("Content-Type header present")
            .to_str()
            .expect("valid header value"),
        "text/plain"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    assert!(
        body.is_empty(),
        "HEAD response body must be empty, got {} byte(s)",
        body.len()
    );
}

/// `HEAD` on a missing object must be `404`, same as `GET`.
#[tokio::test]
async fn download_head_missing_object_returns_404() {
    let (state, issuer, _backend) = test_download_state();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}"); // never written
    let token = download_token(&issuer, file_id, version_id, &path);

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri(format!(
                    "/api/file-storage-data/v1/download/{file_id}/{version_id}?fs-token={token}"
                ))
                .body(Body::empty())
                .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// -- part-upload concurrency limiter ---------------------------------------

/// Build a `SidecarState` wired to a fresh `InMemoryBackend` (`multipart_native`,
/// so `write_multipart_part` takes the permit-guarded branch) with its
/// `part_upload_semaphore` sized at `limit` -- used by the concurrency
/// tests below, which need a small, explicit limit rather than the
/// production default.
fn test_multipart_state_with_limit(limit: usize) -> (SidecarState, Issuer, Arc<InMemoryBackend>) {
    let issuer = Issuer::generate(60).expect("issuer generation");
    let backend = Arc::new(InMemoryBackend::new("mem"));
    let backends =
        BackendRegistry::new(vec![Arc::clone(&backend) as Arc<dyn StorageBackend>], "mem")
            .expect("build test backend registry");
    let state = SidecarState {
        verifier: Arc::new(issuer.verifier()),
        backends,
        control_base_url: String::new(),
        internal_token: None,
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
        part_upload_semaphore: Arc::new(tokio::sync::Semaphore::new(limit)),
        body_idle_timeout: None,
        callback_retry_budget: Duration::from_secs(10),
    };
    (state, issuer, backend)
}

/// With `part_upload_semaphore` exhausted (limit `1`, and
/// the single permit held externally for the duration of this request), a
/// `multipart_native` part-upload request must fail with `503` and a
/// `Retry-After` header rather than hang or silently proceed unbounded.
#[tokio::test]
async fn upload_multipart_part_returns_503_when_semaphore_exhausted() {
    let (state, issuer, backend) = test_multipart_state_with_limit(1);
    let semaphore = Arc::clone(&state.part_upload_semaphore);

    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let backend_path = format!("/{file_id}/{version_id}");
    let backend_handle = backend
        .initiate_multipart(&backend_path)
        .await
        .expect("initiate native multipart session");
    let part = b"exhausted-slot-part".to_vec();
    let token = multipart_part_token(
        &issuer,
        file_id,
        version_id,
        "mem",
        &backend_path,
        Uuid::now_v7(),
        1,
        0,
        part.len() as u64,
        &backend_handle,
    );

    // Hold the only permit so the request below can never acquire one --
    // exercises the `PART_UPLOAD_ACQUIRE_TIMEOUT` bounded-wait-then-503 path.
    let held_permit = Arc::clone(&semaphore)
        .try_acquire_owned()
        .expect("acquire the single permit for the test to hold");

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/1?fs-token={token}"
            ))
            .body(Body::from(part))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .expect("Retry-After header present on 503")
            .to_str()
            .expect("valid header value"),
        "1"
    );

    drop(held_permit);
}

/// Once a `multipart_native` part write completes, its
/// permit must be released before the handler returns -- a second part
/// upload against a `part_upload_semaphore` sized at `1` must succeed right
/// after the first, not wait out `PART_UPLOAD_ACQUIRE_TIMEOUT` and fail.
#[tokio::test]
async fn upload_multipart_part_releases_permit_after_write() {
    let (state, issuer, backend) = test_multipart_state_with_limit(1);

    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let backend_path = format!("/{file_id}/{version_id}");
    let upload_id = Uuid::now_v7();
    let backend_handle = backend
        .initiate_multipart(&backend_path)
        .await
        .expect("initiate native multipart session");

    let part1 = b"first-part".to_vec();
    let part2 = b"second-part".to_vec();
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
            .body(Body::from(part1))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(
        resp1.status(),
        StatusCode::OK,
        "first part upload must succeed"
    );

    // With the semaphore still sized at 1, this only succeeds immediately if
    // the first write's permit was actually released rather than leaked.
    let resp2 = router
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/2?fs-token={token2}"
            ))
            .body(Body::from(part2))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");
    assert_eq!(
        resp2.status(),
        StatusCode::OK,
        "second part upload must succeed once the first write released its permit"
    );
}

// -- post-validation cleanup on a rejected PUT ------------------------------

/// Immutable-path-poisoning fix (`reject_upload_bad_content`): a
/// `PUT` whose streamed bytes fail the post-publish `exact_size` check must
/// answer `400` AND must delete the object it just created --
/// `publish_exclusive` already landed those bytes before the mismatch could
/// be detected, and leaving them in place would permanently poison this
/// immutable path (a corrected retry would itself be rejected as
/// `created: false`).
#[tokio::test]
async fn upload_exact_size_mismatch_returns_400_and_deletes_created_object() {
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
        internal_token: None,
        http: reqwest::Client::new(),
        metrics: Arc::new(NoopMetrics),
        part_upload_semaphore: test_part_upload_semaphore(),
        body_idle_timeout: None,
        callback_retry_budget: Duration::from_secs(10),
    };

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
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/upload/{file_id}/{version_id}?fs-token={token}"
            ))
            .body(Body::from(b"hello world".to_vec())) // 11 bytes, not the claimed 999
            .expect("valid request"),
        )
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

// ── route-level idle-timeout / token-expiry / oversized-part (T11) ─────────

/// `idle_timeout_stream` is unit-tested in isolation above; this drives an
/// actual `PUT /upload` route to prove the wiring: a body that yields one
/// chunk and then goes silent past `FS_SIDECAR_BODY_IDLE_TIMEOUT_SECS` must
/// answer `408 Request Timeout`, and the partial object must never become
/// visible -- `InMemoryBackend::publish_exclusive` only inserts into its
/// blob map after the whole stream drains successfully, which the
/// idle-timeout error path never reaches.
#[tokio::test]
async fn upload_body_idle_timeout_returns_408_and_publishes_nothing() {
    let issuer = Issuer::generate(60).expect("issuer generation");
    let mut state = test_state();
    state.verifier = Arc::new(issuer.verifier());
    state.body_idle_timeout = Some(Duration::from_millis(50));
    let backend = state.backends.get("test").expect("test backend registered");

    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    let token = upload_token(&issuer, file_id, version_id, "test", &path);

    // Yields one chunk immediately, then goes silent well past the 50ms
    // idle timeout configured above -- `upload()` must already have answered
    // 408 long before this second item ever resolves.
    let body_stream = futures::stream::unfold(0u8, |i| async move {
        match i {
            0 => Some((
                Ok::<_, std::io::Error>(Bytes::from_static(b"first-chunk")),
                1,
            )),
            1 => {
                tokio::time::sleep(Duration::from_secs(2)).await;
                Some((Ok(Bytes::from_static(b"never-sent")), 2))
            }
            _ => None,
        }
    });

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/upload/{file_id}/{version_id}?fs-token={token}"
            ))
            .body(Body::from_stream(body_stream))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);

    let stat = backend.stat(&path).await.expect("stat succeeds");
    assert!(
        stat.is_none(),
        "a partial upload aborted by the idle timeout must never become visible to stat"
    );
}

/// The sidecar verifies a token's `exp` exactly once, at the start of the
/// request (`state.verifier.verify(&token, now)` in `upload()`) -- never
/// again once the body is streaming. This drives a real upload whose token
/// expires *while* the (slow but steadily-arriving) body is still being
/// read: verification at the start succeeds because `exp` has not yet
/// passed, and the upload must still complete successfully even though, by
/// the time the body finishes, `now() > claims.exp`.
///
/// `state.control_base_url` is empty (the `test_state()` default), so the
/// finalize callback is a local no-op (`finalize_with_control_plane` returns
/// early) and never makes a network call or re-checks `exp` itself -- this
/// isolates the assertion to the sidecar's own single up-front check. The
/// control-plane side of a token that expires before its *finalize* callback
/// arrives (a different, already-covered scenario) has its own grace-window
/// tests in `tests/finalize_test.rs`:
/// `finalize_with_expired_token_accepted_within_grace` /
/// `finalize_with_expired_token_rejected_beyond_grace` and their
/// `report_part_with_expired_token_*` counterparts.
#[tokio::test]
async fn upload_succeeds_when_token_expires_after_verification_while_body_still_streaming() {
    let issuer = Issuer::generate(60).expect("issuer generation");
    let mut state = test_state();
    state.verifier = Arc::new(issuer.verifier());

    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    let claims = Claims {
        op: Op::Put,
        file_id,
        version_id,
        backend_id: "test".to_owned(),
        backend_path: path.clone(),
        // Expires 2 seconds after issuance -- well before the body below
        // finishes streaming. (Not `+1`: `exp`/`now` are compared at whole-
        // second granularity -- `now.unix_timestamp() >= exp` -- so a
        // 1-second margin can race a second-boundary crossing between
        // issuance and the verify call a few microseconds later; `+2`
        // leaves a full second of slack against that.)
        exp: OffsetDateTime::now_utc().unix_timestamp() + 2,
        upload: UploadConstraints::default(),
        multipart: MultipartClaims::default(),
        request_id: "test-request-id".to_owned(),
        content_type: String::new(),
        etag: String::new(),
        bind_on_finalize: false,
    };
    let token = issuer
        .issue(claims, OffsetDateTime::now_utc())
        .expect("issue upload token");

    // Three chunks, each comfortably spaced under any idle-timeout concern
    // (there is none configured here -- `test_state()`'s default is `None`),
    // whose combined delay pushes well past the token's 2-second `exp`.
    let body_stream = futures::stream::unfold(0u8, |i| async move {
        if i >= 3 {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(900)).await;
        Some((
            Ok::<_, std::io::Error>(Bytes::from_static(b"slow-chunk")),
            i + 1,
        ))
    });

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/upload/{file_id}/{version_id}?fs-token={token}"
            ))
            .body(Body::from_stream(body_stream))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "exp is checked only once, at the start of the request -- an upload already \
         admitted and mid-flight when exp passes must still complete"
    );
}

/// Route-level counterpart to the unit-tested
/// `write_multipart_part_offset_object_undersized_returns_400`: an
/// *oversized* part on the offset-object write path (`LocalFsBackend`,
/// `capabilities().multipart_native == false`) must answer `413`, and the
/// partial `.part.N` object must never become visible --
/// `LocalFsBackend::put_stream` removes its temp file on the mid-stream
/// `max_size` guard (same cleanup as any other write failure; see its own
/// doc comment), so nothing is ever published at that path.
#[tokio::test]
async fn upload_multipart_part_offset_object_oversized_returns_413_and_leaves_no_part_file() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let backend = Arc::new(LocalFsBackend::new("local-fs", dir.path()));
    let backends = BackendRegistry::new(
        vec![Arc::clone(&backend) as Arc<dyn StorageBackend>],
        "local-fs",
    )
    .expect("build test backend registry");
    let issuer = Issuer::generate(60).expect("issuer generation");
    let mut state = test_state();
    state.verifier = Arc::new(issuer.verifier());
    state.backends = backends;

    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let upload_id = Uuid::now_v7();
    let backend_path = format!("/{file_id}/{version_id}");
    // Declared part size (10 bytes) is far smaller than the body actually
    // sent below.
    let token = multipart_part_token(
        &issuer,
        file_id,
        version_id,
        "local-fs",
        &backend_path,
        upload_id,
        1,
        0,
        10,
        "",
    );

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::put(format!(
                "/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/1?fs-token={token}"
            ))
            .body(Body::from(Bytes::from_static(
                b"this part body is far longer than the declared size claim",
            )))
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let part_path = format!("{backend_path}.part.1");
    let stat = backend.stat(&part_path).await.expect("stat succeeds");
    assert!(
        stat.is_none(),
        "an oversized part rejected with 413 must never leave a visible .part.N object"
    );
}

// ── observable metrics + download Content-Length (T12) ─────────────────────

/// Test double for `FileStorageMetricsPort` with observable state -- unlike
/// `NoopMetrics` (used by every other sidecar test in this file), which
/// discards every observation, this accumulates egress/ingress byte totals
/// so a test can assert on what the download/upload handlers actually
/// reported, not just on their HTTP-level behavior.
#[derive(Default)]
struct RecordingMetrics {
    egress_bytes: std::sync::Mutex<f64>,
    ingress_bytes: std::sync::Mutex<f64>,
}

impl RecordingMetrics {
    fn egress_total(&self) -> f64 {
        *self.egress_bytes.lock().expect("lock poisoned")
    }
}

impl file_storage::domain::ports::FileStorageMetricsPort for RecordingMetrics {
    fn record_operation(&self, _op: &str, _result: &str) {}
    fn record_backend_error(&self, _backend_id: &str, _op: &str) {}
    fn record_quota_denied(&self, _op: &str) {}
    fn record_sweep_result(
        &self,
        _abandoned_pending_deleted: u64,
        _abandoned_files_deleted: u64,
        _expired_multipart_aborted: u64,
        _retention_expired_deleted: u64,
        _idempotency_keys_deleted: u64,
    ) {
    }
    fn record_ingress_bytes(&self, bytes: f64) {
        *self.ingress_bytes.lock().expect("lock poisoned") += bytes;
    }
    fn record_egress_bytes(&self, bytes: f64) {
        *self.egress_bytes.lock().expect("lock poisoned") += bytes;
    }
    fn record_request(&self, _route: &str, _method: &str, _status: u16, _latency_ms: f64) {}
}

/// Like [`test_download_state`], but with `metrics` swapped for a
/// caller-supplied double (typically a fresh [`RecordingMetrics`]) instead
/// of the usual [`NoopMetrics`].
fn test_download_state_with_metrics(
    metrics: Arc<dyn file_storage::domain::ports::FileStorageMetricsPort>,
) -> (SidecarState, Issuer, Arc<InMemoryBackend>) {
    let (mut state, issuer, backend) = test_download_state();
    state.metrics = metrics;
    (state, issuer, backend)
}

/// Range-request counterpart to
/// `download_whole_content_length_matches_actual_body_length`: a `206`'s
/// `Content-Length` must equal the actual streamed body length (the range
/// span), not merely the `Content-Range` header's own numbers.
#[tokio::test]
async fn download_range_content_length_matches_actual_body_length() {
    let (state, issuer, backend) = test_download_state();
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    let content = b"range download body used to check Content-Length exactly";
    backend
        .put(&path, Bytes::from_static(content))
        .await
        .expect("seed blob");
    let token = download_token(&issuer, file_id, version_id, &path);

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::get(format!(
                "/api/file-storage-data/v1/download/{file_id}/{version_id}?fs-token={token}"
            ))
            .header(header::RANGE, "bytes=2-9")
            .body(Body::empty())
            .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    let content_length: usize = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .expect("Content-Length header present on 206")
        .to_str()
        .expect("valid header value")
        .parse()
        .expect("Content-Length is a valid integer");
    assert_eq!(content_length, 8, "bytes=2-9 spans exactly 8 bytes");

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    assert_eq!(
        body.len(),
        content_length,
        "actual body length must equal the already-sent Content-Length"
    );
    assert_eq!(&body[..], &content[2..10]);
}

/// A whole-object `GET` must report egress bytes equal to exactly the body
/// length actually streamed to the client -- not zero (the pre-`RecordingMetrics`
/// blind spot every other test shares via `NoopMetrics`), and not some other
/// value.
#[tokio::test]
async fn download_whole_get_records_egress_bytes_equal_to_body_length() {
    let metrics = Arc::new(RecordingMetrics::default());
    let (state, issuer, backend) = test_download_state_with_metrics(metrics.clone());
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    let content = b"whole-object body used to check the egress metric";
    backend
        .put(&path, Bytes::from_static(content))
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

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    assert_eq!(&body[..], content);

    #[allow(
        clippy::float_cmp,
        clippy::float_cmp_const,
        clippy::cast_precision_loss
    )]
    {
        assert_eq!(
            metrics.egress_total(),
            content.len() as f64,
            "egress must equal exactly the bytes actually streamed"
        );
    }
}

/// Range-request counterpart to the whole-object egress test above: a `206`
/// must report egress bytes equal to the range span actually streamed, not
/// the full object length.
#[tokio::test]
async fn download_range_get_records_egress_bytes_equal_to_range_length() {
    let metrics = Arc::new(RecordingMetrics::default());
    let (state, issuer, backend) = test_download_state_with_metrics(metrics.clone());
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    let content = b"range body used to check the egress metric records only the span";
    backend
        .put(&path, Bytes::from_static(content))
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

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    assert_eq!(body.len(), 5, "bytes=0-4 spans exactly 5 bytes");

    #[allow(clippy::float_cmp, clippy::float_cmp_const)]
    {
        assert_eq!(
            metrics.egress_total(),
            5.0,
            "egress must equal exactly the range span actually streamed, not the whole object"
        );
    }
}

/// `download_head` never calls `record_egress_bytes` at all (it has no body
/// to stream -- only `Content-Length` is set from `backend.stat`), so a
/// `HEAD` request must leave the egress metric untouched at `0`.
#[tokio::test]
async fn download_head_records_zero_egress_bytes() {
    let metrics = Arc::new(RecordingMetrics::default());
    let (state, issuer, backend) = test_download_state_with_metrics(metrics.clone());
    let file_id = Uuid::now_v7();
    let version_id = Uuid::now_v7();
    let path = format!("/{file_id}/{version_id}");
    backend
        .put(
            &path,
            Bytes::from_static(b"content that HEAD must never account as egress"),
        )
        .await
        .expect("seed blob");
    let token = download_token(&issuer, file_id, version_id, &path);

    let router = build_router(state, DEFAULT_MAX_BODY_BYTES);
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri(format!(
                    "/api/file-storage-data/v1/download/{file_id}/{version_id}?fs-token={token}"
                ))
                .body(Body::empty())
                .expect("valid request"),
        )
        .await
        .expect("router call succeeds");

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().contains_key(header::CONTENT_LENGTH),
        "HEAD must still report Content-Length even though it records no egress"
    );

    #[allow(clippy::float_cmp, clippy::float_cmp_const)]
    {
        assert_eq!(
            metrics.egress_total(),
            0.0,
            "HEAD has no body to stream, so egress must stay at 0"
        );
    }
}
