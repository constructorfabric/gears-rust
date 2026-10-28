//! Engine failure taxonomy (Foundation §3.6 *Infrastructure-error termination*; DESIGN §4.1).
//!
//! Business refusals are not errors here: they commit as outcomes. [`Abort`] is every reason a
//! transition transaction must roll back completely; [`terminate`] maps it to the caller-visible
//! [`EngineError`], which lives in [`crate::domain::error`] and is re-exported here.
use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;

pub use crate::domain::error::EngineError;

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
    /// A database failure.
    #[error(transparent)]
    Db(#[from] DbError),
    /// A scoped store operation failed or was denied.
    #[error(transparent)]
    Store(#[from] ScopeError),
    /// The idempotency registry store failed.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// An idempotency registry rule was violated.
    #[error(transparent)]
    RegistryRule(#[from] RegistryRuleError),
    /// The audit append failed.
    #[error(transparent)]
    Audit(#[from] AuditStoreError),
    /// An audit entry failed encoding, shape or minimization.
    #[error(transparent)]
    AuditRule(#[from] AuditError),
    /// The overlap claim store failed.
    #[error(transparent)]
    Claims(#[from] ClaimStoreError),
    /// A step-17 claim rule was violated.
    #[error(transparent)]
    ClaimRule(#[from] ClaimRuleError),
    /// The row's event could not be enqueued.
    #[error(transparent)]
    Events(#[from] EventEnqueueError),
    /// A request/registry contract violation in the transition decision.
    #[error(transparent)]
    Decision(#[from] DecisionError),
    /// A row and its contribution disagree.
    #[error(transparent)]
    Contribution(#[from] ContributionError),
    /// A slice/engine contract violation that fails closed (e.g. a missing D-198 contribution).
    #[error("engine integration: {0}")]
    Integration(&'static str),
    /// A required later-package capability is not delivered on this path.
    #[error("engine capability not delivered: {0}")]
    NotDelivered(&'static str),
    /// A failure injected at the named persistence boundary (tests only).
    #[error("injected fault at {0:?}")]
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "only the cfg(test) fault hook injects a failure")
    )]
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
        Abort::NotDelivered(capability) => EngineError::NotDelivered(capability),
        Abort::Integration(_)
        | Abort::Decision(_)
        | Abort::Contribution(_)
        | Abort::ClaimRule(_)
        | Abort::Events(EventEnqueueError::Contract(_) | EventEnqueueError::NotLockedPostState) => {
            EngineError::Integration
        }
        Abort::Db(_)
        | Abort::Store(_)
        | Abort::Registry(_)
        | Abort::Audit(_)
        | Abort::Claims(_) => {
            if abort_db_err(abort).is_some_and(is_retryable_before_commit) {
                EngineError::Unavailable
            } else {
                EngineError::Internal
            }
        }
        Abort::RegistryRule(_) | Abort::AuditRule(_) | Abort::Events(_) | Abort::Injected(_) => {
            EngineError::Internal
        }
    }
}

/// Log an undelivered capability on its own line and answer [`EngineError::NotDelivered`]: a
/// permanent "not yet" of a later delivery package, distinguishable in logs from a real outage
/// although it still answers `Unavailable` (503) on the wire.
pub fn not_delivered(capability: &'static str) -> EngineError {
    log_not_delivered(capability);
    EngineError::NotDelivered(capability)
}

/// The one log line of an undelivered capability (see [`not_delivered`]).
pub fn log_not_delivered(capability: &'static str) {
    tracing::info!(
        target: "orders.not_delivered",
        capability,
        "capability is not delivered on this path; answered unavailable before any effect"
    );
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

/// The database failure inside an abort: the typed store wrappings of [`Abort::db_err`], or a
/// database cause carried through `DbError::Other` ([`store_db_err`]).
fn abort_db_err(abort: &Abort) -> Option<&sea_orm::DbErr> {
    abort.db_err().or_else(|| match abort {
        Abort::Db(DbError::Other(error)) => store_db_err(error),
        _ => None,
    })
}

/// A statement of a transition attempt failed before COMMIT, so the whole transaction rolled
/// back and the same request may succeed when sent again: a [temporary](is_temporary) outage,
/// or a [bound wait](is_wait_bound_exceeded) the deployment's `lock_timeout` or
/// `statement_timeout` ended (D-222).
fn is_retryable_before_commit(e: &sea_orm::DbErr) -> bool {
    is_temporary(e) || is_wait_bound_exceeded(e)
}

/// SQLSTATE 55P03 (`lock_not_available`: the aggregate row lock or another lock was not granted
/// within `lock_timeout`) or 57014 (`query_canceled`: `statement_timeout` ended the statement).
/// Either ends one statement on a live session: nothing was written and the same statements may
/// succeed if sent again, the graph-storage gear's `55P03`/`57014` classification (D-222).
fn is_wait_bound_exceeded(e: &sea_orm::DbErr) -> bool {
    has_sqlstate(e, "55P03") || has_sqlstate(e, "57014")
}

/// Whether a store failure outside the transition transaction (the 08 §3.6 point prefetch) is a
/// known temporary outage: a lost or unobtainable connection, transport I/O, or a serialization
/// or deadlock conflict. Anything else (a missing grant, schema drift, a denied scope) is
/// permanent and must not answer a retryable 503 forever.
pub fn is_temporary_store_failure(error: &anyhow::Error) -> bool {
    store_db_err(error).is_some_and(is_temporary_read)
}

/// Whether a store failure is the loss of the database session itself, the case a maintenance
/// pass reports as a coordination failure (its advisory lock went with the session): the
/// [`is_connection_loss`] classification of the failure's database cause, never its text.
pub fn is_connection_loss_failure(error: &anyhow::Error) -> bool {
    store_db_err(error).is_some_and(is_connection_loss)
}

/// Whether a store failure is SQLSTATE 42501 (`insufficient_privilege`): the session role lacks
/// the right the statement needs. Decided by SQLSTATE, never by the (localized) message text.
pub fn is_insufficient_privilege(error: &anyhow::Error) -> bool {
    store_db_err(error).is_some_and(|e| has_sqlstate(e, "42501"))
}

/// Whether the server answered the statement with SQLSTATE `code`.
fn has_sqlstate(e: &sea_orm::DbErr, code: &str) -> bool {
    use sea_orm::{DbErr, RuntimeErr, sqlx};
    let (DbErr::Exec(runtime) | DbErr::Query(runtime)) = e else {
        return false;
    };
    let RuntimeErr::SqlxError(error) = runtime else {
        return false;
    };
    match error.as_ref() {
        sqlx::Error::Database(db) => db.code().is_some_and(|c| c == code),
        _ => false,
    }
}

/// The database failure behind a store error, however the adapters wrapped it (`DbErr`, a
/// toolkit `DbError::Sea`, or a secure-scope `ScopeError::Db`).
fn store_db_err(error: &anyhow::Error) -> Option<&sea_orm::DbErr> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<sea_orm::DbErr>()
            .or_else(|| match cause.downcast_ref::<DbError>() {
                Some(DbError::Sea(e)) => Some(e),
                _ => None,
            })
            .or_else(|| match cause.downcast_ref::<ScopeError>() {
                Some(ScopeError::Db(e)) => Some(e),
                _ => None,
            })
    })
}

/// The database session is gone or could not be obtained, so no statement on it has an answer:
/// a pool or connection failure, transport I/O, SQLSTATE class 08 (connection exception) or
/// 57P0x (operator intervention: terminated session, shutdown). Decided from the error's
/// variants and SQLSTATE, never from its (driver- and locale-dependent) message text.
pub fn is_connection_loss(e: &sea_orm::DbErr) -> bool {
    use sea_orm::{DbErr, RuntimeErr, sqlx};
    if is_temporary(e) {
        return true;
    }
    let (DbErr::Exec(runtime) | DbErr::Query(runtime)) = e else {
        return false;
    };
    let RuntimeErr::SqlxError(error) = runtime else {
        return false;
    };
    match error.as_ref() {
        sqlx::Error::Io(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => true,
        sqlx::Error::Database(db) => db
            .code()
            .is_some_and(|code| code.starts_with("08") || code.starts_with("57P")),
        _ => false,
    }
}

/// [`is_temporary`] widened for a read-only statement, which has no commit outcome to lose: a
/// [connection loss](is_connection_loss), a [bound wait](is_wait_bound_exceeded), or SQLSTATE
/// 40001 (serialization failure) or 40P01 (deadlock) on a live session.
fn is_temporary_read(e: &sea_orm::DbErr) -> bool {
    use sea_orm::{DbErr, RuntimeErr, sqlx};
    if is_connection_loss(e) || is_wait_bound_exceeded(e) {
        return true;
    }
    let (DbErr::Exec(runtime) | DbErr::Query(runtime)) = e else {
        return false;
    };
    let RuntimeErr::SqlxError(error) = runtime else {
        return false;
    };
    match error.as_ref() {
        sqlx::Error::Database(db) => db
            .code()
            .is_some_and(|code| code == "40001" || code == "40P01"),
        _ => false,
    }
}

/// Log a prefetch store failure (as the read path's `store_unavailable` does) and classify it:
/// a temporary outage is `Unavailable`, a permanent one an unexpected persistence failure.
pub fn prefetch_failure(trigger: &str, error: &anyhow::Error) -> EngineError {
    let outcome = if is_temporary_store_failure(error) {
        EngineError::Unavailable
    } else {
        EngineError::Internal
    };
    tracing::warn!(
        target: "orders.engine.store",
        trigger,
        outcome = ?outcome,
        error = %error,
        "authorization prefetch failed"
    );
    outcome
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use bss_orders_sdk::OrdersError;

    /// A server error carrying `code` as its SQLSTATE and `message` as its (possibly localized)
    /// text, as the graph-storage gear's `DriverError` builds one.
    #[derive(Debug)]
    struct DriverError {
        code: &'static str,
        message: String,
    }
    impl std::fmt::Display for DriverError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.message)
        }
    }
    impl std::error::Error for DriverError {}
    impl sea_orm::sqlx::error::DatabaseError for DriverError {
        fn message(&self) -> &str {
            &self.message
        }
        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            Some(std::borrow::Cow::Borrowed(self.code))
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sea_orm::sqlx::error::ErrorKind {
            sea_orm::sqlx::error::ErrorKind::Other
        }
    }

    /// A statement failure the server answered with `code` and `message`.
    pub fn driver_error(code: &'static str, message: &str) -> sea_orm::DbErr {
        sea_orm::DbErr::Query(sea_orm::RuntimeErr::SqlxError(std::sync::Arc::new(
            sea_orm::sqlx::Error::Database(Box::new(DriverError {
                code,
                message: message.to_owned(),
            })),
        )))
    }

    #[test]
    fn connection_loss_is_decided_by_variant_and_sqlstate_never_by_message_text() {
        use sea_orm::{DbErr, RuntimeErr, sqlx};
        // sqlx renders a dropped socket as "error communicating with database: Connection reset
        // by peer": no lowercase "connection", which a text match missed.
        let reset = DbErr::Query(RuntimeErr::SqlxError(std::sync::Arc::new(sqlx::Error::Io(
            std::io::Error::from(std::io::ErrorKind::ConnectionReset),
        ))));
        assert!(is_connection_loss(&reset));
        assert!(is_connection_loss(&DbErr::Conn(RuntimeErr::Internal(
            "down".into()
        ))));
        assert!(is_connection_loss(&driver_error(
            "57P01",
            "terminating connection due to administrator command"
        )));
        // A localized server message classifies by its SQLSTATE alone.
        assert!(is_connection_loss(&driver_error(
            "08006",
            "Verbindung zum Server verloren"
        )));
        // A conflict on a live session is a temporary read failure, not a lost session.
        let conflict = driver_error("40001", "could not serialize access");
        assert!(!is_connection_loss(&conflict));
        assert!(is_temporary_read(&conflict));
        // A permanent failure is never a loss, whatever its text says about connections.
        let denied = driver_error("42501", "permission denied: connection closed for role");
        assert!(!is_connection_loss(&denied));
        assert!(!is_temporary_read(&denied));
        // The wrapped forms the stores return classify the same way.
        assert!(is_connection_loss_failure(&anyhow::Error::from(
            ScopeError::Db(reset)
        )));
        assert!(!is_connection_loss_failure(&anyhow::Error::from(
            DbError::Sea(conflict)
        )));
        assert!(!is_connection_loss_failure(&anyhow::anyhow!(
            "connection reset"
        )));
    }

    #[test]
    fn aborts_map_to_sanitized_outcomes_and_a_completed_body_is_never_a_rollback_claim() {
        // Before COMMIT: unavailable, integration and internal, never a refusal.
        assert_eq!(
            terminate(&Abort::NotDelivered("d-198"), false),
            EngineError::NotDelivered("d-198")
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
        // An undelivered capability stays distinct internally and answers 503 on the wire.
        assert!(matches!(
            OrdersError::from(not_delivered("d-198")),
            OrdersError::Unavailable
        ));
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

    #[test]
    fn lock_and_statement_timeouts_before_commit_are_retryable_never_after() {
        // D-222: a lock wait or statement the deployment bound ended rolled the attempt back.
        for code in ["55P03", "57014"] {
            let store = Abort::Store(ScopeError::Db(driver_error(code, "canceling statement")));
            assert_eq!(terminate(&store, false), EngineError::Unavailable, "{code}");
            let db = Abort::Db(DbError::Sea(driver_error(code, "lock timeout")));
            assert_eq!(terminate(&db, false), EngineError::Unavailable, "{code}");
            let wrapped = Abort::Db(DbError::Other(anyhow::Error::from(ScopeError::Db(
                driver_error(code, "Sperrzeit\u{fc}berschreitung"),
            ))));
            assert_eq!(
                terminate(&wrapped, false),
                EngineError::Unavailable,
                "{code}"
            );
            let registry = Abort::Registry(RegistryError::Store(ScopeError::Db(driver_error(
                code, "x",
            ))));
            assert_eq!(
                terminate(&registry, false),
                EngineError::Unavailable,
                "{code}"
            );
            // At COMMIT the server answered definitely: a rollback, never retryable as an outage.
            assert_eq!(terminate(&store, true), EngineError::Internal, "{code}");
            // A read-only prefetch is retryable the same way.
            assert!(is_temporary_store_failure(&anyhow::Error::from(
                DbError::Sea(driver_error(code, "x"))
            )));
        }
        // Any other server error before COMMIT stays an unexpected persistence failure.
        let denied = Abort::Store(ScopeError::Db(driver_error("42501", "lock timeout")));
        assert_eq!(terminate(&denied, false), EngineError::Internal);
    }

    #[test]
    fn insufficient_privilege_is_decided_by_sqlstate_never_by_message_text() {
        let localized = driver_error(
            "42501",
            "Keine Berechtigung f\u{fc}r Tabelle bss_orders__order",
        );
        assert!(is_insufficient_privilege(&anyhow::Error::from(
            ScopeError::Db(localized)
        )));
        let english_text_other_code = driver_error("XX000", "permission denied for table t");
        assert!(!is_insufficient_privilege(&anyhow::Error::from(
            DbError::Sea(english_text_other_code)
        )));
        assert!(!is_insufficient_privilege(&anyhow::anyhow!(
            "permission denied for table t"
        )));
    }

    #[test]
    fn prefetch_failures_are_unavailable_only_when_temporary() {
        use sea_orm::{DbErr, RuntimeErr};
        let conn = anyhow::Error::from(DbError::Sea(DbErr::Conn(RuntimeErr::Internal(
            "down".into(),
        ))));
        assert!(is_temporary_store_failure(&conn));
        assert_eq!(
            prefetch_failure("draft-mutate", &conn),
            EngineError::Unavailable
        );
        let scoped = anyhow::Error::from(ScopeError::Db(DbErr::ConnectionAcquire(
            sea_orm::ConnAcquireErr::Timeout,
        )));
        assert!(is_temporary_store_failure(&scoped));
        // A permanent failure (here an unexpected record) is never a retryable outage.
        let permanent = anyhow::Error::from(DbError::Sea(DbErr::RecordNotFound("x".into())));
        assert!(!is_temporary_store_failure(&permanent));
        assert_eq!(
            prefetch_failure("create", &permanent),
            EngineError::Internal
        );
        let denied = anyhow::Error::from(ScopeError::Denied("no scope"));
        assert_eq!(prefetch_failure("create", &denied), EngineError::Internal);
    }
}
