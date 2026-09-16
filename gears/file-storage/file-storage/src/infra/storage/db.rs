//! Database error conversion helpers.
//!
//! Three tiers of error handling live here, from least to most specific:
//!
//! 1. [`db_err`] -- the untyped fallback: any `Display` error becomes
//!    `DomainError::Database`, which the REST layer maps to HTTP 500
//!    (`api/rest/error.rs`). Correct for genuine infrastructure failures
//!    (connection lost, pool exhausted); wrong for the recoverable ones
//!    (racing unique-key insert, lock-order deadlock) that (2) and (3) exist
//!    to catch.
//! 2. [`conflict_on_unique_violation`] -- classifies a *typed* database
//!    error (`sea_orm::DbErr` or `toolkit_db::secure::ScopeError`, the two
//!    shapes this gear's repositories ever see) as a unique-constraint
//!    violation and maps it to `DomainError::Conflict` (HTTP 409) instead,
//!    falling back to the same shape `db_err` would have produced otherwise.
//!    Used where a repository still holds the original typed error, i.e.
//!    right at the `.map_err(..)` call site of the failing query.
//! 3. [`transaction_with_bounded_retry`] -- retries a transaction body a
//!    bounded number of times when it fails with a lock-contention error
//!    (`PostgreSQL` serialization failure / deadlock, `MySQL` deadlock,
//!    `SQLite` `BUSY`). Unlike (2), this operates on an already-mapped
//!    [`DomainError`] (see that function's doc comment for why), because the
//!    transactions that need it call through repository methods several
//!    layers away from the raw driver error.

use std::fmt::Display;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use sea_orm::{DbBackend, DbErr};
use toolkit_db::contention::is_retryable_contention;
use toolkit_db::secure::{
    DEFAULT_TX_RETRY_ATTEMPTS, Db, DbTx, ScopeError,
    is_unique_violation as toolkit_is_unique_violation,
};

use crate::domain::error::DomainError;

/// Convert any displayable error into a [`DomainError::Database`].
///
/// The untyped fallback from the module doc comment: stringifies the error,
/// discarding its original shape. Every classifier in this module falls back
/// to exactly this shape when it does not recognize the error, so switching
/// one call site to a more specific classifier never changes behaviour
/// anywhere else.
pub fn db_err(e: impl Display) -> DomainError {
    DomainError::database(e.to_string())
}

/// A database error shape that can report whether it wraps a
/// unique-constraint violation.
///
/// Implemented for the two error types that actually reach a `db.rs` call
/// site in this gear:
/// - `sea_orm::DbErr` -- produced by a raw query that does not go through
///   `SecureORM` (e.g. a plain `Entity::delete_many()...exec(conn)`);
/// - `toolkit_db::secure::ScopeError` -- produced by anything routed through
///   `.secure()`/`secure_insert` (tenant-scoped inserts, scoped
///   deletes/updates).
///
/// [`conflict_on_unique_violation`] is generic over this trait instead of
/// hard-coding one of the two, because which shape a given repository call
/// produces depends only on whether that call goes through `.secure()`.
pub trait ClassifiableDbError: Display {
    /// Returns `true` if `self` represents a unique-constraint violation
    /// (`PostgreSQL` `23505`, `SQLite` "UNIQUE constraint failed", `MySQL`
    /// `1062`, ...). See `toolkit_db::secure::error::is_unique_violation`
    /// for the exact detection rules (SQLSTATE fast path + message
    /// fallback).
    fn is_unique_violation(&self) -> bool;
}

impl ClassifiableDbError for DbErr {
    fn is_unique_violation(&self) -> bool {
        toolkit_is_unique_violation(self)
    }
}

impl ClassifiableDbError for ScopeError {
    fn is_unique_violation(&self) -> bool {
        // Only the `Db` variant can be a unique-constraint violation; every
        // other variant is a scope/validation error the database never saw.
        match self {
            Self::Db(db_err) => toolkit_is_unique_violation(db_err),
            _ => false,
        }
    }
}

/// Classify a database error: a unique-constraint violation becomes
/// `DomainError::Conflict` (HTTP 409, `api/rest/error.rs`) with
/// `conflict_message`; anything else falls back to exactly the
/// `DomainError::Database` shape [`db_err`] would have produced (HTTP 500).
///
/// `conflict_message` must be safe to return to the client verbatim --
/// `DomainError::Conflict`'s payload is sent as-is
/// (`FileResourceError::aborted(message)` in `api/rest/error.rs`). The
/// original error's text is never included in it; it is logged at `DEBUG`
/// instead, only on the conflict path, so a client-facing conflict always
/// leaves a matching log line with the underlying constraint. The argument
/// is lazily converted (`impl Into<String>`) so the common fallback path
/// never pays for a `String` it discards.
///
/// Only call sites backed by a unique index that holds an application-level
/// invariant use this instead of plain [`db_err`]
/// (`repo/idempotency_repo.rs::insert`, `repo/policy_repo.rs::upsert`); reads,
/// unconditional deletes, and inserts with no such invariant have nothing
/// more specific to say than "a database error occurred".
pub fn conflict_on_unique_violation<E: ClassifiableDbError>(
    e: E,
    conflict_message: impl Into<String>,
) -> DomainError {
    if e.is_unique_violation() {
        let message = conflict_message.into();
        tracing::debug!(
            error = %e,
            conflict_message = %message,
            "database unique-constraint violation classified as a conflict"
        );
        DomainError::conflict(message)
    } else {
        db_err(e)
    }
}

/// Attempt budget for [`transaction_with_bounded_retry`].
///
/// Reuses [`toolkit_db::secure::DEFAULT_TX_RETRY_ATTEMPTS`] (3: the first try
/// plus two retries) -- the same bounded-retry budget the rest of the
/// workspace standardizes on, rather than a gear-local number.
const TX_RETRY_ATTEMPTS: u32 = DEFAULT_TX_RETRY_ATTEMPTS;

/// Base delay, growth factor, and cap for [`retry_backoff_delay`].
///
/// Deliberately small: every transaction this wrapper is applied to is a
/// handful of point writes against indexed rows, not a batch job -- even
/// three failed attempts in a row add at most a few hundred milliseconds of
/// backoff before giving up and returning the last error.
const RETRY_BACKOFF_BASE_MS: u64 = 10;
const RETRY_BACKOFF_FACTOR: u64 = 2;
const RETRY_BACKOFF_MAX: Duration = Duration::from_millis(200);

/// Small jittered backoff before retry attempt `next_attempt` (which must be
/// `>= 2`; the first attempt is never delayed).
///
/// A from-scratch reimplementation of `toolkit_db::secure::db`'s private
/// exponential-with-jitter helper (see [`transaction_with_bounded_retry`] for
/// why this gear cannot reuse that one directly). The jitter matters: without
/// it, two transactions that just deadlocked against each other would both
/// wake and retry at the same instant and could collide again.
fn retry_backoff_delay(next_attempt: u32) -> Duration {
    use tokio_retry::strategy::{ExponentialBackoff, jitter};

    debug_assert!(
        next_attempt >= 2,
        "attempt 1 (the first try) is never delayed"
    );
    // 0-based index into the backoff sequence: the delay before attempt 2 is
    // the sequence's first element, attempt 3's delay is the second, ...
    let index = next_attempt.saturating_sub(2) as usize;

    let base = ExponentialBackoff::from_millis(RETRY_BACKOFF_BASE_MS)
        .factor(RETRY_BACKOFF_FACTOR)
        .max_delay(RETRY_BACKOFF_MAX)
        .nth(index)
        .unwrap_or(RETRY_BACKOFF_MAX);

    jitter(base)
}

/// Re-derive retryable-contention classification for an already-mapped
/// [`DomainError`].
///
/// # Why this works on a string, not the original `DbErr`
///
/// By the time an error reaches [`transaction_with_bounded_retry`], the
/// repository methods it calls through have already flattened their
/// `DbErr`/`ScopeError` into a `DomainError::Database` string via [`db_err`];
/// the typed error is gone. `is_retryable_contention` classifies by matching
/// the error's message text against backend-specific signatures (SQLSTATE
/// codes and message fragments), and already accepts a `DbErr::Custom(msg)`
/// for exactly this case -- a documented path in `toolkit_db::contention` --
/// so re-wrapping the stored string in a synthetic `DbErr::Custom` reuses
/// that matching logic unchanged. Because the SQLSTATE code does not survive
/// into the message on every path, this classification leans on message
/// text, which is `lc_messages`-dependent: a `PostgreSQL` server running with
/// a non-English locale can produce contention errors this does not
/// recognize.
///
/// A `DomainError` variant other than `Database` (e.g. `Conflict`,
/// `PreconditionFailed`) is never retried: those are domain decisions made
/// by the transaction body itself, not database errors, and retrying them
/// would just reproduce the same decision.
fn is_retryable_domain_error(e: &DomainError, backend: DbBackend) -> bool {
    match e {
        DomainError::Database { message } => {
            is_retryable_contention(backend, &DbErr::Custom(message.clone()))
        }
        _ => false,
    }
}

/// Run a transaction with a bounded number of retries on transient
/// lock-contention failures (`PostgreSQL` serialization failure `40001` /
/// deadlock `40P01`, `MySQL`/`InnoDB` deadlock, `SQLite` `BUSY` /
/// `BUSY_SNAPSHOT` -- see `toolkit_db::contention`).
///
/// # Why a bespoke wrapper instead of `Db::transaction_with_retry`
///
/// `toolkit_db::secure::Db` already provides
/// [`Db::transaction_with_retry`](toolkit_db::secure::Db::transaction_with_retry),
/// but it requires an `extract_db_err: Fn(&E) -> Option<&sea_orm::DbErr>`
/// accessor to reach the original `DbErr` back out of the returned error.
/// `DomainError::Database` holds only a `String`, so no such accessor can
/// exist. This wrapper reimplements the same retry/backoff shape against
/// `Db::transaction_ref_mapped`, using [`is_retryable_domain_error`]'s
/// string-based reclassification instead.
///
/// # `FnMut`, not `FnOnce`: cloning per attempt
///
/// `Db::transaction_ref_mapped` takes an `FnOnce` closure, but a retry loop
/// must run the same logical attempt again after a failure. `body` is
/// therefore `FnMut`, and every retryable call site in `store/*.rs` clones
/// its captured state (audit rows, events, byte buffers, ...) *inside*
/// `body`, once per invocation, before moving the clones into the
/// `async move` block -- the outer `move` closure keeps the originals for
/// the next attempt; only the per-attempt clones are consumed. Every such
/// value is a small, owned, `#[derive(Clone)]` type (domain rows, scalar
/// hashes/strings) with no connection or transaction state to re-use.
///
/// # Safety precondition: no side effects outside the DB before commit
///
/// Retrying re-runs `body` from a fresh `BEGIN`, so `body` must not perform
/// any effect that persists outside the (rolled-back) transaction -- an
/// outbound network call, a spawned task, anything the rollback would not
/// undo. This holds at every call site this wrapper is applied to
/// (`store/versions.rs`, `store/files.rs`, `store/metadata.rs`): each `body`
/// only calls repository methods against `tx`, including
/// `EventsOutboxRepo::enqueue`, which inserts an outbox row as part of the
/// same transaction rather than sending anything directly. It is not applied
/// to every transaction in the gear -- see those modules' call sites for
/// which ones were left un-retried and why.
///
/// # Errors
///
/// Returns the last `DomainError` once [`TX_RETRY_ATTEMPTS`] is exhausted, or
/// immediately for any error [`is_retryable_domain_error`] does not
/// recognize as retryable -- in both cases the same error a non-retrying
/// caller of `transaction_ref_mapped` would have received.
pub async fn transaction_with_bounded_retry<T, F>(db: &Db, mut body: F) -> Result<T, DomainError>
where
    T: Send + 'static,
    F: for<'a> FnMut(
            &'a DbTx<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<T, DomainError>> + Send + 'a>>
        + Send,
{
    let backend = db.backend();
    let mut attempt: u32 = 1;

    loop {
        // `|tx| body(tx)` is a fresh `FnOnce` per iteration (it uniquely
        // borrows `body` for one call), which is all `transaction_ref_mapped`
        // requires -- `body` stays `FnMut` and available for the next
        // attempt.
        let result = db.transaction_ref_mapped(|tx| body(tx)).await;

        match result {
            Ok(value) => return Ok(value),
            Err(e) => {
                if attempt < TX_RETRY_ATTEMPTS && is_retryable_domain_error(&e, backend) {
                    let next_attempt = attempt + 1;
                    let delay = retry_backoff_delay(next_attempt);
                    tracing::warn!(
                        attempt,
                        max_attempts = TX_RETRY_ATTEMPTS,
                        delay = ?delay,
                        error = %e,
                        "retrying transaction after a likely lock-contention failure"
                    );
                    // Jittered -- see `retry_backoff_delay`.
                    tokio::time::sleep(delay).await;
                    attempt = next_attempt;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sea_orm::{DbBackend, DbErr, RuntimeErr};
    use toolkit_db::secure::ScopeError;

    use super::{
        TX_RETRY_ATTEMPTS, conflict_on_unique_violation, db_err, is_retryable_domain_error,
        transaction_with_bounded_retry,
    };
    use crate::domain::error::DomainError;

    // -- conflict_on_unique_violation ------------------------------------

    #[test]
    fn unique_violation_dberr_becomes_conflict() {
        let err = DbErr::Custom(
            "duplicate key value violates unique constraint \
             \"policies_tenant_scope_unique_idx\""
                .to_owned(),
        );
        let mapped = conflict_on_unique_violation(err, "policy already exists for this scope");
        assert!(
            matches!(mapped, DomainError::Conflict { .. }),
            "expected Conflict, got {mapped:?}"
        );
    }

    #[test]
    fn non_unique_dberr_falls_back_to_database() {
        let err = DbErr::Custom("connection reset by peer".to_owned());
        let mapped = conflict_on_unique_violation(err, "should never be used");
        assert!(
            matches!(mapped, DomainError::Database { .. }),
            "expected Database (the db_err fallback), got {mapped:?}"
        );
    }

    #[test]
    fn unique_violation_scope_error_becomes_conflict() {
        // The shape `secure_insert` actually returns -- see
        // `repo/idempotency_repo.rs::insert` and `repo/policy_repo.rs::upsert`.
        let err = ScopeError::Db(DbErr::Custom(
            "UNIQUE constraint failed: idempotency_keys.idempotency_key".to_owned(),
        ));
        let mapped = conflict_on_unique_violation(err, "idempotency key already claimed");
        assert!(
            matches!(mapped, DomainError::Conflict { .. }),
            "expected Conflict, got {mapped:?}"
        );
    }

    #[test]
    fn non_db_scope_error_falls_back_to_database() {
        let err = ScopeError::Invalid("tenant_id is required");
        let mapped = conflict_on_unique_violation(err, "should never be used");
        assert!(
            matches!(mapped, DomainError::Database { .. }),
            "expected Database (the db_err fallback), got {mapped:?}"
        );
    }

    #[test]
    fn fallback_matches_db_err_exactly() {
        // The fallback branch must be indistinguishable from calling
        // `db_err` directly, so switching a call site to
        // `conflict_on_unique_violation` never changes behaviour for the
        // errors it doesn't classify.
        let err = DbErr::Custom("connection reset by peer".to_owned());
        let via_classifier = conflict_on_unique_violation(err.clone(), "unused");
        let via_db_err = db_err(err);
        match (via_classifier, via_db_err) {
            (DomainError::Database { message: a }, DomainError::Database { message: b }) => {
                assert_eq!(a, b);
            }
            other => panic!("expected two Database variants, got {other:?}"),
        }
    }

    // -- is_retryable_domain_error ----------------------------------------

    #[test]
    fn retryable_contention_detected_through_the_db_err_round_trip() {
        // What a caller several layers from the raw `DbErr` actually sees:
        // already flattened into a `DomainError::Database` string.
        let raw = DbErr::Exec(RuntimeErr::Internal(
            "error returned from database: deadlock detected".to_owned(),
        ));
        let domain_err = db_err(raw);
        assert!(is_retryable_domain_error(&domain_err, DbBackend::Postgres));
    }

    #[test]
    fn non_contention_database_error_not_retried() {
        let domain_err = DomainError::database("connection reset by peer");
        assert!(!is_retryable_domain_error(&domain_err, DbBackend::Postgres));
    }

    #[test]
    fn non_database_domain_error_never_retried() {
        // A domain decision (CAS lost, etc.), not a database error -- must
        // never be treated as retryable no matter what text it carries.
        let domain_err = DomainError::conflict("target version no longer exists (40P01)");
        assert!(!is_retryable_domain_error(&domain_err, DbBackend::Postgres));
    }

    // -- transaction_with_bounded_retry -------------------------------------
    //
    // Builds a real in-memory SQLite `Db` via `toolkit_db::connect_db` --
    // the same public entry point `tests/common/mod.rs::test_db()` uses --
    // rather than mocking `Db`/`DbTx`, both of which have no public
    // constructor outside `toolkit_db` itself. No migrations run: the
    // transaction bodies below never touch a table, only the retry
    // bookkeeping around `Db::transaction_ref_mapped`.

    async fn retry_test_db() -> toolkit_db::secure::Db {
        let opts = toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        toolkit_db::connect_db("sqlite::memory:", opts)
            .await
            .expect("connect to in-memory SQLite")
    }

    /// A message shape `is_sqlite_busy` (`toolkit_db::contention`) recognizes
    /// for the `Sqlite` backend our test `Db` reports via `db.backend()`:
    /// needs both the busy status code and the locked-database text.
    const RETRYABLE_SQLITE_MSG: &str = "(code: 5) database is locked";

    #[tokio::test]
    async fn bounded_retry_succeeds_after_transient_failures_within_budget() {
        let db = retry_test_db().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_body = Arc::clone(&calls);

        let result: Result<usize, DomainError> = transaction_with_bounded_retry(&db, move |_tx| {
            let calls = Arc::clone(&calls_for_body);
            Box::pin(async move {
                let attempt = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt < TX_RETRY_ATTEMPTS as usize {
                    Err(DomainError::database(RETRYABLE_SQLITE_MSG))
                } else {
                    Ok(attempt)
                }
            })
        })
        .await;

        assert_eq!(
            result.expect("must succeed once the body stops failing"),
            TX_RETRY_ATTEMPTS as usize,
            "the successful attempt must be exactly the last one in the budget"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            TX_RETRY_ATTEMPTS as usize,
            "body must be invoked exactly once per attempt, no more"
        );
    }

    #[tokio::test]
    async fn bounded_retry_exhausts_budget_and_returns_last_error_unchanged() {
        let db = retry_test_db().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_body = Arc::clone(&calls);

        let result: Result<(), DomainError> = transaction_with_bounded_retry(&db, move |_tx| {
            let calls = Arc::clone(&calls_for_body);
            Box::pin(async move {
                let attempt = calls.fetch_add(1, Ordering::SeqCst) + 1;
                // Distinct message per attempt so the final returned error
                // can be checked to be the LAST attempt's, not the first.
                Err(DomainError::database(format!(
                    "{RETRYABLE_SQLITE_MSG} (attempt {attempt})"
                )))
            })
        })
        .await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            TX_RETRY_ATTEMPTS as usize,
            "must stop retrying once the attempt budget is exhausted, not loop forever"
        );
        match result {
            Err(DomainError::Database { message }) => {
                assert!(
                    message.contains(&format!("attempt {TX_RETRY_ATTEMPTS}")),
                    "expected the LAST attempt's error to be returned unchanged, got: {message}"
                );
            }
            other => panic!("expected a Database error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bounded_retry_never_retries_a_nonretryable_error() {
        let db = retry_test_db().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_body = Arc::clone(&calls);

        let result: Result<(), DomainError> = transaction_with_bounded_retry(&db, move |_tx| {
            let calls = Arc::clone(&calls_for_body);
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                // A domain decision (CAS lost), not a database error -- must
                // never be retried no matter how many attempts remain.
                Err(DomainError::conflict("target version no longer exists"))
            })
        })
        .await;

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a non-retryable error must stop the loop after the first attempt"
        );
        assert!(
            matches!(result, Err(DomainError::Conflict { .. })),
            "expected the original Conflict error to pass through unchanged, got {result:?}"
        );
    }
}
