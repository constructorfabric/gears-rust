//! REST error mapping for the Types Registry module.

use modkit::api::canonical_prelude::CanonicalProblemMigrationExt;
use modkit_canonical_errors::{CanonicalError, Problem, resource_error};

use crate::domain::error::DomainError;

#[resource_error("gts.cf.types_registry.registry.type.v1~")]
pub struct TypeRegistryError;

impl From<DomainError> for CanonicalError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::InvalidGtsId(msg) => TypeRegistryError::invalid_argument()
                .with_field_violation("gts_id", msg, "INVALID_GTS_ID")
                .create(),
            DomainError::NotFound { kind, target } => {
                TypeRegistryError::not_found(format!("No entity with {kind}: {target}"))
                    .with_resource(target)
                    .create()
            }
            DomainError::AlreadyExists(id) => TypeRegistryError::already_exists(format!(
                "Entity with GTS ID already exists: {id}"
            ))
            .with_resource(id)
            .create(),
            DomainError::InvalidQuery(msg) => TypeRegistryError::invalid_argument()
                .with_field_violation("query", msg, "INVALID_QUERY")
                .create(),
            DomainError::ValidationFailed(msg) => TypeRegistryError::invalid_argument()
                .with_field_violation("entity", msg, "VALIDATION_FAILED")
                .create(),
            DomainError::NotInReadyMode => CanonicalError::service_unavailable().create(),
            DomainError::ReadyCommitFailed(errors) => {
                // Unreachable from REST handlers — `switch_to_ready` runs in
                // module `post_init` only. Kept for `From` exhaustiveness.
                // If it ever surfaces, we want an opaque internal response;
                // the validation detail is logged server-side and preserved
                // on the canonical error's diagnostic field.
                for ve in &errors {
                    tracing::error!(
                        gts_id = %ve.gts_id,
                        message = %ve.message,
                        "types_registry ready commit validation failure"
                    );
                }
                let summary = format!("ready commit failed with {} errors", errors.len());
                CanonicalError::internal(summary).create()
            }
            DomainError::Internal(e) => {
                tracing::error!(error = ?e, "Internal error in types_registry");
                CanonicalError::internal(e.to_string()).create()
            }
        }
    }
}

// TODO(cpt-cf-errors-component-error-middleware): drop this impl once
// middleware injects trace_id/instance from request context. The
// `From<DomainError> for CanonicalError` impl above is the long-lived
// mapping; this wrapper exists only to keep handler signatures returning
// `Problem` until middleware lands.
impl From<DomainError> for Problem {
    fn from(e: DomainError) -> Self {
        Problem::from(CanonicalError::from(e)).with_temporary_request_context("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_error_to_problem_not_found_by_id() {
        let err = DomainError::not_found_by_id("gts.cf.core.events.test.v1~");
        let problem: Problem = err.into();
        assert_eq!(problem.status, 404);
        assert_eq!(problem.instance.as_deref(), Some("/"));
        assert!(
            problem
                .detail
                .contains("GTS ID: gts.cf.core.events.test.v1~"),
            "expected GTS-id-keyed detail, got {:?}",
            problem.detail,
        );
    }

    #[test]
    fn test_domain_error_to_problem_not_found_by_uuid() {
        let err = DomainError::not_found_by_uuid(uuid::Uuid::nil());
        let problem: Problem = err.into();
        assert_eq!(problem.status, 404);
        assert!(
            problem
                .detail
                .contains("UUID: 00000000-0000-0000-0000-000000000000"),
            "expected UUID-keyed detail, got {:?}",
            problem.detail,
        );
    }

    #[test]
    fn test_domain_error_to_problem_already_exists() {
        let err = DomainError::already_exists("gts.cf.core.events.test.v1~");
        let problem: Problem = err.into();
        assert_eq!(problem.status, 409);
    }

    #[test]
    fn test_domain_error_to_problem_invalid_gts_id() {
        let err = DomainError::invalid_gts_id("bad format");
        let problem: Problem = err.into();
        assert_eq!(problem.status, 400);
    }

    #[test]
    fn test_domain_error_to_problem_validation_failed() {
        let err = DomainError::validation_failed("schema invalid");
        let problem: Problem = err.into();
        assert_eq!(problem.status, 400);
    }

    #[test]
    fn test_domain_error_to_problem_not_in_ready_mode() {
        let err = DomainError::NotInReadyMode;
        let problem: Problem = err.into();
        assert_eq!(problem.status, 503);
    }

    #[test]
    fn test_domain_error_to_problem_ready_commit_failed() {
        use crate::domain::error::ValidationError;
        let err = DomainError::ReadyCommitFailed(vec![
            ValidationError::new("gts.test1~", "error1"),
            ValidationError::new("gts.test2~", "error2"),
            ValidationError::new("gts.test3~", "error3"),
        ]);
        let problem: Problem = err.into();
        // ReadyCommitFailed is only produced by post_init lifecycle and
        // never reaches a REST response; map opaquely to internal.
        assert_eq!(problem.status, 500);
    }

    #[test]
    fn test_domain_error_to_problem_internal() {
        let err = DomainError::Internal(anyhow::anyhow!("test error"));
        let problem: Problem = err.into();
        assert_eq!(problem.status, 500);
    }

    #[test]
    fn test_domain_error_to_problem_invalid_query() {
        let err = DomainError::invalid_query("bad pattern");
        let problem: Problem = err.into();
        assert_eq!(problem.status, 400);
    }
}
