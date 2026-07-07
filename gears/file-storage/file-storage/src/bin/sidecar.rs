//! `FileStorage` data-plane sidecar (`cpt-cf-file-storage-component-sidecar-gateway`,
//! `cpt-cf-file-storage-component-stream-proxy`).
//!
//! The sidecar is the only component that moves user bytes. It verifies the
//! control-minted Ed25519 signed-URL token, enforces the token's upload
//! constraints (size / hash), and streams content to/from a storage backend.
//! Clients never address a backend directly — the signed URL always points here.
//!
//! Configuration (env, P1 static):
//!   - `FS_SIDECAR_ADDR`         — bind address (default `0.0.0.0:8087`)
//!   - `FS_SIDECAR_PUBLIC_KEY`   — base64url Ed25519 public key (from control)
//!   - `FS_SIDECAR_BACKEND_ROOT` — local-fs backend root (default `./.file-storage-data`)
//!   - `FS_SIDECAR_CONTROL_URL`  — base URL of the control plane (for finalize callback,
//!     default `http://localhost:8080`). When set to an empty string the callback is
//!     disabled (dev/test mode only).
//!   - `FS_SIDECAR_MAX_BODY_BYTES` — raises axum's blanket 2 MiB request-body floor
//!     (default `5_368_709_120`, i.e. 5 GiB). This is only a transport-layer ceiling;
//!     the real per-request limit is still enforced by the signed token's
//!     `claims.upload.max_size`/`exact_size`.
//!   - `FS_SIDECAR_FINALIZE_TIMEOUT_SECS` — total request timeout (seconds) for the
//!     sidecar→control-plane finalize/report-part callbacks (default `10`).
//!   - `FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS` — connect timeout (seconds) for the
//!     same callbacks (default `5`). Together these bound how long a client's upload
//!     request can be held open by an unreachable or hung control plane (P2 1.5).
//!   - `FS_SIDECAR_S3_BACKENDS` — P2 1.7.3 config wiring: an optional JSON array of
//!     `file_storage::config::S3BackendConfig` entries, e.g. a single entry
//!     `{"id":"s3-primary","endpoint":"http://127.0.0.1:9000","region":"us-east-1",
//!     "bucket":"my-bucket","access_key_id":"...","secret_access_key":"...","path_style":true}`
//!     wrapped in a JSON array.
//!     Unset or empty = no S3 backends. Credentials embedded in this env var are
//!     acceptable for the sidecar (it is the one component authorized to hold them,
//!     per ADR-0003's sidecar/control-plane split) but in production this JSON blob
//!     should be sourced from a secrets manager / mounted file, not a plain process
//!     env var, where the deployment platform supports it. Each entry is
//!     validated at startup (a bad endpoint or missing credentials fails the sidecar
//!     fast) and, alongside the always-present `local-fs` backend, is folded into a
//!     `BackendRegistry` (`cpt-cf-file-storage` P2 1.7.2 / Stage 5): every request
//!     resolves its backend per request from the verified token's
//!     `claims.backend_id`, so a control-plane-registered `S3Backend` is reachable
//!     by real traffic.
//!
//! ## Upload lifecycle
//!
//! After a successful single-part `PUT`, the sidecar:
//! 1. Writes the blob to the backend.
//! 2. Posts a finalize callback to the control plane:
//!    `POST {control_url}/api/file-storage/v1/files/{file_id}/versions/{version_id}/finalize`
//!    carrying the signed upload token + the measured size+hash.
//! 3. Returns `200 OK` to the client only when the callback succeeds.
//!    A failed callback returns `502 Bad Gateway` — the client should retry
//!    the upload (idempotent: the backend PUT is overwrite-safe).

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
use uuid::Uuid;

use file_storage::domain::error::DomainError;
use file_storage::domain::ports::FileStorageMetricsPort;
use file_storage::infra::backend::{BackendRegistry, LocalFsBackend, S3Backend, StorageBackend};
use file_storage::infra::content::{hash, range};
use file_storage::infra::metrics::FileStorageMetricsMeter;
use file_storage::infra::signed_url::{Claims, Op, Verifier};

/// Id of the local-fs backend, and the sidecar's `BackendRegistry` default id.
/// The default is never actually consulted by request dispatch (every request
/// names its backend explicitly via `claims.backend_id`), but
/// `BackendRegistry::new` requires a valid default id to construct at all.
const LOCAL_FS_ID: &str = "local-fs";

#[derive(Clone)]
struct SidecarState {
    verifier: Arc<Verifier>,
    /// Backends this sidecar can dispatch to, keyed by id. The backend used
    /// for a given request is resolved *per request* from the verified
    /// token's `claims.backend_id` (Stage 5 / P2 1.7.2) — never a single
    /// hardcoded backend.
    backends: BackendRegistry,
    /// Base URL of the control plane, e.g. `http://localhost:8080`.
    /// Empty string = finalize callback disabled (dev/no-control-plane mode).
    control_base_url: String,
    http: reqwest::Client,
    /// Metrics port (P2 1.8 remediation) — ingress/egress bytes and
    /// route/method/status/latency for the sidecar's own HTTP routes. The
    /// control-plane's routes are already covered by the platform's
    /// api-gateway `http.server.request.duration` middleware; this process is
    /// never proxied by it, so it owns its own `OTel` `Meter` instance (see the
    /// module note on `FileStorageMetricsMeter` re: exporter wiring being out
    /// of scope here).
    metrics: Arc<dyn FileStorageMetricsPort>,
}

#[derive(Debug, Deserialize)]
struct TokenQuery {
    #[serde(rename = "fs-token")]
    fs_token: Option<String>,
}

/// Default value for `FS_SIDECAR_MAX_BODY_BYTES` (5 GiB) — comfortably above any
/// policy-permitted single-part upload. The real ceiling is still enforced
/// per-request by the signed token's `claims.upload.max_size`/`exact_size`;
/// this constant only bounds axum's blanket request-body floor (2 MiB default).
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

    // `FS_SIDECAR_CONTROL_URL` — base URL of the control-plane finalize endpoint.
    // An empty string disables the callback (useful for local dev or standalone tests).
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

    // `FS_SIDECAR_MAX_BODY_BYTES` — raises axum's blanket 2 MiB request-body floor.
    // The real per-request ceiling is still enforced by the signed token's
    // `claims.upload.max_size`/`exact_size` inside the handlers; this value only
    // needs to be large enough that no policy-permitted upload ever hits it.
    let max_body_bytes: usize = std::env::var("FS_SIDECAR_MAX_BODY_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_BODY_BYTES);

    // `FS_SIDECAR_FINALIZE_TIMEOUT_SECS` / `FS_SIDECAR_FINALIZE_CONNECT_TIMEOUT_SECS`
    // bound how long the sidecar will wait on the control-plane finalize/report-part
    // callbacks (P2 1.5) — without these, a hung or unreachable control plane could
    // block the client's upload request indefinitely.
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

    // `FS_SIDECAR_S3_BACKENDS` (P2 1.7.3 config wiring) — a JSON array of
    // `S3BackendConfig` entries. Parsed and eagerly constructed here (so a
    // misconfigured entry, e.g. a bad endpoint URL or missing credentials
    // with no env fallback, fails sidecar startup fast). Folded into the
    // `BackendRegistry` below alongside `local-fs`, so entries here are
    // reachable by traffic via `claims.backend_id` dispatch (Stage 5 / P2
    // 1.7.2).
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

    // P2 1.8 remediation: the sidecar is its own OS process, so it owns its
    // own OTel `Meter` — mirrors the control plane's `meter_with_scope` call
    // in `gear.rs`, scoped under the sidecar's own instrumentation name.
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
        http,
        metrics,
    };

    let app = build_router(state, max_body_bytes);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "file-storage sidecar listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build the sidecar's `Router` from a `SidecarState`, without binding a
/// socket. Factored out of `main()` so the `#[cfg(test)]` module can exercise
/// routes in-process via `Router::oneshot` (see `13_e2e_testing.md`'s
/// route-smoke pattern).
///
/// `max_body_bytes` raises axum's blanket 2 MiB request-body floor via
/// `DefaultBodyLimit` — the real per-request ceiling is still enforced inside
/// the handlers by the signed token's `claims.upload.max_size`/`exact_size`.
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
        // Server-authoritative multipart part upload (multipart-coordinator feature).
        // The control plane mints a `multipart_part` token for each part; the
        // sidecar verifies and enforces the exact `size` claim before writing.
        .route(
            "/api/file-storage-data/v1/multipart/{file_id}/{version_id}/parts/{part_number}",
            put(upload_multipart_part),
        )
        // Liveness probe: always 200 once the process is up and the router is
        // wired. No backend/dependency check — see module docs for `/readyz`
        // rationale (skipped for this PR).
        .route("/healthz", get(healthz))
        // P2 1.8 remediation: route-level latency + status. Bound to its own
        // state clone (not the shared router state) via `from_fn_with_state`
        // so it wraps every route above regardless of extractor ordering.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            record_request_metrics,
        ))
        .with_state(state)
        .layer(DefaultBodyLimit::max(max_body_bytes))
}

/// Records one `file_storage_sidecar_request_duration_ms` observation per
/// request: route (from [`MatchedPath`], falling back to `"unmatched"` so
/// cardinality stays bounded), method, status, and latency (P2 1.8
/// remediation — the control plane's routes already get an equivalent metric
/// for free from the platform's api-gateway; this process is never proxied by
/// it).
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

/// Liveness probe handler. Always returns `200 OK` with a trivial body once
/// the sidecar process is serving requests. Intentionally does not check
/// backend health — see the module-level note on `/readyz` (skipped here as
/// a fast-follow: `SidecarState` only holds `Arc<dyn StorageBackend>`, not a
/// cheaply-inspectable root path, so a real readiness check would need a new
/// `StorageBackend` method rather than fitting into this step).
async fn healthz() -> &'static str {
    "ok"
}

/// Extract the token from the `fs-token` query param or the `X-FS-Token` header.
fn extract_token(q: &TokenQuery, headers: &HeaderMap) -> Option<String> {
    q.fs_token.clone().or_else(|| {
        headers
            .get("x-fs-token")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    })
}

/// `PUT` upload: verify token (op=PUT), stream bytes straight to the backend.
///
/// P2 1.2b (memory-DoS fix): the body is never buffered whole in this
/// handler — it is converted to a byte stream and handed to
/// `StorageBackend::put_stream`, which writes + hashes chunks as they arrive
/// and aborts mid-stream the moment `claims.upload.max_size` is exceeded.
/// `exact_size`/`expected_hash` can only be checked once the stream is fully
/// drained (the incremental length/hash are only final at that point), so
/// those checks now run *after* `put_stream` returns.
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
        // `put_stream`'s only `Validation` error is the mid-stream `max_size`
        // guard (see `StorageBackend::put_stream`'s default/`LocalFsBackend`
        // implementations) — every other failure is a genuine backend error.
        Err(DomainError::Validation { .. }) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, "exceeds max_size").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "backend put_stream failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response();
        }
    };

    // Enforce the remaining upload constraints now that the streamed
    // length/hash are final.
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

    // P2 1.8 remediation: ingress bytes (sidecar is the only component that
    // ever sees content bytes, so this is the sole place to record them).
    #[allow(clippy::cast_precision_loss)]
    state.metrics.record_ingress_bytes(bytes_written as f64);

    // Finalize callback: notify the control plane that bytes have landed so it
    // can mark the version `available`. The same signed token proves this was
    // a pre-authorized upload (DESIGN §bind-service). `claims.request_id`
    // (P2 1.8) is echoed back as `x-request-id` so both planes' logs for this
    // upload can be correlated.
    if let Err(resp) = finalize_with_control_plane(
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
        return resp;
    }

    (StatusCode::OK, "uploaded").into_response()
}

/// Build the finalize request body bytes (JSON `{size, hash_hex}`).
///
/// Returns an internal-error `Response` (boxed) if JSON serialization fails,
/// which is only possible if `serde_json` itself has a bug (our value is trivial).
#[allow(clippy::result_large_err)]
fn finalize_body(size: i64, hash_hex: &str) -> Result<Vec<u8>, Response> {
    let body = serde_json::json!({ "size": size, "hash_hex": hash_hex });
    serde_json::to_vec(&body).map_err(|e| {
        tracing::error!(error = %e, "failed to serialize finalize request body");
        (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
    })
}

/// Interpret the HTTP response from the control-plane finalize call.
async fn interpret_finalize_response(
    resp: reqwest::Response,
    file_id: Uuid,
    version_id: Uuid,
) -> Result<(), Response> {
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
    // P2 1.11: the detailed status/body stay in the server-side log above —
    // forwarding them to the client would leak the control plane's raw error
    // body (which can carry internal details) to an uploading client.
    Err((StatusCode::BAD_GATEWAY, "finalize failed").into_response())
}

/// Maximum number of attempts (including the first) for a sidecar→control-plane
/// callback POST (finalize or report-part) — P2 1.5. Only transport-level
/// failures (`reqwest::Error::is_connect()` / `is_timeout()`) are retried; a
/// successful-but-error HTTP status is a real 4xx/5xx from the control plane
/// and is returned immediately by the caller's response interpretation.
const CALLBACK_MAX_ATTEMPTS: u32 = 3;

/// Fixed delay between callback retry attempts. Short enough that even the
/// maximum number of attempts adds well under a second to the test suite's
/// wall-clock budget.
const CALLBACK_RETRY_DELAY: Duration = Duration::from_millis(100);

/// POST `body_bytes` to `url` under the sidecar's callback retry policy: up to
/// `CALLBACK_MAX_ATTEMPTS` attempts total, retrying only on a transport
/// connect/timeout failure, with `CALLBACK_RETRY_DELAY` between attempts.
/// Shared by `finalize_with_control_plane` and `report_part_with_control_plane`
/// so both callbacks get the same bounded-retry behavior (P2 1.5).
async fn post_with_retry(
    http: &reqwest::Client,
    url: &str,
    token: &str,
    request_id: &str,
    body_bytes: &[u8],
) -> Result<reqwest::Response, reqwest::Error> {
    let mut attempt: u32 = 1;
    loop {
        let mut req = http
            .post(url)
            .header("content-type", "application/json")
            .header("x-fs-token", token);
        // P2 1.8 remediation: propagate the signed URL's correlation id so the
        // control plane's finalize/report-part log lines can be joined with
        // this sidecar's own logs for the same upload.
        if !request_id.is_empty() {
            req = req.header("x-request-id", request_id);
        }
        let result = req.body(body_bytes.to_vec()).send().await;
        match result {
            Ok(resp) => return Ok(resp),
            Err(e) if attempt < CALLBACK_MAX_ATTEMPTS && (e.is_connect() || e.is_timeout()) => {
                tracing::warn!(
                    attempt,
                    error = %e,
                    "control-plane callback transport error, retrying"
                );
                tokio::time::sleep(CALLBACK_RETRY_DELAY).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Call the control-plane finalize endpoint after a successful PUT.
///
/// Returns `Ok(())` when the control plane accepted the finalize, or
/// `Err(Response)` with a `502 Bad Gateway` response when the callback
/// fails (so the upload handler can surface the failure to the client).
///
/// When `control_base_url` is empty, the callback is skipped (dev mode).
async fn finalize_with_control_plane(
    state: &SidecarState,
    token: &str,
    request_id: &str,
    file_id: Uuid,
    version_id: Uuid,
    size: i64,
    hash_hex: &str,
) -> Result<(), Response> {
    if state.control_base_url.is_empty() {
        return Ok(());
    }

    let url = format!(
        "{}/api/file-storage/v1/files/{}/versions/{}/finalize",
        state.control_base_url.trim_end_matches('/'),
        file_id,
        version_id,
    );

    let body_bytes = finalize_body(size, hash_hex)?;

    match post_with_retry(&state.http, &url, token, request_id, &body_bytes).await {
        Ok(resp) => interpret_finalize_response(resp, file_id, version_id).await,
        Err(e) => {
            tracing::error!(
                %file_id, %version_id, error = %e,
                "control-plane finalize callback failed"
            );
            // P2 1.11: `e` (a `reqwest::Error`) embeds the request URL, i.e.
            // the internal `FS_SIDECAR_CONTROL_URL` host:port — never forward
            // it to the client. The detail is already in the log above.
            Err((StatusCode::BAD_GATEWAY, "finalize failed").into_response())
        }
    }
}

/// Build the report-part request body bytes (JSON `{backend_etag, hash_hex, size}`).
///
/// Returns an internal-error `Response` (boxed) if JSON serialization fails,
/// which is only possible if `serde_json` itself has a bug (our value is trivial).
#[allow(clippy::result_large_err)]
fn report_part_body(backend_etag: &str, hash_hex: &str, size: i64) -> Result<Vec<u8>, Response> {
    let body = serde_json::json!({
        "backend_etag": backend_etag,
        "hash_hex": hash_hex,
        "size": size,
    });
    serde_json::to_vec(&body).map_err(|e| {
        tracing::error!(error = %e, "failed to serialize report-part request body");
        (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
    })
}

/// Interpret the HTTP response from the control-plane report-part call.
async fn interpret_report_part_response(
    resp: reqwest::Response,
    upload_id: Uuid,
    part_number: u32,
) -> Result<(), Response> {
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
    // P2 1.11: same no-leak principle as `interpret_finalize_response` — the
    // detailed status/body stay server-side only.
    Err((StatusCode::BAD_GATEWAY, "report failed").into_response())
}

/// Call the control-plane report-part endpoint after a successful part write.
///
/// This is the sidecar half of the "report part" callback (P2 0.2 group B):
/// without it, nothing ever populates `multipart_upload_parts`, so
/// `complete_multipart_upload`'s `list_multipart_parts` is structurally empty
/// in a real deployment. Mirrors `finalize_with_control_plane`'s contract:
/// returns `Ok(())` when the control plane accepted the report, or
/// `Err(Response)` with a `502 Bad Gateway` when the callback fails (the
/// client should retry — the part write and this report are both idempotent
/// per `(upload_id, part_number)`).
///
/// When `control_base_url` is empty, the callback is skipped (dev mode).
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
) -> Result<(), Response> {
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

    let body_bytes = report_part_body(backend_etag, hash_hex, size)?;

    match post_with_retry(&state.http, &url, token, request_id, &body_bytes).await {
        Ok(resp) => interpret_report_part_response(resp, upload_id, part_number).await,
        Err(e) => {
            tracing::error!(
                %file_id, %version_id, %upload_id, part_number, error = %e,
                "control-plane report-part callback failed"
            );
            // P2 1.11: same no-leak principle as `finalize_with_control_plane`
            // — `e` embeds the internal control-plane URL.
            Err((StatusCode::BAD_GATEWAY, "report failed").into_response())
        }
    }
}

/// Writes one multipart part to `backend`, returning `(body_len, backend_etag,
/// hash_hex)` on success or an early terminal `Response` on any client/backend
/// error.
///
/// Two write models, chosen by the backend's own capabilities (P2 1.7 Stage 6
/// fix — see the "Before this fix" note below for why this branch exists):
/// * `multipart_native` (e.g. `S3Backend`): call the backend's own
///   `upload_part` against its native multipart session
///   (`claims.multipart.backend_handle`, minted by `initiate_multipart_upload`
///   at plan time). `upload_part`'s trait signature takes the whole part as
///   one `Bytes` — S3's `UploadPart` needs the full body up front to sign and
///   send in a single request — so the part is buffered here, bounded by the
///   token's exact `size` claim (the same bound the non-native path enforces
///   via `put_stream`'s `max_size`), so this never buffers more than one
///   part's worth of bytes.
/// * otherwise (e.g. `LocalFsBackend`, which has no native multipart): the
///   original offset-object model — each part is written as its own backend
///   object at `{backend_path}.part.{n}` via `put_stream`, and
///   `complete_multipart_upload`'s local-fs fallback assembles them (§4
///   "otherwise offset-write into `/{file_id}/{version_id}`").
///
/// Before this fix, EVERY backend (including `multipart_native` ones) went
/// through the offset-object path unconditionally, so a real multipart
/// session was `initiate_multipart`'d but never actually received any
/// `UploadPart` calls — `complete_multipart` then failed against the backend
/// (proven by the P2 1.7 Stage 6 S3 e2e suite,
/// `testing/e2e/gears/file_storage/lifecycle_s3/`, which is what surfaced
/// this bug: `CompleteMultipartUpload` 500s against a real S3-compatible
/// endpoint because none of its parts were ever uploaded).
async fn write_multipart_part(
    backend: &dyn StorageBackend,
    claims: &Claims,
    part_number: u32,
    body: Body,
) -> Result<(u64, String, String), Response> {
    if backend.capabilities().multipart_native {
        write_multipart_part_native(backend, claims, part_number, body).await
    } else {
        write_multipart_part_offset_object(backend, claims, part_number, body).await
    }
}

/// `multipart_native` backend write path — see `write_multipart_part`'s doc
/// comment.
async fn write_multipart_part_native(
    backend: &dyn StorageBackend,
    claims: &Claims,
    part_number: u32,
    body: Body,
) -> Result<(u64, String, String), Response> {
    let max_size = claims.multipart.size;
    let mut stream = body.into_data_stream();
    let mut buf = bytes::BytesMut::new();
    loop {
        match stream.next().await {
            Some(Ok(chunk)) => {
                if (buf.len() as u64).saturating_add(chunk.len() as u64) > max_size {
                    return Err((
                        StatusCode::PAYLOAD_TOO_LARGE,
                        format!("part body length exceeds token size claim {max_size}"),
                    )
                        .into_response());
                }
                buf.extend_from_slice(&chunk);
            }
            Some(Err(e)) => {
                tracing::error!(error = %e, part_number, "part body stream read failed");
                return Err((StatusCode::BAD_REQUEST, "body read error").into_response());
            }
            None => break,
        }
    }
    let body_len = buf.len() as u64;
    // FEATURE §4, point 2: reject if body length ≠ size claim. Checked here
    // (before the backend call) rather than after, since the whole part is
    // already buffered — no partial native upload to clean up.
    if body_len != max_size {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("part body length {body_len} does not match token size claim {max_size}"),
        )
            .into_response());
    }
    match backend
        .upload_part(
            &claims.backend_path,
            &claims.multipart.backend_handle,
            part_number,
            buf.freeze(),
        )
        .await
    {
        Ok((etag, hash)) => Ok((body_len, etag, hex::encode(hash))),
        Err(e) => {
            tracing::error!(error = %e, part_number, "backend native upload_part failed");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response())
        }
    }
}

/// Non-native (offset-object) backend write path — see
/// `write_multipart_part`'s doc comment.
async fn write_multipart_part_offset_object(
    backend: &dyn StorageBackend,
    claims: &Claims,
    part_number: u32,
    body: Body,
) -> Result<(u64, String, String), Response> {
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
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "part body length exceeds token size claim {}",
                    claims.multipart.size
                ),
            )
                .into_response());
        }
        Err(e) => {
            tracing::error!(error = %e, part_number, "backend part write failed");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response());
        }
    };

    // FEATURE §4, point 2: reject if body length ≠ size claim. The
    // `max_size` guard above only rejects an *oversized* part mid-stream; an
    // undersized part still streams to completion, so the exact-length check
    // happens here, now that `body_len` is final. The mismatched part is
    // removed so a rejected part never lingers as an orphaned backend object.
    if body_len != claims.multipart.size {
        drop(backend.delete(&part_path).await);
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "part body length {} does not match token size claim {}",
                body_len, claims.multipart.size
            ),
        )
            .into_response());
    }

    let part_etag = hex::encode(part_hash);
    Ok((body_len, part_etag.clone(), part_etag))
}

/// `PUT` multipart part: verify `op=multipart_part` token, stream the part
/// straight to the backend, enforce the exact `size` claim, compute and
/// return the part hash.
///
/// P2 1.2b (memory-DoS fix): the part body is never buffered whole here —
/// like `upload`, it streams through `StorageBackend::put_stream`, which
/// enforces the token's declared `size` as an upper bound (`max_size`) while
/// bytes arrive, aborting mid-stream on an oversized part instead of
/// buffering it first. An *undersized* part can only be detected once the
/// stream is fully drained, so the exact-length check (FEATURE §4, point 2)
/// now runs after the write completes, comparing against the streamed
/// `bytes_written`.
///
/// This is the sidecar half of the server-authoritative multipart model. The
/// control plane mints the token (sole minter, ADR-0004); the sidecar only
/// verifies and enforces — it can never mint a token.
///
/// Idempotent per `(upload_id, part_number)`: a re-PUT with the same token
/// overwrites the earlier part (safe for resume — ADR-0004 §4).
///
/// @cpt-cf-file-storage-fr-multipart-upload
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

    // Verify op and path bindings.
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

    // Verify part-number binding (prevents replaying another part's token here).
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

    // Write the part — see `write_multipart_part`'s doc comment for the two
    // models this dispatches between, and why the branch exists at all (P2
    // 1.7 Stage 6 fix).
    let (body_len, backend_etag, hash_hex) =
        match write_multipart_part(backend.as_ref(), &claims, part_number, body).await {
            Ok(v) => v,
            Err(resp) => return resp,
        };

    // P2 1.8 remediation: ingress bytes for this part.
    #[allow(clippy::cast_precision_loss)]
    state.metrics.record_ingress_bytes(body_len as f64);

    // Report-part callback: notify the control plane that this part's bytes
    // have landed so it can record the part row `complete_multipart_upload`
    // assembles from (P2 0.2 group B — the "report part" fix).
    // `claims.request_id` (P2 1.8) is echoed back as `x-request-id` so both
    // planes' logs for this upload can be correlated.
    if let Err(resp) = report_part_with_control_plane(
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
        return resp;
    }

    // Return the part hash and ETag so callers can track per-part integrity.
    let body = serde_json::json!({
        "part_number": part_number,
        "etag": backend_etag,
        "hash_algorithm": "SHA-256",
        "hash": hash_hex,
    });
    (StatusCode::OK, axum::Json(body)).into_response()
}

/// Fallback `Content-Type` for every sidecar download response.
///
/// P2 1.11: the sidecar is a stateless byte-mover — the signed-URL `Claims`
/// it verifies (`infra::signed_url::Claims`) carry no MIME field, and the
/// version's stored MIME type lives only in the control plane's DB. Threading
/// the real MIME through requires either a `Claims` schema change or a
/// control-plane lookup from the sidecar, both out of scope for this step;
/// flagged as a follow-up to land alongside/after 1.10. A generic
/// octet-stream type is always a safe (if non-specific) answer.
const FALLBACK_CONTENT_TYPE: &str = "application/octet-stream";

/// Build a `Content-Range` header value, e.g. `bytes 0-99/1000` or
/// `bytes */1000` (the unsatisfiable-range form, RFC 9110 §14.4).
fn header_value(s: &str) -> HeaderValue {
    // Every caller builds this from ASCII digits/literals, so this can only
    // fail if a future edit introduces non-ASCII content; fall back to a
    // clearly-invalid-but-safe placeholder rather than panicking.
    HeaderValue::from_str(s).unwrap_or_else(|_| HeaderValue::from_static("invalid"))
}

/// `GET` download: verify token (op=GET), stream bytes, honour `Range`.
///
/// P2 1.11: every backend error is now mapped distinctly instead of folding
/// blob-not-found, unsatisfiable-range, and genuine I/O failures into a
/// blanket `416`. `Content-Range` is emitted on every `206` (and on `416`,
/// per RFC 9110 §14.4) and `Content-Type` is always set — see
/// `FALLBACK_CONTENT_TYPE` for why it is not (yet) the real stored MIME.
/// `ETag` is intentionally omitted: the sidecar has no stored content hash
/// available for a GET token without the same kind of claims/DB-lookup
/// change noted above (follow-up, coordinate with 1.10).
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

    // Resolve existence first, distinctly from any later I/O failure: a
    // missing blob must be `404`, never folded into `416` (bad range) or
    // `500` (genuine backend fault). `exists` already distinguishes a real
    // `NotFound` from other I/O errors per backend (see
    // `StorageBackend::exists`'s contract), so anything failing after this
    // point is a genuine backend error, not a missing blob.
    match backend.exists(path).await {
        Ok(true) => {}
        Ok(false) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "backend existence check failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response();
        }
    }

    // Range support (random read access) — a single signed URL serves many ranges.
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(range::parse);

    match range {
        Some(r) => download_range(&state, &backend, path, r).await,
        None => download_whole(&state, &backend, path).await,
    }
}

/// Serve a `Range`-qualified `GET` once the blob's existence has already been
/// confirmed by the caller (`download`). Split out of `download` to keep its
/// cognitive complexity down.
async fn download_range(
    state: &SidecarState,
    backend: &Arc<dyn StorageBackend>,
    path: &str,
    r: file_storage_sdk::ByteRange,
) -> Response {
    let total = match backend.size(path).await {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, "backend size lookup failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response();
        }
    };
    let Some((start, end)) = r.resolve(total) else {
        // Genuine range-unsatisfiable (RFC 9110 §14.4): the client asked for
        // bytes past the end of a blob that does exist.
        let mut resp = (StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable").into_response();
        resp.headers_mut().insert(
            header::CONTENT_RANGE,
            header_value(&format!("bytes */{total}")),
        );
        return resp;
    };
    match backend.get_range(path, r).await {
        Ok(bytes) => {
            // P2 1.8 remediation: egress bytes for this range read.
            #[allow(clippy::cast_precision_loss)]
            state.metrics.record_egress_bytes(bytes.len() as f64);
            let mut resp = (StatusCode::PARTIAL_CONTENT, bytes).into_response();
            let headers_mut = resp.headers_mut();
            headers_mut.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            headers_mut.insert(
                header::CONTENT_RANGE,
                header_value(&format!("bytes {start}-{end}/{total}")),
            );
            headers_mut.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(FALLBACK_CONTENT_TYPE),
            );
            resp
        }
        Err(e) => {
            // Existence and range satisfiability were already confirmed
            // above, so a failure here is a genuine I/O fault (e.g. disk
            // error), not a missing blob or a bad range.
            tracing::error!(error = %e, "backend get_range failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response()
        }
    }
}

/// Serve a whole-blob `GET` (no `Range` header) once the blob's existence has
/// already been confirmed by the caller (`download`). Split out of
/// `download` to keep its cognitive complexity down.
async fn download_whole(
    state: &SidecarState,
    backend: &Arc<dyn StorageBackend>,
    path: &str,
) -> Response {
    match backend.get(path).await {
        Ok(bytes) => {
            // P2 1.8 remediation: egress bytes for this whole-blob read.
            #[allow(clippy::cast_precision_loss)]
            state.metrics.record_egress_bytes(bytes.len() as f64);
            let mut resp = (StatusCode::OK, bytes).into_response();
            let headers_mut = resp.headers_mut();
            headers_mut.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            headers_mut.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(FALLBACK_CONTENT_TYPE),
            );
            resp
        }
        Err(e) => {
            // Existence was already confirmed above, so this is a genuine
            // backend fault, not a missing blob.
            tracing::error!(error = %e, "backend get failed after existence check");
            (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use time::OffsetDateTime;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tower::ServiceExt;
    use uuid::Uuid;

    use file_storage::infra::backend::{BackendRegistry, InMemoryBackend, StorageBackend};
    use file_storage::infra::metrics::NoopMetrics;
    use file_storage::infra::signed_url::{Claims, Issuer, MultipartClaims, Op, UploadConstraints};

    use super::{DEFAULT_MAX_BODY_BYTES, SidecarState, build_router, finalize_with_control_plane};

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
            http: reqwest::Client::new(),
            metrics: Arc::new(NoopMetrics),
        };
        (state, issuer, backend)
    }

    /// Mint a signed `op = get` download token for `(file_id, version_id, backend_path)`.
    fn download_token(
        issuer: &Issuer,
        file_id: Uuid,
        version_id: Uuid,
        backend_path: &str,
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
        };
        issuer
            .issue(claims, OffsetDateTime::now_utc())
            .expect("issue download token")
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
        };
        issuer
            .issue(claims, OffsetDateTime::now_utc())
            .expect("issue multipart part token")
    }

    /// P2 1.7 Stage 6 regression: `upload_multipart_part` must dispatch to the
    /// backend's own `upload_part` (native multipart) for a
    /// `multipart_native` backend, instead of unconditionally falling back to
    /// the local-fs-style offset-object model. That bug was silent until the
    /// S3 e2e suite (`testing/e2e/gears/file_storage/lifecycle_s3/`) surfaced
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
            http: reqwest::Client::new(),
            metrics: Arc::new(NoopMetrics),
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
        // landed via `upload_part`, proving the dispatch fix.
        let content_hash = backend
            .complete_multipart(&backend_path, &backend_handle, &[(1, etag1), (2, etag2)])
            .await
            .expect("complete native multipart session - both parts must be real");

        let assembled = backend
            .get(&backend_path)
            .await
            .expect("read assembled object");
        let mut expected = part1;
        expected.extend_from_slice(&part2);
        assert_eq!(
            &assembled[..],
            &expected[..],
            "assembled object must be the exact concatenation of the two parts"
        );
        assert_eq!(
            content_hash,
            file_storage::infra::content::hash::sha256(&expected),
            "complete_multipart's returned digest must match the assembled bytes"
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
}
