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
//!
//! After a successful upload the sidecar pre-registers + binds the version
//! against the control plane on the user's behalf (FS SDK, on-behalf-of). That
//! control callback is performed by `file_storage::domain::service::FileService`
//! ({`finalize_upload`, `bind`}); wiring the s2s client to invoke it is the
//! remaining deployment step and is intentionally left out of this thin binary.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use file_storage::infra::backend::{LocalFsBackend, StorageBackend};
use file_storage::infra::content::hash::sha256;
use file_storage::infra::content::{hash, range};
use file_storage::infra::signed_url::{Op, Verifier};

#[derive(Clone)]
struct SidecarState {
    verifier: Arc<Verifier>,
    backend: Arc<dyn StorageBackend>,
}

#[derive(Debug, Deserialize)]
struct TokenQuery {
    #[serde(rename = "fs-token")]
    fs_token: Option<String>,
}

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

    let state = SidecarState {
        verifier: Arc::new(
            Verifier::from_public_key(public_key)
                .map_err(|e| anyhow::anyhow!("invalid FS_SIDECAR_PUBLIC_KEY: {e}"))?,
        ),
        backend: Arc::new(LocalFsBackend::new("local-fs", root)),
    };

    let app = Router::new()
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
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "file-storage sidecar listening");
    axum::serve(listener, app).await?;
    Ok(())
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

/// `PUT` upload: verify token (op=PUT), enforce constraints, write bytes.
async fn upload(
    State(state): State<SidecarState>,
    Path((file_id, version_id)): Path<(Uuid, Uuid)>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    body: Bytes,
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

    // Enforce upload constraints carried in the token.
    let len = body.len() as u64;
    if claims.upload.max_size.is_some_and(|max| len > max) {
        return (StatusCode::PAYLOAD_TOO_LARGE, "exceeds max_size").into_response();
    }
    if claims.upload.exact_size.is_some_and(|exact| len != exact) {
        return (StatusCode::BAD_REQUEST, "size does not match exact_size").into_response();
    }
    if let Some(expected) = &claims.upload.expected_hash {
        let got = format!("{}:{}", hash::ALGORITHM, hash::sha256_hex(&body));
        if !expected.eq_ignore_ascii_case(&got) {
            return (StatusCode::BAD_REQUEST, "content hash mismatch").into_response();
        }
    }

    match state.backend.put(&claims.backend_path, body).await {
        Ok(()) => {
            // NOTE: the control-plane pre-register + bind on-behalf-of happens here
            // via the FS SDK in a full deployment (see module docs).
            (StatusCode::OK, "uploaded").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "backend put failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response()
        }
    }
}

/// `PUT` multipart part: verify `op=multipart_part` token, enforce the exact
/// `size` claim (FEATURE §4, point 2: reject `413` before writing any bytes),
/// write the part via the backend, compute and return the part hash.
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
    body: Bytes,
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

    // FEATURE §4, point 2: reject 413 if body length ≠ size claim — BEFORE writing.
    // This is the enforcement point that closes the oversized-part abuse vector.
    let body_len = body.len() as u64;
    if body_len != claims.multipart.size {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "part body length {} does not match token size claim {}",
                body_len, claims.multipart.size
            ),
        )
            .into_response();
    }

    // Compute the part hash before the write so the caller can persist it.
    let part_hash = sha256(&body);
    let part_etag = hex::encode(&part_hash);

    // Write the part. For a `multipart_native` backend this would call
    // `upload_part`; the sidecar here uses the simple `put` into the versioned
    // path for the local-fs backend (offset-write model, §4 "otherwise
    // offset-write into /{file_id}/{version_id}").
    //
    // NOTE: a production sidecar would call `backend.upload_part(...)` when the
    // backend supports native multipart. For the current thin binary (local-fs
    // only, no S3) we persist each part as a separate object keyed by path + part
    // and rely on `complete_multipart_upload` to assemble them.
    let part_path = format!("{}.part.{}", claims.backend_path, part_number);
    match state.backend.put(&part_path, body).await {
        Ok(()) => {
            // Return the part hash and ETag so callers can track per-part integrity.
            let body = serde_json::json!({
                "part_number": part_number,
                "etag": part_etag,
                "hash_algorithm": "SHA-256",
                "hash": part_etag,
            });
            (StatusCode::OK, axum::Json(body)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, part_number, "backend part write failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response()
        }
    }
}

/// `GET` download: verify token (op=GET), stream bytes, honour `Range`.
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

    // Range support (random read access) — a single signed URL serves many ranges.
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(range::parse);

    match range {
        Some(r) => match state.backend.get_range(&claims.backend_path, r).await {
            Ok(bytes) => (
                StatusCode::PARTIAL_CONTENT,
                [(header::ACCEPT_RANGES, "bytes")],
                bytes,
            )
                .into_response(),
            Err(_) => (StatusCode::RANGE_NOT_SATISFIABLE, "bad range").into_response(),
        },
        None => match state.backend.get(&claims.backend_path).await {
            Ok(bytes) => {
                (StatusCode::OK, [(header::ACCEPT_RANGES, "bytes")], bytes).into_response()
            }
            Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
        },
    }
}
