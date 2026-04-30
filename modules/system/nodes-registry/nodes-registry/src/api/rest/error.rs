use crate::domain::error::DomainError;
use modkit::api::canonical_prelude::CanonicalProblemMigrationExt;
use modkit_canonical_errors::{CanonicalError, Problem, resource_error};

#[resource_error("gts.cf.nodes_registry.registry.node.v1~")]
pub struct NodeError;

impl From<DomainError> for CanonicalError {
    // Flat match on the domain enum is the whole point of this conversion;
    // the structured `tracing::*!` macros count toward cognitive complexity
    // but splitting the arms into helpers would just hide the mapping.
    #[allow(clippy::cognitive_complexity)]
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::NodeNotFound(id) => NodeError::not_found(format!("No node with id {id}"))
                .with_resource(id.to_string())
                .create(),
            DomainError::SysInfoCollectionFailed(msg) => {
                tracing::error!(
                    kind = "sysinfo_collection_failed",
                    error = %msg,
                    "nodes-registry sysinfo collection failed"
                );
                CanonicalError::internal(msg).create()
            }
            DomainError::SysCapCollectionFailed(msg) => {
                tracing::error!(
                    kind = "syscap_collection_failed",
                    error = %msg,
                    "nodes-registry syscap collection failed"
                );
                CanonicalError::internal(msg).create()
            }
            DomainError::Internal(msg) => {
                tracing::error!(
                    kind = "internal_error",
                    error = %msg,
                    "nodes-registry internal error"
                );
                CanonicalError::internal(msg).create()
            }
            DomainError::InvalidInput(msg) => NodeError::invalid_argument()
                .with_field_violation("input", msg, "VALIDATION_ERROR")
                .create(),
        }
    }
}

// TODO(cpt-cf-errors-component-error-middleware): drop this impl once
// middleware injects trace_id/instance from request context. The
// `From<DomainError> for CanonicalError` impl above is the long-lived
// mapping; this wrapper exists only to keep handler signatures returning
// `Problem` until middleware lands.
impl From<DomainError> for Problem {
    fn from(err: DomainError) -> Self {
        Problem::from(CanonicalError::from(err)).with_temporary_request_context("/")
    }
}
