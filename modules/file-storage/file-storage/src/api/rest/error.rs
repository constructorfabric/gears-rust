//! `FileStorageError` → RFC 7807 `Problem` mapping.

use axum::http::StatusCode;
use file_storage_sdk::FileStorageError;
use modkit::api::problem::Problem;

use crate::domain::error::DomainError;

pub fn file_storage_error_to_problem(err: &FileStorageError, instance: &str) -> Problem {
    let trace_id = tracing::Span::current()
        .id()
        .map(|id| id.into_u64().to_string());

    let (status, title, code) = match err {
        FileStorageError::NotFound => (StatusCode::NOT_FOUND, "Not Found", "not_found"),
        FileStorageError::AccessDenied => (StatusCode::FORBIDDEN, "Forbidden", "access_denied"),
        FileStorageError::BadRequest(_) => (StatusCode::BAD_REQUEST, "Bad Request", "bad_request"),
        FileStorageError::EtagMismatch => (
            StatusCode::PRECONDITION_FAILED,
            "Etag Mismatch",
            "etag_mismatch",
        ),
        FileStorageError::InvalidStatusTransition(_) => (
            StatusCode::CONFLICT,
            "Invalid Status Transition",
            "invalid_status_transition",
        ),
        FileStorageError::CapabilityUnavailable(_) => (
            StatusCode::CONFLICT,
            "Capability Unavailable",
            "capability_unavailable",
        ),
        FileStorageError::PayloadTooLarge { .. } => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "Payload Too Large",
            "payload_too_large",
        ),
        FileStorageError::UploadExpired => (StatusCode::GONE, "Upload Expired", "upload_expired"),
        FileStorageError::BackendFailure(_) => (
            StatusCode::BAD_GATEWAY,
            "Backend Failure",
            "backend_failure",
        ),
        FileStorageError::Internal => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "internal",
        ),
    };

    let detail = err.to_string();
    let mut p = Problem::new(status, title, detail).with_code(code).with_instance(instance);
    if let Some(t) = trace_id {
        p = p.with_trace_id(t);
    }
    p
}

impl From<FileStorageError> for ProblemWrapper {
    fn from(e: FileStorageError) -> Self {
        Self(file_storage_error_to_problem(&e, "/"))
    }
}

impl From<DomainError> for ProblemWrapper {
    fn from(e: DomainError) -> Self {
        Self(file_storage_error_to_problem(&FileStorageError::from(e), "/"))
    }
}

/// Newtype to anchor `From<…> for Problem` impls without orphan-rule
/// conflicts.
pub struct ProblemWrapper(pub Problem);

impl From<ProblemWrapper> for Problem {
    fn from(w: ProblemWrapper) -> Self {
        w.0
    }
}
