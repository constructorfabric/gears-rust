//! `FileStorage` data-plane sidecar: the only component that moves user bytes.
//!
//! It verifies the control-minted Ed25519 signed-URL token, enforces the token's upload
//! constraints (size / hash) and streams content to/from a storage backend. Clients never
//! address a backend directly.
//!
//! Configuration (env):
//!   - `FS_SIDECAR_ADDR` - bind address (default `0.0.0.0:8087`)
//!   - `FS_SIDECAR_PUBLIC_KEY` - base64url Ed25519 public key (from control)
//!   - `FS_SIDECAR_BACKEND_ROOT` - local-fs backend root (default `./.file-storage-data`)
//!   - `FS_SIDECAR_CONTROL_URL` - control-plane base URL for the finalize/report-part
//!     callbacks (default `http://localhost:8080`); empty disables them (dev/test only)
//!   - `FS_SIDECAR_MAX_BODY_BYTES` - transport-level body ceiling replacing axum's 2 MiB
//!     default (default 5 GiB); the real limit is the token's `upload.max_size`/`exact_size`
//!   - `FS_SIDECAR_FINALIZE_TIMEOUT_SECS` / `FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS` -
//!     total / connect timeout of the control-plane callbacks (defaults `10` / `5`); they
//!     bound how long a hung control plane can hold a client's upload open
//!   - `FS_SIDECAR_INTERNAL_TOKEN` - **required**: shared secret sent as
//!     `x-fs-internal-token` on the finalize and report-part callbacks. The sidecar refuses
//!     to start without it. Must equal the control plane's
//!     `FileStorageConfig::finalize_internal_secret`; the control plane trusts the size and
//!     SHA-256 reported on these callbacks (see ADR-0003).
//!   - `FS_SIDECAR_S3_BACKENDS` - optional JSON array of
//!     `file_storage::config::S3BackendConfig` entries (credentials included; prefer sourcing
//!     it from a secrets manager or mounted file). Entries are validated at startup and
//!     registered next to the always-present `local-fs` backend; each request resolves its
//!     backend from the verified token's `claims.backend_id`.
//!
//! ## Upload lifecycle
//!
//! After a successful single-part `PUT` the sidecar posts a finalize callback to
//! `POST {control_url}/api/file-storage/v1/files/{file_id}/versions/{version_id}/finalize`
//! with the signed token and the size and SHA-256 it measured while streaming. It answers
//! `200 OK` only if the callback succeeds; otherwise `502 Bad Gateway`, and the client
//! retries (the backend write overwrites, so retrying is safe).

use std::borrow::Cow;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, MatchedPath, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::StreamExt;
use serde::Deserialize;
use time::OffsetDateTime;
use toolkit_utils::SecretString;
use uuid::Uuid;

use file_storage::domain::error::DomainError;
use file_storage::domain::ports::FileStorageMetricsPort;
use file_storage::infra::backend::{BackendRegistry, LocalFsBackend, S3Backend, StorageBackend};
use file_storage::infra::content::{hash, range};
use file_storage::infra::metrics::FileStorageMetricsMeter;
use file_storage::infra::signed_url::{Claims, Op, Verifier};

/// Id of the local-fs backend; also the registry's default id, which dispatch never uses
/// (requests name their backend via `claims.backend_id`) but `BackendRegistry::new` requires.
const LOCAL_FS_ID: &str = "local-fs";

#[derive(Clone)]
/// Shared per-process state of the sidecar HTTP handlers: token verifier, backends, control-plane callback settings and metrics.
struct SidecarState {
    verifier: Arc<Verifier>,
    /// Backends keyed by id; resolved per request from the verified token's `claims.backend_id`.
    backends: BackendRegistry,
    /// Control-plane base URL; empty disables the callbacks (dev mode).
    control_base_url: String,
    /// `FS_SIDECAR_INTERNAL_TOKEN`, sent as `x-fs-internal-token` on the callbacks.
    internal_token: String,
    http: reqwest::Client,
    /// Ingress/egress bytes and per-route latency/status. This process is not behind the
    /// api-gateway, so it owns its own `OTel` `Meter`.
    metrics: Arc<dyn FileStorageMetricsPort>,
}

#[derive(Debug, Deserialize)]
/// Query string of the data-plane routes; carries the signed token when it is not sent in a header.
struct TokenQuery {
    #[serde(rename = "fs-token")]
    fs_token: Option<SecretString>,
}

/// Default `FS_SIDECAR_MAX_BODY_BYTES` (5 GiB), above any policy-permitted single-part upload.
const DEFAULT_MAX_BODY_BYTES: usize = 5_368_709_120;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addr: SocketAddr = std::env::var("FS_SIDECAR_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8087".to_owned())
        .parse()?;
    let root = std::env::var("FS_SIDECAR_BACKEND_ROOT")
        .unwrap_or_else(|_| "./.file-storage-data".to_owned());
    let public_key_b64 = std::env::var("FS_SIDECAR_PUBLIC_KEY")
        .map_err(|_| anyhow::anyhow!("FS_SIDECAR_PUBLIC_KEY is required"))?;
    let public_key = URL_SAFE_NO_PAD
        .decode(public_key_b64.trim())
        .map_err(|e| anyhow::anyhow!("invalid FS_SIDECAR_PUBLIC_KEY: {e}"))?;

    let control_base_url = std::env::var("FS_SIDECAR_CONTROL_URL")
        .unwrap_or_else(|_| "http://localhost:8080".to_owned());
    if control_base_url.is_empty() {
        tracing::warn!(
            "FS_SIDECAR_CONTROL_URL is empty \u{2014} finalize callback disabled. \
             Uploaded versions will remain in 'pending' status."
        );
    } else {
        tracing::info!(control_base_url = %control_base_url, "sidecar finalize callback enabled");
    }

    let max_body_bytes: usize = std::env::var("FS_SIDECAR_MAX_BODY_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_BODY_BYTES);

    let finalize_timeout_secs: u64 = std::env::var("FS_SIDECAR_FINALIZE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let finalize_connect_timeout_secs: u64 =
        std::env::var("FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(finalize_timeout_secs))
        .connect_timeout(Duration::from_secs(finalize_connect_timeout_secs))
        .build()
        .map_err(|e| anyhow::anyhow!("reqwest client: {e}"))?;

    // Mandatory: fail fast rather than start and have every finalize rejected.
    let internal_token = std::env::var("FS_SIDECAR_INTERNAL_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "FS_SIDECAR_INTERNAL_TOKEN is required (must equal the control plane's \
                 finalize_internal_secret)"
            )
        })?;

    // Built eagerly so a bad endpoint or missing credentials fails startup.
    let s3_backends: Vec<Arc<dyn StorageBackend>> = match std::env::var("FS_SIDECAR_S3_BACKENDS") {
        Ok(json) if !json.trim().is_empty() => {
            let entries: Vec<file_storage::config::S3BackendConfig> =
                serde_json::from_str(&json)
                    .map_err(|e| anyhow::anyhow!("invalid FS_SIDECAR_S3_BACKENDS: {e}"))?;
            entries
                .iter()
                .map(|entry| {
                    S3Backend::from_config(entry)
                        .map(|backend| Arc::new(backend) as Arc<dyn StorageBackend>)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow::anyhow!("FS_SIDECAR_S3_BACKENDS: {e}"))?
        }
        _ => Vec::new(),
    };
    if !s3_backends.is_empty() {
        tracing::info!(
            count = s3_backends.len(),
            "sidecar parsed FS_SIDECAR_S3_BACKENDS \u{2014} registered for claims.backend_id dispatch"
        );
    }

    let mut backend_list: Vec<Arc<dyn StorageBackend>> =
        vec![Arc::new(LocalFsBackend::new(LOCAL_FS_ID, root))];
    backend_list.extend(s3_backends);
    let backends = BackendRegistry::new(backend_list, LOCAL_FS_ID)
        .map_err(|e| anyhow::anyhow!("failed to build sidecar backend registry: {e}"))?;

    let metrics_scope =
        opentelemetry::InstrumentationScope::builder("file-storage-sidecar".to_owned()).build();
    let metrics: Arc<dyn FileStorageMetricsPort> = Arc::new(FileStorageMetricsMeter::new(
        &opentelemetry::global::meter_with_scope(metrics_scope),
        "file_storage",
    ));

    let state = SidecarState {
        verifier: Arc::new(
            Verifier::from_public_key(public_key)
                .map_err(|e| anyhow::anyhow!("invalid FS_SIDECAR_PUBLIC_KEY: {e}"))?,
        ),
        backends,
        control_base_url,
        internal_token,
        http,
        metrics,
    };

    let app = build_router(state, max_body_bytes);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "file-storage sidecar listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Builds the `Router` without binding a socket, so tests can drive it in-process.
///
/// `max_body_bytes` replaces axum's 2 MiB body default; the real limit is the token's size claims.
fn build_router(state: SidecarState, max_body_bytes: usize) -> Router {
    Router::new()
        .route(
            "/api/file-storage-data/v1/upload/{file_id}/{version_id}",
            put(upload),
        )
        .route(
            "/api/file-storage-data/v1/download/{file_id}/{version_id}",
            get(download),
        )
        // The control plane mints one `multipart_part` token per part.
        .route(
            "/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/{part_number}",
            put(upload_multipart_part),
        )
        // Liveness only; backend checks live in `readyz`.
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            record_request_metrics,
        ))
        .with_state(state)
        .layer(DefaultBodyLimit::max(max_body_bytes))
}

/// Records one request-duration observation per request (route from `MatchedPath`, or
/// `"unmatched"` to bound cardinality; method; status; latency).
async fn record_request_metrics(
    State(state): State<SidecarState>,
    matched_path: Option<MatchedPath>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().as_str().to_owned();
    let route = matched_path
        .as_ref()
        .map_or("unmatched", MatchedPath::as_str)
        .to_owned();
    let start = std::time::Instant::now();
    let response = next.run(req).await;
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    state
        .metrics
        .record_request(&route, &method, response.status().as_u16(), elapsed_ms);
    response
}

/// Liveness probe: always `200 OK`; does not check backends (see `readyz`).
async fn healthz() -> &'static str {
    "ok"
}

/// Time budget per backend readiness probe, well under a typical k8s probe period (~10s).
const READYZ_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Readiness probe: `200` only if every backend's `is_ready` succeeds within
/// `READYZ_PROBE_TIMEOUT`; otherwise `503` naming only the failing backend ids, never the
/// underlying error text (no backend internals leak).
async fn readyz(State(state): State<SidecarState>) -> Response {
    let checks = state.backends.iter().map(|(id, backend)| {
        let id = id.to_owned();
        let backend = Arc::clone(backend);
        async move {
            match tokio::time::timeout(READYZ_PROBE_TIMEOUT, backend.is_ready()).await {
                Ok(Ok(())) => None,
                Ok(Err(_)) | Err(_) => Some(id),
            }
        }
    });

    let failing: Vec<String> = futures::future::join_all(checks)
        .await
        .into_iter()
        .flatten()
        .collect();

    if failing.is_empty() {
        (StatusCode::OK, "ready").into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("not ready: {}", failing.join(", ")),
        )
            .into_response()
    }
}

/// Extract the token from the `fs-token` query param or the `X-FS-Token` header.
fn extract_token(q: &TokenQuery, headers: &HeaderMap) -> Option<String> {
    q.fs_token
        .as_ref()
        .map(|s| s.expose().to_owned())
        .or_else(|| {
            headers
                .get("x-fs-token")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        })
}

/// `PUT` upload: verifies the token (op=PUT) and streams the body to the backend.
///
/// The body is never buffered whole: `StorageBackend::put_stream` writes and hashes chunks
/// as they arrive and aborts mid-stream once `claims.upload.max_size` is exceeded.
/// `exact_size`/`expected_hash` are final only after the stream is drained, so they are
/// checked after `put_stream` returns.
async fn upload(
    State(state): State<SidecarState>,
    Path((file_id, version_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(token) = extract_token(&q, &headers) else {
        return (StatusCode::UNAUTHORIZED, "missing fs-token").into_response();
    };
    let claims = match state.verifier.verify(&token, OffsetDateTime::now_utc()) {
        Ok(c) => c,
        Err(e) => return (StatusCode::FORBIDDEN, e.to_string()).into_response(),
    };
    if claims.op != Op::Put || claims.file_id != file_id || claims.version_id != version_id {
        return (
            StatusCode::FORBIDDEN,
            "token does not authorize this operation",
        )
            .into_response();
    }

    let backend = match state.backends.get(&claims.backend_id) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("unknown backend '{}': {e}", claims.backend_id),
            )
                .into_response();
        }
    };

    let byte_stream: futures::stream::BoxStream<'_, std::io::Result<bytes::Bytes>> = Box::pin(
        body.into_data_stream()
            .map(|r| r.map_err(std::io::Error::other)),
    );
    let (bytes_written, digest) = match backend
        .put_stream(&claims.backend_path, byte_stream, claims.upload.max_size)
        .await
    {
        Ok(v) => v,
        // `Validation` here is only the mid-stream `max_size` guard.
        Err(DomainError::Validation { .. }) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "exceeds max_size").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "backend put_stream failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response();
        }
    };

    if claims
        .upload
        .exact_size
        .is_some_and(|exact| bytes_written != exact)
    {
        return (StatusCode::BAD_REQUEST, "size does not match exact_size").into_response();
    }
    if let Some(expected) = &claims.upload.expected_hash {
        let got = format!("{}:{}", hash::ALGORITHM, hex::encode(digest));
        if !expected.eq_ignore_ascii_case(&got) {
            return (StatusCode::BAD_REQUEST, "content hash mismatch").into_response();
        }
    }

    let size = i64::try_from(bytes_written).unwrap_or(i64::MAX);
    let hash_hex = hex::encode(digest);

    #[allow(clippy::cast_precision_loss)]
    state.metrics.record_ingress_bytes(bytes_written as f64);

    // Tell the control plane the bytes landed (the signed token proves the upload was
    // pre-authorized); `claims.request_id` is echoed as `x-request-id` for log correlation.
    if let Err(rejection) = finalize_with_control_plane(
        &state,
        &token,
        &claims.request_id,
        file_id,
        version_id,
        size,
        &hash_hex,
    )
    .await
    {
        return rejection.into_response();
    }

    (StatusCode::OK, "uploaded").into_response()
}

/// A failed upload step: the status and plain-text body the client receives. Kept small
/// (unlike a built `Response`) to stay under `clippy::result_large_err`.
struct Rejection {
    status: StatusCode,
    body: Cow<'static, str>,
}

impl Rejection {
    fn new(status: StatusCode, body: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }
}

impl IntoResponse for Rejection {
    fn into_response(self) -> Response {
        (self.status, self.body).into_response()
    }
}

/// Finalize request body: JSON `{size, hash_hex}`.
fn finalize_body(size: i64, hash_hex: &str) -> Vec<u8> {
    serde_json::json!({ "size": size, "hash_hex": hash_hex })
        .to_string()
        .into_bytes()
}

async fn interpret_finalize_response(
    resp: reqwest::Response,
    file_id: Uuid,
    version_id: Uuid,
) -> Result<(), Rejection> {
    if resp.status().is_success() {
        tracing::debug!(%file_id, %version_id, "finalize callback succeeded");
        return Ok(());
    }
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    tracing::error!(
        %file_id, %version_id,
        http_status = %status,
        body = %body_text,
        "control-plane finalize callback returned error"
    );
    // Status/body stay in the log: the control plane's raw error must not reach the client.
    Err(Rejection::new(StatusCode::BAD_GATEWAY, "finalize failed"))
}

/// Max attempts (including the first) per callback POST. Only transport-level connect/timeout
/// failures are retried; an HTTP error status is returned to the caller as is.
const CALLBACK_MAX_ATTEMPTS: u32 = 3;

/// Fixed delay between callback retry attempts.
const CALLBACK_RETRY_DELAY: Duration = Duration::from_millis(100);

/// POSTs `body_bytes` to `url` with the callback retry policy (`CALLBACK_MAX_ATTEMPTS`,
/// `CALLBACK_RETRY_DELAY`); `internal_token` is always sent as `x-fs-internal-token`.
async fn post_with_retry(
    http: &reqwest::Client,
    url: &str,
    token: &str,
    request_id: &str,
    internal_token: &str,
    body_bytes: &[u8],
) -> Result<reqwest::Response, reqwest::Error> {
    use tokio_retry::RetryIf;
    use tokio_retry::strategy::FixedInterval;

    let mut attempt: u32 = 0;
    let action = || {
        attempt += 1;
        let this_attempt = attempt;
        let mut req = http
            .post(url)
            .header("content-type", "application/json")
            .header("x-fs-token", token);
        // Propagate the correlation id so both planes' logs for an upload can be joined.
        if !request_id.is_empty() {
            req = req.header("x-request-id", request_id);
        }
        req = req.header("x-fs-internal-token", internal_token);
        let fut = req.body(body_bytes.to_vec()).send();
        async move {
            let result = fut.await;
            if let Err(ref e) = result
                && this_attempt < CALLBACK_MAX_ATTEMPTS
                && (e.is_connect() || e.is_timeout())
            {
                tracing::warn!(
                    attempt = this_attempt,
                    error = %e,
                    "control-plane callback transport error, retrying"
                );
            }
            result
        }
    };
    // `CALLBACK_MAX_ATTEMPTS` includes the first attempt, so one fewer delay.
    let retryable = |e: &reqwest::Error| e.is_connect() || e.is_timeout();
    let strategy =
        FixedInterval::new(CALLBACK_RETRY_DELAY).take((CALLBACK_MAX_ATTEMPTS - 1) as usize);
    RetryIf::start(strategy, action, retryable).await
}

/// Calls the control-plane finalize endpoint after a successful PUT; a failed callback is a
/// `502`. Skipped when `control_base_url` is empty (dev mode).
async fn finalize_with_control_plane(
    state: &SidecarState,
    token: &str,
    request_id: &str,
    file_id: Uuid,
    version_id: Uuid,
    size: i64,
    hash_hex: &str,
) -> Result<(), Rejection> {
    if state.control_base_url.is_empty() {
        return Ok(());
    }

    let url = format!(
        "{}/api/file-storage/v1/files/{}/versions/{}/finalize",
        state.control_base_url.trim_end_matches('/'),
        file_id,
        version_id,
    );

    let body_bytes = finalize_body(size, hash_hex);

    match post_with_retry(
        &state.http,
        &url,
        token,
        request_id,
        &state.internal_token,
        &body_bytes,
    )
    .await
    {
        Ok(resp) => interpret_finalize_response(resp, file_id, version_id).await,
        Err(e) => {
            tracing::error!(
                %file_id, %version_id, error = %e,
                "control-plane finalize callback failed"
            );
            // `e` embeds the internal control-plane URL; never forward it to the client.
            Err(Rejection::new(StatusCode::BAD_GATEWAY, "finalize failed"))
        }
    }
}

/// Report-part request body: JSON `{backend_etag, hash_hex, size}`.
fn report_part_body(backend_etag: &str, hash_hex: &str, size: i64) -> Vec<u8> {
    serde_json::json!({
        "backend_etag": backend_etag,
        "hash_hex": hash_hex,
        "size": size,
    })
    .to_string()
    .into_bytes()
}

async fn interpret_report_part_response(
    resp: reqwest::Response,
    upload_id: Uuid,
    part_number: u32,
) -> Result<(), Rejection> {
    if resp.status().is_success() {
        tracing::debug!(%upload_id, part_number, "report-part callback succeeded");
        return Ok(());
    }
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    tracing::error!(
        %upload_id, part_number,
        http_status = %status,
        body = %body_text,
        "control-plane report-part callback returned error"
    );
    // Same no-leak rule as `interpret_finalize_response`.
    Err(Rejection::new(StatusCode::BAD_GATEWAY, "report failed"))
}

/// Reports a written part to the control plane, which records it for
/// `complete_multipart_upload`. Same contract as `finalize_with_control_plane`; the part write
/// and this report are both idempotent per `(upload_id, part_number)`, so clients may retry.
#[allow(clippy::too_many_arguments)]
async fn report_part_with_control_plane(
    state: &SidecarState,
    token: &str,
    request_id: &str,
    file_id: Uuid,
    version_id: Uuid,
    upload_id: Uuid,
    part_number: u32,
    backend_etag: &str,
    hash_hex: &str,
    size: i64,
) -> Result<(), Rejection> {
    if state.control_base_url.is_empty() {
        return Ok(());
    }

    let url = format!(
        "{}/api/file-storage/v1/files/{}/versions/{}/multipart/{}/parts/{}/report",
        state.control_base_url.trim_end_matches('/'),
        file_id,
        version_id,
        upload_id,
        part_number,
    );

    let body_bytes = report_part_body(backend_etag, hash_hex, size);

    match post_with_retry(
        &state.http,
        &url,
        token,
        request_id,
        &state.internal_token,
        &body_bytes,
    )
    .await
    {
        Ok(resp) => interpret_report_part_response(resp, upload_id, part_number).await,
        Err(e) => {
            tracing::error!(
                %file_id, %version_id, %upload_id, part_number, error = %e,
                "control-plane report-part callback failed"
            );
            // `e` embeds the internal control-plane URL; do not forward it.
            Err(Rejection::new(StatusCode::BAD_GATEWAY, "report failed"))
        }
    }
}

/// Writes one multipart part, returning `(body_len, backend_etag, hash_hex)` or a terminal
/// [`Rejection`].
///
/// Backends with `multipart_native` (S3) get `upload_part` on their native session
/// (`claims.multipart.backend_handle`); the part is buffered, bounded by the token's exact
/// `size` claim, because `UploadPart` needs the whole body up front. Other backends
/// (local-fs) write each part as its own object `{backend_path}.part.{n}` via `put_stream`,
/// and `complete_multipart_upload` assembles them.
async fn write_multipart_part(
    backend: &dyn StorageBackend,
    claims: &Claims,
    part_number: u32,
    body: Body,
) -> Result<(u64, String, String), Rejection> {
    if backend.capabilities().multipart_native {
        write_multipart_part_native(backend, claims, part_number, body).await
    } else {
        write_multipart_part_offset_object(backend, claims, part_number, body).await
    }
}

/// `multipart_native` write path; see `write_multipart_part`.
async fn write_multipart_part_native(
    backend: &dyn StorageBackend,
    claims: &Claims,
    part_number: u32,
    body: Body,
) -> Result<(u64, String, String), Rejection> {
    let max_size = claims.multipart.size;
    let mut stream = body.into_data_stream();
    let mut buf = bytes::BytesMut::new();
    loop {
        match stream.next().await {
            Some(Ok(chunk)) => {
                if (buf.len() as u64).saturating_add(chunk.len() as u64) > max_size {
                    return Err(Rejection::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        format!("part body length exceeds token size claim {max_size}"),
                    ));
                }
                buf.extend_from_slice(&chunk);
            }
            Some(Err(e)) => {
                tracing::error!(error = %e, part_number, "part body stream read failed");
                return Err(Rejection::new(StatusCode::BAD_REQUEST, "body read error"));
            }
            None => break,
        }
    }
    let body_len = buf.len() as u64;
    // The guard above rejects oversize, so a mismatch here means an undersized part: a
    // client error (`400`, not `413`). Checked before the backend call, nothing to clean up.
    if body_len != max_size {
        return Err(Rejection::new(
            StatusCode::BAD_REQUEST,
            format!("part body length {body_len} does not match token size claim {max_size}"),
        ));
    }
    match backend
        .upload_part(
            &claims.backend_path,
            &claims.multipart.backend_handle,
            part_number,
            // Part's byte offset in the assembled object (ADR-0006), minted into the token.
            claims.multipart.offset,
            buf.freeze(),
        )
        .await
    {
        Ok((etag, hash)) => Ok((body_len, etag, hex::encode(hash))),
        Err(e) => {
            tracing::error!(error = %e, part_number, "backend native upload_part failed");
            Err(Rejection::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "backend error",
            ))
        }
    }
}

/// Non-native (one object per part) write path; see `write_multipart_part`.
async fn write_multipart_part_offset_object(
    backend: &dyn StorageBackend,
    claims: &Claims,
    part_number: u32,
    body: Body,
) -> Result<(u64, String, String), Rejection> {
    let part_path = format!("{}.part.{}", claims.backend_path, part_number);
    let byte_stream: futures::stream::BoxStream<'_, std::io::Result<bytes::Bytes>> = Box::pin(
        body.into_data_stream()
            .map(|r| r.map_err(std::io::Error::other)),
    );
    let (body_len, part_hash) = match backend
        .put_stream(&part_path, byte_stream, Some(claims.multipart.size))
        .await
    {
        Ok(v) => v,
        Err(DomainError::Validation { .. }) => {
            return Err(Rejection::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "part body length exceeds token size claim {}",
                    claims.multipart.size
                ),
            ));
        }
        Err(e) => {
            tracing::error!(error = %e, part_number, "backend part write failed");
            return Err(Rejection::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "backend error",
            ));
        }
    };

    // Oversize was rejected mid-stream (`413`); a mismatch here means an undersized part
    // (`400`). The part object is removed so a rejected part leaves no orphan.
    if body_len != claims.multipart.size {
        drop(backend.delete(&part_path).await);
        return Err(Rejection::new(
            StatusCode::BAD_REQUEST,
            format!(
                "part body length {} does not match token size claim {}",
                body_len, claims.multipart.size
            ),
        ));
    }

    let part_etag = hex::encode(part_hash);
    Ok((body_len, part_etag.clone(), part_etag))
}

/// `PUT` multipart part: verifies the `multipart_part` token, writes the part, enforces the
/// exact `size` claim and reports the part (etag, SHA-256, size) to the control plane.
///
/// Oversized parts abort mid-stream; undersized ones are detected after the write. The
/// sidecar only verifies tokens, never mints them (ADR-0004). Idempotent per
/// `(upload_id, part_number)`: re-PUTting with the same token overwrites the part.
async fn upload_multipart_part(
    State(state): State<SidecarState>,
    Path((file_id, version_id, part_number)): Path<(Uuid, Uuid, u32)>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(token) = extract_token(&q, &headers) else {
        return (StatusCode::UNAUTHORIZED, "missing fs-token").into_response();
    };
    let claims = match state
        .verifier
        .verify(&token, time::OffsetDateTime::now_utc())
    {
        Ok(c) => c,
        Err(e) => return (StatusCode::FORBIDDEN, e.to_string()).into_response(),
    };

    if claims.op != Op::MultipartPart
        || claims.file_id != file_id
        || claims.version_id != version_id
    {
        return (
            StatusCode::FORBIDDEN,
            "token does not authorize this operation",
        )
            .into_response();
    }

    // Prevents replaying another part's token.
    if claims.multipart.part_number != part_number {
        return (
            StatusCode::FORBIDDEN,
            "token part_number does not match path",
        )
            .into_response();
    }

    let backend = match state.backends.get(&claims.backend_id) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("unknown backend '{}': {e}", claims.backend_id),
            )
                .into_response();
        }
    };

    let (body_len, backend_etag, hash_hex) =
        match write_multipart_part(backend.as_ref(), &claims, part_number, body).await {
            Ok(v) => v,
            Err(rejection) => return rejection.into_response(),
        };

    #[allow(clippy::cast_precision_loss)]
    state.metrics.record_ingress_bytes(body_len as f64);

    // Record the part on the control plane; `claims.request_id` is echoed as `x-request-id`.
    if let Err(rejection) = report_part_with_control_plane(
        &state,
        &token,
        &claims.request_id,
        file_id,
        version_id,
        claims.multipart.upload_id,
        part_number,
        &backend_etag,
        &hash_hex,
        i64::try_from(body_len).unwrap_or(i64::MAX),
    )
    .await
    {
        return rejection.into_response();
    }

    let body = serde_json::json!({
        "part_number": part_number,
        "etag": backend_etag,
        "hash_algorithm": "SHA-256",
        "hash": hash_hex,
    });
    (StatusCode::OK, axum::Json(body)).into_response()
}

/// Fallback `Content-Type` when the token's `content_type` claim is empty or not a valid
/// header value. The sidecar has no DB access; it only echoes what the token carries.
const FALLBACK_CONTENT_TYPE: &str = "application/octet-stream";

/// `Content-Type` for a download: `claims.content_type`, else `FALLBACK_CONTENT_TYPE`.
fn content_type_header(claims: &Claims) -> HeaderValue {
    if claims.content_type.is_empty() {
        return HeaderValue::from_static(FALLBACK_CONTENT_TYPE);
    }
    HeaderValue::from_str(&claims.content_type)
        .unwrap_or_else(|_| HeaderValue::from_static(FALLBACK_CONTENT_TYPE))
}

/// `ETag` for a download: `claims.etag` already holds the quoted content `ETag` minted by the
/// control plane. `None` (header omitted) when empty or not a valid header value.
fn etag_header(claims: &Claims) -> Option<HeaderValue> {
    if claims.etag.is_empty() {
        return None;
    }
    HeaderValue::from_str(&claims.etag).ok()
}

/// Builds a `Content-Range` value, e.g. `bytes 0-99/1000` or `bytes */1000` (unsatisfiable).
fn header_value(s: &str) -> HeaderValue {
    // Callers pass ASCII only; fall back to a placeholder rather than panic.
    HeaderValue::from_str(s).unwrap_or_else(|_| HeaderValue::from_static("invalid"))
}

/// `GET` download: verifies the token (op=GET) and serves the blob, honouring `Range`.
///
/// Not found is `404`, an unsatisfiable range `416` with `Content-Range: bytes */total`,
/// other backend faults `500`; every `206` carries `Content-Range`. `Content-Type` and `ETag`
/// come from the token's claims. `If-None-Match` (`304`) is not implemented: download tokens
/// are scoped to one `(file_id, version_id)`, so the saving is small.
async fn download(
    State(state): State<SidecarState>,
    Path((file_id, version_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = extract_token(&q, &headers) else {
        return (StatusCode::UNAUTHORIZED, "missing fs-token").into_response();
    };
    let claims = match state.verifier.verify(&token, OffsetDateTime::now_utc()) {
        Ok(c) => c,
        Err(e) => return (StatusCode::FORBIDDEN, e.to_string()).into_response(),
    };
    if claims.op != Op::Get || claims.file_id != file_id || claims.version_id != version_id {
        return (
            StatusCode::FORBIDDEN,
            "token does not authorize this operation",
        )
            .into_response();
    }

    let backend = match state.backends.get(&claims.backend_id) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("unknown backend '{}': {e}", claims.backend_id),
            )
                .into_response();
        }
    };

    let path = &claims.backend_path;

    // Existence first, so a missing blob is `404` and never a `416` or `500` later.
    match backend.exists(path).await {
        Ok(true) => {}
        Ok(false) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "backend existence check failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response();
        }
    }

    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(range::parse);

    match range {
        Some(r) => download_range(&state, &backend, path, r, &claims).await,
        None => download_whole(&state, &backend, path, &claims).await,
    }
}

/// Serves a `Range` `GET`; the caller (`download`) has already confirmed the blob exists.
async fn download_range(
    state: &SidecarState,
    backend: &Arc<dyn StorageBackend>,
    path: &str,
    r: file_storage_sdk::ByteRange,
    claims: &Claims,
) -> Response {
    let total = match backend.size(path).await {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, "backend size lookup failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response();
        }
    };
    let Some((start, end)) = r.resolve(total) else {
        // Range starts past the end of an existing blob (RFC 9110 §14.4).
        let mut resp = (StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable").into_response();
        resp.headers_mut().insert(
            header::CONTENT_RANGE,
            header_value(&format!("bytes */{total}")),
        );
        return resp;
    };
    match backend.get_range(path, r).await {
        Ok(bytes) => {
            #[allow(clippy::cast_precision_loss)]
            state.metrics.record_egress_bytes(bytes.len() as f64);
            let mut resp = (StatusCode::PARTIAL_CONTENT, bytes).into_response();
            let headers_mut = resp.headers_mut();
            headers_mut.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            headers_mut.insert(
                header::CONTENT_RANGE,
                header_value(&format!("bytes {start}-{end}/{total}")),
            );
            headers_mut.insert(header::CONTENT_TYPE, content_type_header(claims));
            if let Some(v) = etag_header(claims) {
                headers_mut.insert(header::ETAG, v);
            }
            resp
        }
        Err(e) => {
            // Existence and range were already validated: a genuine I/O fault.
            tracing::error!(error = %e, "backend get_range failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response()
        }
    }
}

/// Serves a whole-blob `GET`; the caller (`download`) has already confirmed the blob exists.
async fn download_whole(
    state: &SidecarState,
    backend: &Arc<dyn StorageBackend>,
    path: &str,
    claims: &Claims,
) -> Response {
    match backend.get(path).await {
        Ok(bytes) => {
            #[allow(clippy::cast_precision_loss)]
            state.metrics.record_egress_bytes(bytes.len() as f64);
            let mut resp = (StatusCode::OK, bytes).into_response();
            let headers_mut = resp.headers_mut();
            headers_mut.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            headers_mut.insert(header::CONTENT_TYPE, content_type_header(claims));
            if let Some(v) = etag_header(claims) {
                headers_mut.insert(header::ETAG, v);
            }
            resp
        }
        Err(e) => {
            tracing::error!(error = %e, "backend get failed after existence check");
            (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response()
        }
    }
}

#[cfg(test)]
#[path = "sidecar_tests.rs"]
mod tests;
