//! The acquisition contention timeout (I8): how long a primitive may wait on a
//! contended row before it gives up with `LeaseContentionTimeout`.
//!
//! # Mechanism: `NOWAIT` and a bounded retry
//!
//! DESIGN leaves the mechanism to the plugin. This one never lets a transaction
//! queue behind another:
//!
//! - Every row lock the budgeted primitives take — debit, credit, rollback and
//!   the three lease operations — is `FOR UPDATE ... NOWAIT`
//!   ([`RowWait::Nowait`](super::repo::RowWait)). A held row is refused at
//!   once with `PostgreSQL`'s `lock_not_available` (SQLSTATE `55P03`).
//! - [`with_budget`] then rolls the whole transaction back, sleeps for the
//!   shorter of its backoff and what remains of the budget, and runs it again,
//!   until it succeeds or the budget is spent. No attempt starts once the
//!   deadline has passed, so at the 0 ms platform default there is no retry:
//!   the first refusal is the answer.
//!
//! The budget is a deadline, so it bounds the operation's total wait across
//! every lock and every retry, not each wait separately. And a retrying
//! transaction holds no row while it backs off: it does not sit on the Quota
//! rows it already locked while waiting for a counter someone else holds, so
//! one slow holder cannot build a convoy behind itself.
//!
//! The cost is fairness: waiters poll rather than queue, so under heavy
//! contention the one that retries at the right moment wins, not the one that
//! arrived first. A starved waiter meets its budget and is told so, which is
//! what I8 promises.
//!
//! # The idempotency scope: a stripe row
//!
//! An insert that meets another transaction's *uncommitted* key waits for that
//! transaction to end, and so does a delete of a row another transaction has
//! already deleted; neither has a `NOWAIT`. Both happen on the idempotency
//! record when two writers share a scope, and the Quota row locks cannot keep
//! those writers apart when their Quota rows do not overlap.
//!
//! So each scope maps to one of a fixed set of rows, `qe_idempotency_stripes`
//! ([`stripe_of`](super::repo::idempotency_repo::stripe_of)), created by
//! migration and never inserted or deleted at run time. Every primitive that
//! writes a record locks its scope's stripe `NOWAIT` inside its transaction,
//! at rank 2 — after its Quota rows, before it reads the record — and so holds
//! it exactly as long as the transaction lives: it cannot outlive the
//! transaction or be lost apart from it. A second writer of the scope is
//! refused and retries under its budget; once it gets in, the winner's record
//! is committed, so its `replay_of` answers and its delete and insert meet no
//! open transaction. A rollback locks two stripes, its own scope's and the
//! original's, deduplicated and in ascending order. The retention sweeper
//! takes a stripe the same way but `SKIP LOCKED`, and deletes only a record
//! still expired, so it neither waits on a writer nor removes a replacement.
//!
//! **Unrelated scopes can share a stripe.** They then serialize like two
//! writers of one scope: at the 0 ms default the second is refused with
//! `LEASE_CONTENTION_TIMEOUT` although nothing it touches is contended, and
//! with a positive budget it waits for the first. With 65 536 stripes the
//! chance is about one in 65 536 per pair of concurrent writers. A retention
//! pass holding a stripe for its one delete has the same effect.
//!
//! A period row's first insert needs no such lock: the Quota row lock already
//! serializes its writers. The capacity row is created with its first Quota
//! rather than its first lease, so an acquisition only ever locks it.
//!
//! `SQLite` has no row locks and drops the lock clauses; writers serialize on
//! the database lock, bounded by the connection's busy timeout.

use std::future::Future;
use std::time::{Duration, Instant};

use quota_enforcement_sdk::{BatchTimer, IdempotencyScope, StorageError};
use sea_orm::{DbErr, RuntimeErr};
use toolkit_db::secure::{DBRunner, ScopeError};

use super::consumption_store::{SqlConsumptionStore, TxError};

/// `PostgreSQL`'s `lock_not_available`: what a `NOWAIT` read on a held row gets.
const LOCK_NOT_AVAILABLE: &str = "55P03";

/// The first pause after a refused lock. Short, because most holders finish in
/// well under a millisecond of work.
const FIRST_BACKOFF: Duration = Duration::from_millis(2);

/// The longest single pause, so a long budget still retries often enough to
/// notice a released row promptly.
const MAX_BACKOFF: Duration = Duration::from_millis(50);

/// What remains of one primitive's contention budget.
///
/// A deadline rather than a duration: every retry of the operation spends the
/// same budget, so it bounds the total wait, however many locks and attempts
/// that wait is spread across.
#[derive(Debug, Clone, Copy)]
pub(super) struct ContentionBudget {
    deadline: Instant,
}

impl ContentionBudget {
    /// A budget of `timeout`, starting now. Zero means "do not wait at all".
    pub(super) fn starting_now(timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + timeout,
        }
    }

    /// The budget with the earlier deadline: a primitive spanning several
    /// metrics waits no longer than the strictest of them allows.
    pub(super) fn stricter(self, other: Self) -> Self {
        if other.deadline < self.deadline {
            other
        } else {
            self
        }
    }

    fn remaining(self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// Pause before another attempt: the shortest of `backoff`, what remains,
    /// and `cap`, then double `backoff`. `false` when no budget is left to
    /// start one, whether it was spent before the pause or by it.
    async fn pause(self, backoff: &mut Duration, cap: Option<Duration>) -> bool {
        let remaining = self.remaining();
        if remaining.is_zero() {
            return false;
        }
        let pause = (*backoff).min(remaining);
        tokio::time::sleep(cap.map_or(pause, |cap| pause.min(cap))).await;
        *backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
        !self.remaining().is_zero()
    }
}

/// Run `attempt` — one whole transaction — until it no longer meets a held row
/// or the budget is spent.
///
/// Every other outcome, success or failure, is returned as it came: only a
/// refused lock is retried, and only while budget remains.
///
/// # Errors
///
/// `LeaseContentionTimeout` once the budget is spent on refused locks, and any
/// other error of `attempt` unchanged.
pub(super) async fn with_budget<T, F, Fut>(
    budget: ContentionBudget,
    attempt: F,
) -> Result<T, TxError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, TxError>>,
{
    with_budget_within(budget, None, attempt).await
}

/// [`with_budget`] that also answers to a batch timer. Once an attempt has
/// armed it, a pause never outlasts what remains of it, and a timer the pause
/// spent ends the retries with `BatchTimeout` rather than starting an attempt
/// past the deadline. A pause only ever follows a rolled-back attempt, so no
/// write or commit is cut short.
///
/// # Errors
///
/// `BatchTimeout` once the armed timer is spent, `LeaseContentionTimeout` once
/// the budget is, and any other error of `attempt` unchanged.
pub(super) async fn with_budget_within<T, F, Fut>(
    budget: ContentionBudget,
    timer: Option<&BatchTimer>,
    mut attempt: F,
) -> Result<T, TxError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, TxError>>,
{
    let mut backoff = FIRST_BACKOFF;
    loop {
        match attempt().await {
            // The transaction has already rolled back, so the pause holds no
            // row lock of ours.
            Err(error) if is_lock_not_available(&error) => {
                let cap = timer.and_then(BatchTimer::armed_remaining);
                let within = budget.pause(&mut backoff, cap).await;
                if timer.is_some_and(BatchTimer::expired) {
                    return Err(TxError::Storage(StorageError::BatchTimeout));
                }
                if !within {
                    return Err(TxError::Storage(StorageError::LeaseContentionTimeout));
                }
            }
            other => return other,
        }
    }
}

/// Whether `error` is a `NOWAIT` read that met a held row.
///
/// The driver's SQLSTATE first, then the message for an error that was
/// re-wrapped on the way here and lost its typed shape.
pub(super) fn is_lock_not_available(error: &TxError) -> bool {
    let TxError::Scope(ScopeError::Db(db_error)) = error else {
        return false;
    };
    if let DbErr::Exec(RuntimeErr::SqlxError(driver)) | DbErr::Query(RuntimeErr::SqlxError(driver)) =
        db_error
        && let Some(database) = driver.as_database_error()
        && database.code().as_deref() == Some(LOCK_NOT_AVAILABLE)
    {
        return true;
    }
    db_error.to_string().contains("could not obtain lock on")
}

/// Lock the stripes of `scopes` (rank 2): deduplicated, in ascending order,
/// each `NOWAIT`, so a held one fails the transaction with
/// `lock_not_available` for [`with_budget`] to retry.
///
/// # Errors
///
/// The refused lock or any other database error; `Internal` for a stripe the
/// migration did not create.
pub(super) async fn lock_scopes(
    tx: &impl DBRunner,
    scopes: &[&IdempotencyScope],
) -> Result<(), TxError> {
    let mut stripes: Vec<i32> = scopes
        .iter()
        .map(|of| {
            super::repo::idempotency_repo::stripe_of(&super::consumption_store::scope_key_of(of))
        })
        .collect();
    stripes.sort_unstable();
    stripes.dedup();
    for stripe in stripes {
        if !super::repo::idempotency_repo::lock_stripe(tx, stripe).await? {
            return Err(TxError::Storage(StorageError::Internal(format!(
                "idempotency stripe {stripe} is missing"
            ))));
        }
    }
    Ok(())
}

impl SqlConsumptionStore {
    /// The contention budget configured for `metric` — its own row, else the
    /// platform default — starting now. `None` reads the platform default,
    /// for a primitive that cannot learn its metric before it locks.
    pub(super) async fn contention_budget(
        &self,
        metric: Option<&str>,
    ) -> Result<ContentionBudget, StorageError> {
        let conn = self.db.conn().map_err(|error| {
            super::consumption_store::unavailable("read contention timeout", "config", &error)
        })?;
        let configured = super::repo::config_repo::read_contention_timeout(
            &conn,
            metric.unwrap_or(crate::infra::storage::entity::DEFAULT_KEY),
        )
        .await
        .map_err(|error| {
            super::consumption_store::unavailable("read contention timeout", "config", &error)
        })?;
        let millis = u64::try_from(configured.unwrap_or(0)).unwrap_or(0);
        Ok(ContentionBudget::starting_now(Duration::from_millis(
            millis,
        )))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "locking_tests.rs"]
mod tests;
