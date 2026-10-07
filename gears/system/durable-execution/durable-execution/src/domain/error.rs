//! Domain failures of durable execution. Mapped to `CanonicalError` once, at
//! the SDK boundary (`infra::error`); variant-specific control flow
//! (lease loss, scope fallback) dispatches on these variants.
use durable_execution_sdk::{DefinitionError, RunId};
use toolkit_macros::domain_model;

#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    #[error("durable execution access denied")]
    Forbidden,
    /// Authorization failed for the shared definition catalog.
    #[error("definition management access denied")]
    DefinitionForbidden,
    #[error("durable run not found")]
    RunNotFound(RunId),
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
    #[error("workflow definition is inactive")]
    DefinitionInactive,
    /// The named idempotency or coalescing key was reused with different input.
    #[error("request key reused with different input")]
    IdempotencyConflict(&'static str),
    #[error(transparent)]
    InvalidDefinition(#[from] DefinitionError),
    #[error("invalid {field}: {message}")]
    InvalidRequest {
        field: &'static str,
        reason: &'static str,
        message: &'static str,
    },
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
