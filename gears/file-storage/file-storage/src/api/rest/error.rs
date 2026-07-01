//! Mapping `DomainError` → RFC-9457 `CanonicalError` at the REST boundary.

use toolkit::api::canonical_prelude::*;

use crate::domain::error::DomainError;

#[resource_error("gts.cf.fstorage.file.object.v1~")]
struct FileResourceError;

impl From<DomainError> for CanonicalError {
    #[allow(clippy::cognitive_complexity)]
    fn from(e: DomainError) -> Self {
        match &e {
            DomainError::FileNotFound { id } => {
                FileResourceError::not_found(format!("File {id} not found"))
                    .with_resource(id.to_string())
                    .create()
            }
            DomainError::VersionNotFound {
                file_id,
                version_id,
            } => FileResourceError::not_found(format!("Version {file_id}/{version_id} not found"))
                .with_resource(version_id.to_string())
                .create(),
            DomainError::Validation { field, message } => FileResourceError::invalid_argument()
                .with_field_violation(field, message, "VALIDATION")
                .create(),
            DomainError::InvalidGtsType { value } => FileResourceError::invalid_argument()
                .with_field_violation(
                    "gts_file_type",
                    format!("invalid GTS type: {value}"),
                    "INVALID_GTS_TYPE",
                )
                .create(),
            DomainError::MimeMismatch { declared, detected } => {
                FileResourceError::invalid_argument()
                    .with_field_violation(
                        "mime_type",
                        format!("declared '{declared}' != detected '{detected}'"),
                        "MIME_MISMATCH",
                    )
                    .create()
            }
            DomainError::HashMismatch { expected, got } => FileResourceError::invalid_argument()
                .with_field_violation(
                    "hash",
                    format!("expected {expected}, got {got}"),
                    "HASH_MISMATCH",
                )
                .create(),
            DomainError::UnknownBackend { backend_id } => FileResourceError::invalid_argument()
                .with_field_violation(
                    "backend_id",
                    format!("unknown backend {backend_id}"),
                    "UNKNOWN_BACKEND",
                )
                .create(),
            DomainError::Conflict { message } => FileResourceError::aborted(message.clone())
                .with_reason("CONFLICT")
                .create(),
            DomainError::PreconditionFailed { message } => FileResourceError::failed_precondition()
                .with_precondition_violation("content", message.clone(), "IF_MATCH")
                .create(),
            DomainError::TokenInvalid { reason } => FileResourceError::permission_denied()
                .with_reason(format!("INVALID_TOKEN: {reason}"))
                .create(),
            DomainError::Forbidden => FileResourceError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),
            DomainError::Backend {
                backend_id,
                message,
            } => {
                tracing::error!(backend_id, message, "storage backend error");
                CanonicalError::internal("storage backend error").create()
            }
            DomainError::Database { .. } => {
                tracing::error!(error = ?e, "database error");
                CanonicalError::internal("internal database error").create()
            }
            DomainError::InternalError => {
                tracing::error!(error = ?e, "internal error");
                CanonicalError::internal("internal error").create()
            }
            DomainError::PolicyMimeNotAllowed { mime_type } => {
                FileResourceError::invalid_argument()
                    .with_field_violation(
                        "mime_type",
                        format!("MIME type '{mime_type}' is not permitted by policy"),
                        "POLICY_MIME_NOT_ALLOWED",
                    )
                    .create()
            }
            DomainError::PolicySizeExceeded {
                limit_bytes,
                limit_source,
            } => FileResourceError::out_of_range(format!(
                "file size exceeds limit of {limit_bytes} bytes ({limit_source})"
            ))
            .with_field_violation(
                "size",
                format!("exceeds limit of {limit_bytes} bytes ({limit_source})"),
                "POLICY_SIZE_EXCEEDED",
            )
            .create(),
            DomainError::PolicyMetadataExceeded { reason } => FileResourceError::invalid_argument()
                .with_field_violation(
                    "custom_metadata",
                    reason.clone(),
                    "POLICY_METADATA_EXCEEDED",
                )
                .create(),
            DomainError::QuotaExceeded { reason } => {
                FileResourceError::resource_exhausted(reason.clone())
                    .with_quota_violation("storage_bytes", reason.clone())
                    .create()
            }
            DomainError::MultipartNotSupported { backend_id } => {
                FileResourceError::invalid_argument()
                    .with_field_violation(
                        "backend",
                        format!("backend '{backend_id}' does not support multipart upload"),
                        "MULTIPART_NOT_SUPPORTED",
                    )
                    .create()
            }
            DomainError::MultipartUploadNotFound { upload_id } => FileResourceError::not_found(
                format!("Multipart upload session {upload_id} not found"),
            )
            .with_resource(upload_id.to_string())
            .create(),
            DomainError::MultipartUploadNotInProgress { upload_id, state } => {
                FileResourceError::aborted(format!(
                    "Multipart upload session {upload_id} is not in progress (state: {state})"
                ))
                .with_reason("MULTIPART_NOT_IN_PROGRESS")
                .create()
            }
            DomainError::VersionedFileMigrationNotSupported { file_id } => {
                FileResourceError::aborted(format!(
                    "File {file_id} has multiple versions and cannot be migrated between backends"
                ))
                .with_reason("VERSIONED_FILE_MIGRATION_NOT_SUPPORTED")
                .create()
            }
        }
    }
}
