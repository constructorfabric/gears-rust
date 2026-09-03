//! Database error conversion helpers.
//!
//! Three tiers of error handling live here, from least to most specific:
//!
//! 1. [`db_err`] -- the untyped fallback. Accepts anything `Display` and
//!    always produces `DomainError::Database`, which the REST layer maps to
//!    HTTP 500 (`api/rest/error.rs`). This is correct for genuine
//!    infrastructure failures (connection lost, pool exhausted, a query the
//!    schema should never let fail), but it was, until this module grew the
//!    two items below, the *only* option -- so a handful of call sites that
//!    fail in an entirely expected, recoverable way (a racing unique-key
//!    insert; a lock-order deadlock under concurrent writers) also surfaced
//!    as an opaque 500.
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
/// This is the untyped fallback described in the module doc comment: it
/// accepts anything `Display` and always stringifies it, discarding the
/// error's original shape. Kept (and still used at every call site that has
/// no more specific classification available) rather than removed, so
/// switching a *different* call site to [`conflict_on_unique_violation`] or
/// [`transaction_with_bounded_retry`] never changes behaviour anywhere else
/// -- every classifier in this module falls back to exactly this shape when
/// it does not recognize the error.
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
/// produces depends only on whether that call goes through `.secure()`, not
/// on anything the caller of `conflict_on_unique_violation` controls.
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
        // Mirrors `ScopeError::is_unique_violation`'s own body (defined in
        // `toolkit_db::secure::error`, not reused by name here to avoid
        // relying on inherent-vs-trait method priority for a same-named
        // method on the same type): only the `Db` variant can ever be a
        // unique-constraint violation; every other variant is a
        // scope/validation error the database never saw.
        match self {
            Self::Db(db_err) => toolkit_is_unique_violation(db_err),
            _ => false,
        }
    }
}

/// Classify a database error: a unique-constraint violation becomes
/// `DomainError::Conflict` (HTTP 409, `api/rest/error.rs`) with
/// `conflict_message`; anything else falls back to exactly the
/// `DomainError::Database` shape [`db_err`] would have produced (HTTP 500),
/// so a caller switching from `.map_err(db_err)` to this function changes
/// behaviour only for the one error shape it is meant to catch.
///
/// `conflict_message` must be a message safe to return to the client
/// verbatim -- `DomainError::Conflict`'s payload is sent as-is
/// (`FileResourceError::aborted(message)` in `api/rest/error.rs`). The
/// original error's text is therefore never included in it; it is logged at
/// `DEBUG` instead; only when the violation is actually classified as a
/// conflict, so a client-facing conflict always leaves a matching log line
/// with the underlying constraint for whoever investigates it. The
/// `conflict_message` argument is lazily converted (`impl Into<String>`
/// rather than an already-built `DomainError`) so the fallback path -- the
/// common case for most errors reaching this function -- never pays for a
/// `String` it discards.
///
/// # Design note: why classify here and not in `db_err` itself
///
/// `db_err` stays generic over `impl Display` because most of its ~90 call
/// sites (see `grep -rn "map_err(db_err)" src`) have no conflict to report
/// -- reads, unconditional deletes, and inserts into tables with no
/// application-level uniqueness invariant genuinely have nothing more
/// specific to say than "a database error occurred". Only the two call
/// sites documented on this function's callers
/// (`repo/idempotency_repo.rs::insert`, `repo/policy_repo.rs::upsert`) rely
/// on a unique index to hold an invariant, so only those switch to this
/// classifier.
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
/// plus two retries) -- the same "small bounded retry" budget the rest of
/// the workspace standardizes on, rather than picking a gear-local number.
const TX_RETRY_ATTEMPTS: u32 = DEFAULT_TX_RETRY_ATTEMPTS;

/// Base delay, growth factor, and cap for [`retry_backoff_delay`].
///
/// Deliberately small: every transaction this wrapper is applied to
/// (`Store::finalize_version`, `delete_file[_with_event]`,
/// `delete_orphan_file_with_event`, `patch_metadata_atomic`,
/// `bind_atomic[_with_event]`) is a handful of point writes against indexed
/// rows, not a long-running batch job -- even the worst case (three failed
/// attempts in a row) adds at most a few hundred milliseconds of backoff
/// before giving up and returning the last error, same as today.
const RETRY_BACKOFF_BASE_MS: u64 = 10;
const RETRY_BACKOFF_FACTOR: u64 = 2;
const RETRY_BACKOFF_MAX: Duration = Duration::from_millis(200);

/// Small jittered backoff before retry attempt `next_attempt` (which must be
/// `>= 2`; the first attempt is never delayed).
///
/// Mirrors the exponential-with-jitter shape of
/// `toolkit_db::secure::db::retry_backoff_delay` -- that helper is private to
/// `toolkit-db` and wired into its own `E: From<DbError>` retry loop
/// (`Db::transaction_with_retry_max`), which this gear cannot reuse directly
/// (see [`transaction_with_bounded_retry`]'s doc comment for why), so this is
/// a small from-scratch reimplementation of the same idea rather than a call
/// into that one. The jitter itself is the reason for reimplementing rather
/// than hand-rolling `base * factor.pow(n)`: without it, two transactions
/// that just deadlocked against each other would both wake and retry at the
/// same instant and could collide again.
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
/// `toolkit_db::contention::is_retryable_contention` is typed over
/// `&sea_orm::DbErr` for good reason -- see that function's doc comment --
/// but the transactions [`transaction_with_bounded_retry`] wraps
/// (`Store::finalize_version`, `delete_file`, ...) call into repository
/// methods (`VersionRepo::finalize`, `FileRepo::delete`, ...) that already
/// converted their `DbErr`/`ScopeError` into `DomainError::Database` via
/// `db_err` *inside* those repositories -- files this task does not touch.
/// By the time the error reaches this wrapper, only the message string
/// survives; the typed error is gone.
///
/// `is_retryable_contention` already handles exactly this situation for its
/// own callers: it classifies a `DbErr::Custom(msg)` by matching `msg`
/// against the same backend-specific signatures (SQLSTATE codes / message
/// fragments) it uses for a real `DbErr::Exec`/`Query`. Re-wrapping the
/// stored message in a synthetic `DbErr::Custom` and handing it to the same
/// function reuses that exact, already-tested matching logic instead of
/// duplicating it here -- the only difference from a "real" typed path is
/// where the string came from.
///
/// A `DomainError` variant other than `Database` (e.g. `Conflict`,
/// `PreconditionFailed`) is never retried: those are domain decisions made
/// by the transaction body itself (`return Err(DomainError::conflict(...))`
/// in `bind_atomic`, for instance), not database errors, and retrying them
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
/// which does exactly this, but it requires the caller to supply
/// `extract_db_err: Fn(&E) -> Option<&sea_orm::DbErr>` -- a way to reach
/// *into* the returned domain error and borrow the original `DbErr` back
/// out. `DomainError::Database` (this gear's `E`) holds only a `String`
/// (`domain/error.rs`, not touched by this change), so no such accessor can
/// exist: there is no borrowed `DbErr` living inside a `DomainError` to
/// return. This wrapper reimplements the same retry/backoff shape directly
/// against `Db::transaction_ref_mapped`, using
/// [`is_retryable_domain_error`]'s string-based reclassification in place of
/// the accessor `toolkit-db`'s version needs.
///
/// # `FnMut`, not `FnOnce`: cloning per attempt
///
/// `Db::transaction_ref_mapped` takes an `FnOnce` closure (a transaction
/// attempt consumes it), but a retry loop must be able to run the same
/// logical attempt again after a failure. `body` is therefore `FnMut`, and
/// every retryable call site in `store/*.rs` clones its captured state
/// (audit rows, events, byte buffers, ...) *inside* `body`, once per
/// invocation, immediately before moving the clones into the `async move`
/// block -- the outer `move` closure captures the original values once and
/// keeps them for the next attempt; only the per-attempt clones are
/// consumed. Every value cloned this way in this gear is a cheap,
/// `#[derive(Clone)]` value: the repositories are zero-sized unit structs,
/// and the domain rows (`AuditEntry`, `FileEvent`, `AutoBindOnFinalize`,
/// `CustomMetadataPatch`) and scalars (`Vec<u8>` hashes, `Option<String>`
/// mime/manifest text) are small, owned, derive-`Clone` data with no
/// interior connection/transaction state to worry about re-using.
///
/// # Safety precondition: no side effects outside the DB before commit
///
/// Retrying re-runs `body` from a fresh `BEGIN`, so `body` must not have
/// performed any effect that persists outside the (rolled-back) transaction
/// -- an outbound network call, a spawned task, anything not undone by the
/// rollback. This holds for every call site this wrapper is actually applied
/// to (see the call sites in `store/versions.rs`, `store/files.rs`,
/// `store/metadata.rs`): each `body` only calls repository methods against
/// `tx` (including `EventsOutboxRepo::enqueue`, which inserts an outbox row
/// -- part of the same transaction, rolled back with everything else, not a
/// direct send). It was **not** applied to every transaction in the gear;
/// see those modules' call sites for which
/// transactions were left un-retried and why.
///
/// # Errors
///
/// Returns the last `DomainError` once the attempt budget
/// ([`TX_RETRY_ATTEMPTS`]) is exhausted, or immediately for any error
/// [`is_retryable_domain_error`] does not recognize as retryable -- in both
/// cases the same error a non-retrying caller of `transaction_ref_mapped`
/// would have received.
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
        // `|tx| body(tx)` is a fresh `FnOnce` value on every loop iteration
        // (it uniquely borrows `body` for the duration of this one call),
        // which is all `transaction_ref_mapped` requires -- `body` itself
        // stays `FnMut` and available for the next attempt. Mirrors
        // `Db::transaction_with_retry_max`'s own
        // `|tx| body(tx)` adapter in `toolkit-db`.
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
                    // Jittered backoff so two transactions that just
                    // deadlocked against each other don't restart in
                    // lockstep and collide again -- see
                    // `retry_backoff_delay`.
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
    use sea_orm::{DbBackend, DbErr, RuntimeErr};
    use toolkit_db::secure::ScopeError;

    use super::{conflict_on_unique_violation, db_err, is_retryable_domain_error};
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
        // Simulates what a caller several layers away from the raw `DbErr`
        // actually sees: `db_err` (used throughout `repo/version_repo.rs`,
        // `repo/file_repo.rs`, ...) has already flattened it into a
        // `DomainError::Database` string before it reaches
        // `transaction_with_bounded_retry`.
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
}
