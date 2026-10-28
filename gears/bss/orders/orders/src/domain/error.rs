//! The engine's caller-visible failure (Foundation §3.6 *Infrastructure-error termination*;
//! DESIGN §4.1) and its one projection into the SDK [`OrdersError`] (TOOLKIT-ERR-002; the
//! ledger gear's `domain/error.rs` precedent).
//!
//! Business refusals are not errors: they commit as outcomes. [`EngineError`] is what a caller
//! sees when no settled outcome exists: an unsettled refusal whose evidence did commit, a
//! sanitized infrastructure failure after confirmed rollback, or an *unknown* outcome after a
//! lost commit acknowledgement, which is never reported as a rollback. The infra-only abort
//! causes stay in `infra::engine::errors`.
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::catalog::Reason;
use toolkit_macros::domain_model;

/// The caller-visible failure when no settled outcome is returned.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// A refusal that settles no idempotency key (authorization denial, idempotency mismatch,
    /// still-processing, changed authorization facts, lost access). Its evidence committed.
    #[error("refused: {0:?}")]
    Refused(Reason),
    /// Retryable outage (PDP, read store, a temporary database outage) after full rollback.
    #[error("unavailable")]
    Unavailable,
    /// A capability a later delivery package owns is not delivered on this path; nothing took
    /// effect. Internal only: it is logged on its own line and still answers
    /// [`OrdersError::Unavailable`], so the wire is unchanged.
    #[error("capability not delivered: {0}")]
    NotDelivered(&'static str),
    /// Fail-closed integration or contract defect after full rollback.
    #[error("integration failure")]
    Integration,
    /// Unexpected persistence failure after confirmed rollback.
    #[error("internal failure")]
    Internal,
    /// Lost commit acknowledgement: resolve by retrying the same key. Never a rollback claim.
    #[error("outcome unknown")]
    OutcomeUnknown,
}

impl From<EngineError> for OrdersError {
    fn from(value: EngineError) -> Self {
        match value {
            EngineError::Refused(reason) => Self::Refused(reason),
            EngineError::Unavailable | EngineError::NotDelivered(_) => Self::Unavailable,
            EngineError::Integration => Self::Integration,
            EngineError::Internal => Self::Internal,
            EngineError::OutcomeUnknown => Self::OutcomeUnknown,
        }
    }
}
