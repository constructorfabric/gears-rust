use http::StatusCode;
use modkit::api::problem::Problem;

use crate::domain::error::DomainError;

impl From<DomainError> for Problem {
    fn from(e: DomainError) -> Self {
        let trace_id = tracing::Span::current()
            .id()
            .map(|id| id.into_u64().to_string());
        match &e {
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

            DomainError::Forbidden => Problem::new(
                StatusCode::FORBIDDEN,
                "Access denied",
                "You do not have permission to perform this action",
            )
            .with_trace_id(trace_id.unwrap_or_default()),

            DomainError::Conflict { code, message } => {
                Problem::new(StatusCode::CONFLICT, "Conflict", message.clone())
                    .with_code(code.clone())
                    .with_trace_id(trace_id.unwrap_or_default())
            }

            DomainError::NotFound { entity, id } => Problem::new(
                StatusCode::NOT_FOUND,
                format!("{entity} not found"),
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

            DomainError::ModelNotFound { model_id } => Problem::new(
                StatusCode::NOT_FOUND,
                "model_not_found",
                format!("Model '{model_id}' was not found"),
            )
            .with_trace_id(trace_id.unwrap_or_default()),

            DomainError::Database { message } | DomainError::InternalError { message } => {
                tracing::error!(error_message = %message, "internal error occurred");
                Problem::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Error",
                    "An internal error occurred",
                )
                .with_trace_id(trace_id.unwrap_or_default())
            }

            DomainError::WebSearchDisabled => Problem::new(
                StatusCode::BAD_REQUEST,
                "web_search_disabled",
                "Web search is currently disabled",
            )
            .with_trace_id(trace_id.unwrap_or_default()),

            DomainError::WebSearchCallsExceeded => Problem::new(
                StatusCode::BAD_REQUEST,
                "web_search_calls_exceeded",
                "Web search calls exceeded for this message",
            )
            .with_trace_id(trace_id.unwrap_or_default()),

            DomainError::UnsupportedFileType { mime } => Problem::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_file_type",
                format!("Unsupported file type: {mime}"),
            )
            .with_code("unsupported_file_type".to_owned())
            .with_trace_id(trace_id.unwrap_or_default()),

            DomainError::FileTooLarge { message } => Problem::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "file_too_large",
                message.clone(),
            )
            .with_code("file_too_large".to_owned())
            .with_trace_id(trace_id.unwrap_or_default()),

            DomainError::DocumentLimitExceeded { message } => Problem::new(
                StatusCode::BAD_REQUEST,
                "document_limit_exceeded",
                message.clone(),
            )
            .with_code("document_limit_exceeded".to_owned())
            .with_trace_id(trace_id.unwrap_or_default()),

            DomainError::StorageLimitExceeded { message } => Problem::new(
                StatusCode::BAD_REQUEST,
                "storage_limit_exceeded",
                message.clone(),
            )
            .with_code("storage_limit_exceeded".to_owned())
            .with_trace_id(trace_id.unwrap_or_default()),

            DomainError::ServiceUnavailable { message } => Problem::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                message.clone(),
            )
            .with_code("service_unavailable".to_owned())
            .with_trace_id(trace_id.unwrap_or_default()),

            DomainError::ProviderError {
                code,
                sanitized_message,
            } => {
                tracing::error!(code = %code, message = %sanitized_message, "provider error");
                Problem::new(StatusCode::BAD_GATEWAY, code, sanitized_message)
                    .with_code(code.clone())
                    .with_trace_id(trace_id.unwrap_or_default())
            }
        }
    }
}
