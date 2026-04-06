//! REST error mapping for the Types Registry module.

use modkit::api::prelude::StatusCode;
use modkit::api::problem::Problem;

use crate::domain::error::DomainError;

impl From<DomainError> for Problem {
    fn from(e: DomainError) -> Self {
        let trace_id = tracing::Span::current()
            .id()
            .map(|id| id.into_u64().to_string());

        let (status, code, title, detail) = match &e {
            DomainError::InvalidGtsId(msg) => (
                StatusCode::BAD_REQUEST,
                "TYPES_REGISTRY_INVALID_GTS_ID",
                "Invalid GTS ID",
                msg.clone(),
            ),
            DomainError::NotFound(id) => (
                StatusCode::NOT_FOUND,
                "TYPES_REGISTRY_NOT_FOUND",
                "Entity not found",
                format!("No entity with GTS ID: {id}"),
            ),
            DomainError::AlreadyExists(id) => (
                StatusCode::CONFLICT,
                "TYPES_REGISTRY_ALREADY_EXISTS",
                "Entity already exists",
                format!("Entity with GTS ID already exists: {id}"),
            ),
            DomainError::ValidationFailed(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "TYPES_REGISTRY_VALIDATION_FAILED",
                "Validation failed",
                msg.clone(),
            ),
            DomainError::NotInReadyMode => (
                StatusCode::SERVICE_UNAVAILABLE,
                "TYPES_REGISTRY_NOT_READY",
                "Service not ready",
                "The types registry is not yet ready".to_owned(),
            ),
            DomainError::ReadyCommitFailed(errors) => {
                let error_strings: Vec<String> = errors
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect();
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "TYPES_REGISTRY_ACTIVATION_FAILED",
                    "Registry activation failed",
                    format!(
                        "Failed to activate registry: {} validation errors: {}",
                        errors.len(),
                        error_strings.join("; ")
                    ),
                )
            }
            DomainError::Internal(e) => {
                tracing::error!(error = ?e, "Internal error in types_registry");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "TYPES_REGISTRY_INTERNAL",
                    "Internal Server Error",
                    "An internal error occurred".to_owned(),
                )
            }
        };

        let mut problem = Problem::new(status, title, detail)
            .with_type(format!("https://errors.hyperspot.com/{code}"))
            .with_code(code);

        if let Some(id) = trace_id {
            problem = problem.with_trace_id(id);
        }

        problem
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
