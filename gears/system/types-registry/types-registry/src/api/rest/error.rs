//! REST error mapping for the Types Registry gear.

use toolkit_canonical_errors::{CanonicalError, resource_error};
use types_registry_sdk::{field, precondition};

use crate::domain::error::DomainError;

#[resource_error("gts.cf.types_registry.registry.type.v1~")]
pub struct TypeRegistryError;

impl From<DomainError> for CanonicalError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::InvalidGtsId(msg) => TypeRegistryError::invalid_argument()
                .with_field_violation(field::GTS_ID_FIELD, msg, field::INVALID_GTS_ID)
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
            // Adapter-only (in-process) batch-register disposition — never
            // reaches a REST handler. Carries `parent_type_id` / `dependent_id`
            // losslessly so the SDK projects it back to
            // `TypesRegistryError::ParentNotRegistered`: dependent → resource,
            // parent → violation subject, message → violation description.
            DomainError::ParentTypeSchemaNotRegistered {
                parent_type_id,
                dependent_id,
            } => {
                let detail = format!(
                    "Cannot register {dependent_id}: required type-schema {parent_type_id} is not registered"
                );
                TypeRegistryError::failed_precondition()
                    .with_resource(dependent_id)
                    .with_precondition_violation(
                        parent_type_id,
                        detail,
                        precondition::PARENT_NOT_REGISTERED,
                    )
                    .create()
            }
            DomainError::InvalidQuery(msg) => TypeRegistryError::invalid_argument()
                .with_field_violation(field::QUERY_FIELD, msg, field::INVALID_QUERY)
                .create(),
            DomainError::ValidationFailed(msg) => TypeRegistryError::invalid_argument()
                .with_field_violation(field::ENTITY_FIELD, msg, field::VALIDATION_FAILED)
                .create(),
            DomainError::NotInReadyMode => CanonicalError::service_unavailable().create(),
            DomainError::ReadyCommitFailed(errors) => {
                // Unreachable from REST handlers — `switch_to_ready` runs in
                // gear `post_init` only. Kept for `From` exhaustiveness.
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

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_canonical_errors::Problem;

    fn problem_from(err: DomainError) -> Problem {
        // Construct the wire `Problem` the same way the canonical error
        // middleware does — minus the post-response `instance` / `trace_id`
        // injection, which has no request context at the unit-test level.
        Problem::from(CanonicalError::from(err))
    }

    #[test]
    fn test_domain_error_to_problem_not_found_by_id() {
        let problem = problem_from(DomainError::not_found_by_id("gts.cf.core.events.test.v1~"));
        assert_eq!(problem.status, 404);
        // `instance` is filled by the canonical error middleware on the way
        // out — at the unit-test level no middleware is in scope.
        assert!(problem.instance.is_none());
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
        let problem = problem_from(DomainError::not_found_by_uuid(uuid::Uuid::nil()));
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
        let problem = problem_from(DomainError::already_exists("gts.cf.core.events.test.v1~"));
        assert_eq!(problem.status, 409);
    }

    #[test]
    fn test_domain_error_to_problem_invalid_gts_id() {
        let problem = problem_from(DomainError::invalid_gts_id("bad format"));
        assert_eq!(problem.status, 400);
    }

    #[test]
    fn test_domain_error_to_problem_validation_failed() {
        let problem = problem_from(DomainError::validation_failed("schema invalid"));
        assert_eq!(problem.status, 400);
    }

    #[test]
    fn test_domain_error_to_problem_not_in_ready_mode() {
        let problem = problem_from(DomainError::NotInReadyMode);
        assert_eq!(problem.status, 503);
    }

    #[test]
    fn test_domain_error_to_problem_ready_commit_failed() {
        use crate::domain::error::ValidationError;
        let problem = problem_from(DomainError::ReadyCommitFailed(vec![
            ValidationError::new("gts.test1~", "error1"),
            ValidationError::new("gts.test2~", "error2"),
            ValidationError::new("gts.test3~", "error3"),
        ]));
        // ReadyCommitFailed is only produced by post_init lifecycle and
        // never reaches a REST response; map opaquely to internal.
        assert_eq!(problem.status, 500);
    }

    #[test]
    fn test_domain_error_to_problem_internal() {
        let problem = problem_from(DomainError::Internal(anyhow::anyhow!("test error")));
        assert_eq!(problem.status, 500);
    }

    #[test]
    fn test_domain_error_to_problem_invalid_query() {
        let problem = problem_from(DomainError::invalid_query("bad pattern"));
        assert_eq!(problem.status, 400);
    }
}
