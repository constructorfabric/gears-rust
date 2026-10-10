//! The single `DomainError` → `CanonicalError` mapping for the SDK boundary
//! (ADR-0005). Reason strings come from `durable_execution_sdk::reason`.
use crate::domain::error::DomainError;
use durable_execution_sdk::reason;
use toolkit_canonical_errors::{CanonicalError, resource_error};

// Literals MUST equal `durable_execution_sdk::gts::{RUN,DEFINITION}_RESOURCE_TYPE`.
#[resource_error(gts_id!("cf.durable_execution.execution.run.v1~"))]
struct RunResource;
#[resource_error(gts_id!("cf.durable_execution.execution.definition.v1~"))]
struct DefinitionResource;

impl From<DomainError> for CanonicalError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::Forbidden => RunResource::permission_denied()
                .with_reason(reason::EXECUTION_ACCESS_DENIED)
                .create(),
            DomainError::DefinitionForbidden => DefinitionResource::permission_denied()
                .with_reason(reason::EXECUTION_ACCESS_DENIED)
                .create(),
            DomainError::RunNotFound(id) => RunResource::not_found("durable run not found")
                .with_resource(id.0.to_string())
                .create(),
            DomainError::DefinitionNotFound(name) => {
                DefinitionResource::not_found("execution definition not registered")
                    .with_resource(name)
                    .create()
            }
            DomainError::LeaseLost => RunResource::aborted("activity lease lost")
                .with_reason(reason::LEASE_LOST)
                .create(),
            DomainError::ConcurrentUpdate => RunResource::aborted("concurrent update")
                .with_reason(reason::CONCURRENT_UPDATE)
                .create(),
            DomainError::DefinitionConcurrentUpdate => {
                DefinitionResource::aborted("concurrent update")
                    .with_reason(reason::CONCURRENT_UPDATE)
                    .create()
            }
            DomainError::DefinitionInvalidState(description) => {
                DefinitionResource::failed_precondition()
                    .with_precondition_violation("state", description, reason::INVALID_STATE)
                    .create()
            }
            DomainError::InvalidState(description) => RunResource::failed_precondition()
                .with_precondition_violation("state", description, reason::INVALID_STATE)
                .create(),
            DomainError::DefinitionInactive => DefinitionResource::failed_precondition()
                .with_precondition_violation(
                    "registration",
                    "workflow definition is inactive",
                    reason::DEFINITION_INACTIVE,
                )
                .create(),
            DomainError::DefinitionMismatch => DefinitionResource::failed_precondition()
                .with_precondition_violation(
                    "fingerprint",
                    "saved definition does not match registered version",
                    reason::DEFINITION_MISMATCH,
                )
                .create(),
            DomainError::DefinitionConflict(name) => DefinitionResource::already_exists(
                "execution definition already exists with different content",
            )
            .with_resource(name)
            .create(),
            DomainError::IdempotencyConflict(key) => {
                RunResource::already_exists("request key reused with different input")
                    .with_resource(key)
                    .create()
            }
            DomainError::InvalidDefinition(e) => e.into(),
            DomainError::InvalidRequest {
                field,
                reason,
                message,
            } => RunResource::invalid_argument()
                .with_field_violation(field, message, reason)
                .create(),
            DomainError::Unavailable => CanonicalError::service_unavailable()
                .with_detail("durable execution is unavailable")
                .create(),
            DomainError::Internal(what) => {
                tracing::error!(invariant = what, "durable execution internal error");
                CanonicalError::internal(what).create()
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/error_tests.rs"]
mod tests;
