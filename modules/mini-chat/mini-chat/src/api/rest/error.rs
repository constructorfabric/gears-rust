use axum::response::IntoResponse;
use http::StatusCode;
use modkit::api::problem::Problem;
use serde::Serialize;

use crate::domain::error::DomainError;

/// `OpenAPI` `QuotaExceededError` schema — returned for 429 quota responses
/// instead of `Problem` so the response matches the contract exactly.
#[derive(Debug, Serialize)]
pub(crate) struct QuotaExceededResponse {
    pub code: &'static str,
    pub message: String,
    pub quota_scope: &'static str,
}

impl QuotaExceededResponse {
    pub fn uploads() -> Self {
        Self {
            code: "quota_exceeded",
            message: "Upload quota exceeded".to_owned(),
            quota_scope: "uploads",
        }
    }
}

impl IntoResponse for QuotaExceededResponse {
    fn into_response(self) -> axum::response::Response {
        (StatusCode::TOO_MANY_REQUESTS, axum::Json(self)).into_response()
    }
}

/// Convert a `DomainError` to an `axum::response::Response`, using the
/// `OpenAPI` `QuotaExceededError` schema for quota errors and `Problem` for everything else.
pub(crate) fn domain_error_to_response(e: &DomainError) -> axum::response::Response {
    match e {
        DomainError::UploadQuotaExceeded => QuotaExceededResponse::uploads().into_response(),
        other => domain_error_to_problem(other).into_response(),
    }
}

/// Map domain errors to RFC9457 Problem responses.
///
/// This is the single canonical mapping used by both the `From<DomainError>` impl
/// and handler-local error conversions.
pub(crate) fn domain_error_to_problem(e: &DomainError) -> Problem {
    let trace_id = tracing::Span::current()
        .id()
        .map(|id| id.into_u64().to_string());
    match e {
        DomainError::ChatNotFound { id } => Problem::new(
            StatusCode::NOT_FOUND,
            "Chat Not Found",
            format!("Chat with id {id} was not found"),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::InvalidModel { model } => Problem::new(
            StatusCode::BAD_REQUEST,
            "Invalid Model",
            format!("Model '{model}' is not available in the catalog"),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::Validation { message } => {
            Problem::new(StatusCode::BAD_REQUEST, "Validation Error", message.clone())
                .with_trace_id(trace_id.unwrap_or_default())
        }

        // Security: mask Forbidden as 404 to prevent information leakage
        DomainError::Forbidden { .. } => Problem::new(
            StatusCode::NOT_FOUND,
            "Not Found",
            "Resource not found or not accessible",
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::Conflict { code, message } => {
            Problem::new(StatusCode::CONFLICT, code.clone(), message.clone())
                .with_trace_id(trace_id.unwrap_or_default())
        }

        DomainError::NotFound { entity, id } => Problem::new(
            StatusCode::NOT_FOUND,
            format!("{entity} Not Found"),
            format!("{entity} with id {id} was not found"),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::MessageNotFound { id } => Problem::new(
            StatusCode::NOT_FOUND,
            "Message Not Found",
            format!("Message with id {id} was not found"),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::InvalidReactionTarget { id } => Problem::new(
            StatusCode::BAD_REQUEST,
            "Invalid Reaction Target",
            format!("Message {id} is not an assistant message"),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        // Security: mask entity type — do not reveal "attachment" vs "chat"
        DomainError::AttachmentNotFound { id } => Problem::new(
            StatusCode::NOT_FOUND,
            "Not Found",
            format!("Resource not found: {id}"),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::AttachmentNotReady { id } => Problem::new(
            StatusCode::BAD_REQUEST,
            "Attachment Not Ready",
            format!("Attachment {id} is not ready"),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::InvalidAttachment { message } => Problem::new(
            StatusCode::BAD_REQUEST,
            "Invalid Attachment",
            message.clone(),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::FileTooLarge { max_bytes } => Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "File Too Large",
            format!("File exceeds maximum size of {max_bytes} bytes"),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::UnsupportedFileType { content_type } => Problem::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported File Type",
            format!("Unsupported file type: {content_type}"),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::DocumentLimitExceeded { max } => Problem::new(
            StatusCode::CONFLICT,
            "Document Limit Exceeded",
            format!("Maximum {max} documents per chat"),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::UploadSizeLimitExceeded { max_mb } => Problem::new(
            StatusCode::CONFLICT,
            "Upload Size Limit Exceeded",
            format!("Maximum {max_mb} MB per chat"),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::UploadQuotaExceeded => Problem::new(
            StatusCode::TOO_MANY_REQUESTS,
            "Upload Quota Exceeded",
            "Upload quota exceeded",
        )
        .with_code("quota_exceeded")
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::ModelNotFound { model_id } => Problem::new(
            StatusCode::NOT_FOUND,
            "model_not_found",
            format!("Model '{model_id}' was not found"),
        )
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::AlreadyExists { message } => {
            Problem::new(StatusCode::CONFLICT, "Already Exists", message.clone())
                .with_trace_id(trace_id.unwrap_or_default())
        }

        DomainError::ImageInRagScope { id } => Problem::new(
            StatusCode::BAD_REQUEST,
            "Invalid Request",
            format!("Images cannot be used in rag_attachment_ids: {id}"),
        )
        .with_code("invalid_request")
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::AttachmentIdOverlap { id } => Problem::new(
            StatusCode::BAD_REQUEST,
            "Invalid Request",
            format!("Attachment ID appears in both attachment_ids and rag_attachment_ids: {id}"),
        )
        .with_code("invalid_request")
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::DuplicateAttachmentId { id } => Problem::new(
            StatusCode::BAD_REQUEST,
            "Invalid Request",
            format!("Duplicate attachment ID: {id}"),
        )
        .with_code("invalid_request")
        .with_trace_id(trace_id.unwrap_or_default()),

        DomainError::Database { .. } | DomainError::Internal { .. } => {
            tracing::error!(error = ?e, "Internal error occurred");
            Problem::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Error",
                "An internal error occurred",
            )
            .with_trace_id(trace_id.unwrap_or_default())
        }
    }
}

/// Implement `Into<Problem>` for `DomainError` so `?` works in handlers.
impl From<DomainError> for Problem {
    fn from(e: DomainError) -> Self {
        domain_error_to_problem(&e)
    }
}

#[cfg(test)]
mod tests {
    use super::domain_error_to_problem;
    use crate::domain::error::DomainError;
    use axum::http::StatusCode;

    #[test]
    fn file_too_large_maps_to_413() {
        let problem = domain_error_to_problem(&DomainError::FileTooLarge {
            max_bytes: 26_214_400,
        });
        assert_eq!(problem.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(problem.title, "File Too Large");
    }

    #[test]
    fn unsupported_file_type_maps_to_415() {
        let problem = domain_error_to_problem(&DomainError::UnsupportedFileType {
            content_type: "video/mp4".to_owned(),
        });
        assert_eq!(problem.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(problem.title, "Unsupported File Type");
    }

    #[test]
    fn document_limit_exceeded_maps_to_409() {
        let problem = domain_error_to_problem(&DomainError::DocumentLimitExceeded { max: 50 });
        assert_eq!(problem.status, StatusCode::CONFLICT);
        assert_eq!(problem.title, "Document Limit Exceeded");
    }

    #[test]
    fn upload_size_limit_exceeded_maps_to_409() {
        let problem =
            domain_error_to_problem(&DomainError::UploadSizeLimitExceeded { max_mb: 100 });
        assert_eq!(problem.status, StatusCode::CONFLICT);
        assert_eq!(problem.title, "Upload Size Limit Exceeded");
    }

    #[test]
    fn upload_quota_exceeded_maps_to_429() {
        let problem = domain_error_to_problem(&DomainError::UploadQuotaExceeded);
        assert_eq!(problem.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(problem.title, "Upload Quota Exceeded");
        assert_eq!(problem.code, "quota_exceeded");
    }

    #[test]
    fn attachment_not_found_masked_as_404() {
        let problem = domain_error_to_problem(&DomainError::AttachmentNotFound {
            id: uuid::Uuid::nil(),
        });
        assert_eq!(problem.status, StatusCode::NOT_FOUND);
        assert_eq!(problem.title, "Not Found");
        assert!(
            !problem.detail.contains("Attachment"),
            "entity type must not leak in detail"
        );
    }

    #[test]
    fn attachment_not_ready_maps_to_400() {
        let problem = domain_error_to_problem(&DomainError::AttachmentNotReady {
            id: uuid::Uuid::nil(),
        });
        assert_eq!(problem.status, StatusCode::BAD_REQUEST);
        assert_eq!(problem.title, "Attachment Not Ready");
    }

    #[test]
    fn forbidden_masked_as_404() {
        let problem = domain_error_to_problem(&DomainError::Forbidden {
            message: "access denied".to_owned(),
        });
        assert_eq!(problem.status, StatusCode::NOT_FOUND);
        assert_eq!(problem.title, "Not Found");
        assert!(
            !problem.detail.contains("access denied"),
            "forbidden reason must not leak"
        );
    }

    #[test]
    fn chat_not_found_maps_to_404() {
        let problem = domain_error_to_problem(&DomainError::ChatNotFound {
            id: uuid::Uuid::nil(),
        });
        assert_eq!(problem.status, StatusCode::NOT_FOUND);
        assert_eq!(problem.title, "Chat Not Found");
    }

    #[test]
    fn internal_error_maps_to_500() {
        let problem = domain_error_to_problem(&DomainError::Internal {
            message: "boom".to_owned(),
        });
        assert_eq!(problem.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(problem.title, "Internal Error");
        assert!(!problem.detail.contains("boom"));
    }

    #[test]
    fn already_exists_maps_to_409() {
        let problem = domain_error_to_problem(&DomainError::AlreadyExists {
            message: "resource".to_owned(),
        });
        assert_eq!(problem.status, StatusCode::CONFLICT);
        assert_eq!(problem.title, "Already Exists");
    }

    #[test]
    fn quota_exceeded_response_serializes_per_openapi() {
        let resp = super::QuotaExceededResponse::uploads();
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["code"], "quota_exceeded");
        assert_eq!(json["quota_scope"], "uploads");
        assert!(json["message"].as_str().unwrap().contains("quota"));
    }

    #[test]
    fn image_in_rag_scope_maps_to_400() {
        let id = uuid::Uuid::new_v4();
        let problem = domain_error_to_problem(&DomainError::ImageInRagScope { id });
        assert_eq!(problem.status, StatusCode::BAD_REQUEST);
        assert_eq!(problem.title, "Invalid Request");
        assert!(problem.detail.contains(&id.to_string()));
    }

    #[test]
    fn attachment_id_overlap_maps_to_400() {
        let id = uuid::Uuid::new_v4();
        let problem = domain_error_to_problem(&DomainError::AttachmentIdOverlap { id });
        assert_eq!(problem.status, StatusCode::BAD_REQUEST);
        assert_eq!(problem.title, "Invalid Request");
        assert!(problem.detail.contains("both"));
    }

    #[test]
    fn duplicate_attachment_id_maps_to_400() {
        let id = uuid::Uuid::new_v4();
        let problem = domain_error_to_problem(&DomainError::DuplicateAttachmentId { id });
        assert_eq!(problem.status, StatusCode::BAD_REQUEST);
        assert_eq!(problem.title, "Invalid Request");
        assert!(problem.detail.contains(&id.to_string()));
    }
}
