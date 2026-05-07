//! Error type for the `FileStorage` SDK.
//!
//! Each variant maps 1:1 to a `ProblemDetails.code` value (see
//! [`openapi.yaml`](../../docs/openapi.yaml) `components.schemas.ProblemDetails`).

use thiserror::Error;

/// Errors returned by the `FileStorage` SDK.
#[derive(Debug, Clone, Error)]
pub enum FileStorageError {
    /// `code = not_found` — file, backend, or upload session missing.
    /// Returned identically for "absent" and "not visible to this tenant"
    /// (see ADR-0002).
    #[error("not found")]
    NotFound,

    /// `code = access_denied` — authz rejected the request.
    #[error("access denied")]
    AccessDenied,

    /// `code = bad_request` — request validation failed (missing fields,
    /// invalid GTS file type, malformed slug, etc.).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// `code = etag_mismatch` — the caller's `etag` does not match the
    /// current value on the row. Caller should re-read with `get_file_info`
    /// and retry.
    #[error("etag mismatch")]
    EtagMismatch,

    /// `code = invalid_status_transition` — `change_status` was called with
    /// a target state the row cannot move to (e.g. `Uploaded` →
    /// `PendingUpload`).
    #[error("invalid status transition: {0}")]
    InvalidStatusTransition(String),

    /// `code = capability_unavailable` — the backend does not declare or
    /// has disabled the capability needed for this operation.
    #[error("capability unavailable: {0}")]
    CapabilityUnavailable(String),

    /// `code = payload_too_large` — bytes exceed `max_file_size_bytes`.
    #[error("payload too large (max {max_bytes} bytes)")]
    PayloadTooLarge { max_bytes: u64 },

    /// `code = upload_expired` — the presigned URL TTL elapsed before
    /// `change_status` confirmed the upload.
    #[error("upload expired")]
    UploadExpired,

    /// `code = backend_failure` — wrapped error from the storage backend.
    #[error("backend failure: {0}")]
    BackendFailure(String),

    /// `code = internal` — unexpected server error.
    #[error("internal error")]
    Internal,
}
