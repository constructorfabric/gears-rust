//! Domain failures of durable execution. Wrapped into the public `Error` at the
//! client boundary; variant-specific control flow (lease loss, scope fallback)
//! dispatches on these variants.
use crate::{DefinitionError, RunId};

/// Domain failure of a durable execution operation.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    /// Authorization failed for a durable run, or the caller has no subject or tenant.
    #[error("durable execution access denied")]
    Forbidden,
    /// Authorization failed for the shared definition catalog.
    #[error("definition management access denied")]
    DefinitionForbidden,
    /// No run with this id is visible to the caller.
    #[error("durable run not found")]
    RunNotFound(RunId),
    /// No execution definition is registered under this name.
    #[error("execution definition not registered")]
    DefinitionNotFound(String),
    /// The caller's claim or lease is no longer the current owner.
    #[error("activity lease lost")]
    LeaseLost,
    /// An epoch, revision or row version changed under the caller.
    #[error("concurrent update")]
    ConcurrentUpdate,
    /// The definition catalog revision changed under the caller.
    #[error("concurrent definition update")]
    DefinitionConcurrentUpdate,
    /// The current run or registration state forbids the transition.
    #[error("invalid state: {0}")]
    InvalidState(&'static str),
    /// The registration state forbids this catalog transition.
    #[error("invalid definition state: {0}")]
    DefinitionInvalidState(&'static str),
    /// A different contract, or a duplicate handler binding, under the same name.
    #[error("execution definition already exists with different content")]
    DefinitionConflict(String),
    /// The run is pinned to a contract that differs from the registered version.
    #[error("saved definition does not match registered version")]
    DefinitionMismatch,
    /// The definition is inactive or revoked for the run's registration generation.
    #[error("workflow definition is inactive")]
    DefinitionInactive,
    /// This engine has no delivery, so it cannot schedule work.
    #[error("this engine does not deliver work; use a Dispatcher or Worker mode")]
    DeliveryDisabled,
    /// The named idempotency or coalescing key was reused with different input.
    #[error("request key reused with different input")]
    IdempotencyConflict(&'static str),
    /// The execution definition failed SDK validation.
    #[error(transparent)]
    InvalidDefinition(#[from] DefinitionError),
    /// A request field failed validation.
    #[error("invalid {field}: {message}")]
    InvalidRequest {
        /// Name of the offending request field.
        field: &'static str,
        /// Machine-readable validation reason code, such as `codes::INVALID_FORMAT`.
        reason: &'static str,
        /// Human-readable description of the violation.
        message: &'static str,
    },
    /// A dependency, such as the authorization policy, is temporarily unavailable.
    #[error("durable execution is unavailable")]
    Unavailable,
    /// Broken invariant: overflow, corrupt persisted state or a missing journal.
    #[error("internal error: {0}")]
    Internal(&'static str),
}

impl DomainError {
    /// Preserve the resource of failures at a definition-only operation boundary.
    pub(crate) fn for_definition(self) -> Self {
        match self {
            Self::Forbidden => Self::DefinitionForbidden,
            Self::ConcurrentUpdate => Self::DefinitionConcurrentUpdate,
            Self::InvalidState(description) => Self::DefinitionInvalidState(description),
            other => other,
        }
    }
}
