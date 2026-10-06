use toolkit_db::DbError;
use toolkit_macros::domain_model;

#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("Note not found")]
    NotFound,

    #[error("Validation error on field '{field}': {message}")]
    Validation { field: String, message: String },

    #[error("Access forbidden: {0}")]
    Forbidden(String),

    #[error("Service unavailable: {0}")]
    Unavailable(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Database error: {0}")]
    Database(#[from] DbError),
}

impl DomainError {
    pub fn validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Validation {
            field: field.into(),
            message: message.into(),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
}

fn log_enforcer_error(e: &authz_resolver_sdk::EnforcerError) {
    use authz_resolver_sdk::EnforcerError as E;
    match e {
        E::Denied { .. } => log_denied(e),
        E::CompileFailed(_) => log_compile_failed(e),
        E::EvaluationFailed(_) => log_evaluation_failed(e),
    }
}

fn log_denied(e: &authz_resolver_sdk::EnforcerError) {
    tracing::debug!(error = %e, "AuthZ scope resolution denied");
}

fn log_compile_failed(e: &authz_resolver_sdk::EnforcerError) {
    tracing::warn!(error = %e, "AuthZ scope compilation failed");
}

fn log_evaluation_failed(e: &authz_resolver_sdk::EnforcerError) {
    tracing::error!(error = %e, "AuthZ scope resolution failed");
}

/// @cpt-dod:cpt-cf-construct-dod-gear-foundation-error-mapping:p1
// TODO(DE1302): `Forbidden` and `Unavailable` only carry Strings, so the
// `EnforcerError` source is lost. Extend the variants to hold a boxed source,
// then remove this allow.
#[allow(unknown_lints, reason = "dylint lint names are unknown to rustc")]
#[allow(
    de1302_error_from_to_string,
    reason = "the source error is flattened to a String until the variants carry a boxed source"
)]
impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        log_enforcer_error(&e);
        match e {
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-denied
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-denied-return
            authz_resolver_sdk::EnforcerError::Denied { .. }
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => Self::Forbidden(e.to_string()),
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-denied-return
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-denied
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-failed
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-failed-return
            authz_resolver_sdk::EnforcerError::EvaluationFailed(_) => {
                Self::Unavailable(e.to_string())
            }
        }
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-failed-return
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-failed
    }
}
