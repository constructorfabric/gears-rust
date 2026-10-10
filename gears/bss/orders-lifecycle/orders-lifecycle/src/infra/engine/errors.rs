//! Engine failure taxonomy (Foundation §3.6 *Infrastructure-error termination*; DESIGN §4.1).
//!
//! Business refusals are not errors here: they commit as outcomes. [`Abort`] is every reason a
//! transition transaction must roll back completely; [`EngineError`] is what a caller sees when
//! no settled outcome exists: an unsettled refusal whose evidence did commit, a sanitized
//! infrastructure failure after confirmed rollback, or an *unknown* outcome after a lost commit
//! acknowledgement, which is never reported as a rollback.
use bss_orders_lifecycle_sdk::OrdersError;
use bss_orders_lifecycle_sdk::catalog::Reason;
use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;

use crate::domain::audit::AuditError;
use crate::domain::contributions::ContributionError;
use crate::domain::idempotency::RegistryRuleError;
use crate::domain::overlap::ClaimRuleError;
use crate::domain::transition::DecisionError;
use crate::infra::events::EventEnqueueError;
use crate::infra::storage::repo::audit::AuditStoreError;
use crate::infra::storage::repo::claims::ClaimStoreError;
use crate::infra::storage::repo::idempotency::RegistryError;

/// A named persistence boundary for failure injection (tests only arm it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// After the registry gate granted ownership.
    Gate,
    /// After step 17 overlap maintenance.
    Claims,
    /// After the step-18 version append.
    Version,
    /// After the slice's child-document contributions.
    Documents,
    /// After the aggregate write (steps 18.2-21).
    Aggregate,
    /// After the committed audit append (step 22) or a refusal's audit append.
    Audit,
    /// After the event enqueue (step 24).
    Enqueue,
    /// After settlement (step 25), immediately before commit.
    Settle,
}

/// Every cause that aborts the whole attempt. Nothing it carries is returned to a caller.
#[derive(Debug, thiserror::Error)]
pub enum Abort {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Store(#[from] ScopeError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    RegistryRule(#[from] RegistryRuleError),
    #[error(transparent)]
    Audit(#[from] AuditStoreError),
    #[error(transparent)]
    AuditRule(#[from] AuditError),
    #[error(transparent)]
    Claims(#[from] ClaimStoreError),
    #[error(transparent)]
    ClaimRule(#[from] ClaimRuleError),
    #[error(transparent)]
    Events(#[from] EventEnqueueError),
    #[error(transparent)]
    Decision(#[from] DecisionError),
    #[error(transparent)]
    Contribution(#[from] ContributionError),
    /// A slice/engine contract violation that fails closed (e.g. a missing D-198 contribution).
    #[error("engine integration: {0}")]
    Integration(&'static str),
    /// A required later-package capability is not delivered on this path.
    #[error("engine capability unavailable: {0}")]
    Unavailable(&'static str),
    #[error("injected fault at {0:?}")]
    Injected(FaultPoint),
}
impl Abort {
    /// The database error inside, for the toolkit's contention retry classifier.
    pub fn db_err(&self) -> Option<&sea_orm::DbErr> {
        match self {
            Self::Db(DbError::Sea(e))
            | Self::Store(ScopeError::Db(e))
            | Self::Registry(RegistryError::Store(ScopeError::Db(e)))
            | Self::Audit(AuditStoreError::Store(ScopeError::Db(e)))
            | Self::Claims(ClaimStoreError::Store(ScopeError::Db(e))) => Some(e),
            _ => None,
        }
    }
    /// Whether this abort is a failed producer enqueue (always a confirmed rollback).
    fn is_enqueue_failure(&self) -> bool {
        match self {
            Self::Events(_) => true,
            Self::Db(DbError::Other(e)) => e.downcast_ref::<EventEnqueueError>().is_some(),
            _ => false,
        }
    }
}

/// The caller-visible failure when no settled outcome is returned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// A refusal that settles no idempotency key (authorization denial, idempotency mismatch,
    /// still-processing, changed authorization facts, lost access). Its evidence committed.
    #[error("refused: {0:?}")]
    Refused(Reason),
    /// Retryable outage (PDP, read store, a temporary database outage) after full rollback.
    #[error("unavailable")]
    Unavailable,
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
            EngineError::Unavailable => Self::Unavailable,
            EngineError::Integration => Self::Integration,
            EngineError::Internal => Self::Internal,
            EngineError::OutcomeUnknown => Self::OutcomeUnknown,
        }
    }
}

/// Map an abort to the sanitized caller outcome. `body_completed` says every statement of the
/// attempt succeeded, so the failure arose at COMMIT: unless the server itself rejected the
/// commit (a definite rollback, e.g. a deferred constraint), the outcome is unknown.
pub fn terminate(abort: &Abort, body_completed: bool) -> EngineError {
    if body_completed && !abort.is_enqueue_failure() {
        let rejected_by_server = abort.db_err().is_some_and(commit_rejected_by_server);
        return if rejected_by_server {
            EngineError::Internal
        } else {
            EngineError::OutcomeUnknown
        };
    }
    match abort {
        Abort::Unavailable(_) => EngineError::Unavailable,
        Abort::Integration(_)
        | Abort::Decision(_)
        | Abort::Contribution(_)
        | Abort::ClaimRule(_)
        | Abort::Events(EventEnqueueError::Contract(_) | EventEnqueueError::NotLockedPostState) => {
            EngineError::Integration
        }
        other if other.db_err().is_some_and(is_temporary) => EngineError::Unavailable,
        _ => EngineError::Internal,
    }
}

/// Whether the server answered COMMIT with a definite error (the transaction rolled back, e.g.
/// a deferred constraint). Connection exceptions (SQLSTATE class 08), operator intervention
/// (57P0x: terminated session, shutdown) and transport/IO failures end the session without a
/// definite answer: the commit outcome is unknown.
fn commit_rejected_by_server(e: &sea_orm::DbErr) -> bool {
    use sea_orm::{DbErr, RuntimeErr, sqlx};
    let (DbErr::Exec(runtime) | DbErr::Query(runtime) | DbErr::Conn(runtime)) = e else {
        return false;
    };
    let RuntimeErr::SqlxError(error) = runtime else {
        return false;
    };
    let sqlx::Error::Database(db) = error.as_ref() else {
        return false;
    };
    db.code()
        .is_some_and(|code| !(code.starts_with("08") || code.starts_with("57P")))
}

/// A known temporary database/storage outage maps to `ServiceUnavailable` (503).
fn is_temporary(e: &sea_orm::DbErr) -> bool {
    matches!(
        e,
        sea_orm::DbErr::ConnectionAcquire(_) | sea_orm::DbErr::Conn(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aborts_map_to_sanitized_outcomes_and_a_completed_body_is_never_a_rollback_claim() {
        // Before COMMIT: unavailable, integration and internal, never a refusal.
        assert_eq!(
            terminate(&Abort::Unavailable("d-198"), false),
            EngineError::Unavailable
        );
        assert_eq!(
            terminate(&Abort::Integration("x"), false),
            EngineError::Integration
        );
        assert_eq!(
            terminate(&Abort::Decision(DecisionError::DraftRevisions), false),
            EngineError::Integration
        );
        assert_eq!(
            terminate(&Abort::Injected(FaultPoint::Audit), false),
            EngineError::Internal
        );
        let conn = Abort::Db(DbError::Sea(sea_orm::DbErr::Conn(
            sea_orm::RuntimeErr::Internal("down".into()),
        )));
        assert_eq!(terminate(&conn, false), EngineError::Unavailable);
        // After every statement succeeded, a failure without a definite server answer is an
        // unknown outcome; an enqueue failure is always a confirmed rollback.
        assert_eq!(terminate(&conn, true), EngineError::OutcomeUnknown);
        assert_eq!(
            terminate(&Abort::Injected(FaultPoint::Settle), true),
            EngineError::OutcomeUnknown
        );
        assert_eq!(
            terminate(&Abort::Events(EventEnqueueError::EnqueueFailed), true),
            EngineError::Internal
        );
    }
}
