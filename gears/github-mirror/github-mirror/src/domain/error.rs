use toolkit_macros::domain_model;

#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("Repository not found")]
    NotFound,

    #[error("Validation error on field '{field}': {message}")]
    Validation { field: String, message: String },

    #[error("Access forbidden: {0}")]
    Forbidden(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Database error: {0}")]
    Database(#[from] toolkit_db::DbError),
}

impl DomainError {
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
}

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        tracing::error!(error = %e, "AuthZ scope resolution failed");
        match e {
            authz_resolver_sdk::EnforcerError::Denied { .. }
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => Self::Forbidden(e.to_string()),
            authz_resolver_sdk::EnforcerError::EvaluationFailed(_) => Self::Internal(e.to_string()),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn constructors_carry_their_messages() {
        assert_eq!(
            (DomainError::Validation {
                field: "title".to_owned(),
                message: "too long".to_owned()
            })
            .to_string(),
            "Validation error on field 'title': too long"
        );
        assert_eq!(
            DomainError::forbidden("nope").to_string(),
            "Access forbidden: nope"
        );
        assert_eq!(
            DomainError::internal("boom").to_string(),
            "Internal error: boom"
        );
        assert_eq!(DomainError::NotFound.to_string(), "Repository not found");
    }

    #[test]
    fn enforcer_denial_maps_to_forbidden() {
        let denied = authz_resolver_sdk::EnforcerError::Denied { deny_reason: None };
        assert!(matches!(
            DomainError::from(denied),
            DomainError::Forbidden(_)
        ));
    }

    #[test]
    fn enforcer_evaluation_failure_maps_to_internal() {
        let failed = authz_resolver_sdk::EnforcerError::EvaluationFailed(
            authz_resolver_sdk::AuthZResolverError::Internal("pdp unreachable".to_owned()),
        );
        assert!(matches!(
            DomainError::from(failed),
            DomainError::Internal(_)
        ));
    }
}
