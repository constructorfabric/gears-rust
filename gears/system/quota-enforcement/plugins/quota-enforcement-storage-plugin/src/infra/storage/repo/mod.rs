//! Repositories over the plugin's tables. Every function takes a `DBRunner`,
//! so it works inside and outside a transaction, and an `AccessScope` that
//! `SecureORM` applies to every row.

use sea_orm::QuerySelect;
use sea_orm::sea_query::{LockBehavior, LockType};

pub mod allocation_counter_repo;
pub mod config_repo;
pub mod consumption_counter_repo;
pub mod idempotency_repo;
pub mod lease_repo;
pub mod operation_log_repo;
pub mod quota_repo;
pub mod schema_repo;

pub mod policy_repo;

/// How a locking read behaves when another transaction holds the row.
///
/// The contention-budgeted primitives — debit, credit, rollback and the three
/// lease operations (I8) — always read `Nowait`: a held row is refused at once
/// and the primitive retries under its budget, so none of their transactions
/// ever queues behind another and a wait can never run past the budget. The
/// paths I8 does not cover (Quota lifecycle, the deactivation cascade, the
/// sweeper, the snapshot read's period materialization) keep queueing.
///
/// `SQLite` has no row locks and drops the clause either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowWait {
    /// Refuse a held row rather than queue behind it.
    Nowait,
    /// Queue behind the holder.
    Wait,
}

impl RowWait {
    /// Apply this behaviour as the select's row lock.
    pub(crate) fn apply<S: QuerySelect>(self, select: S) -> S {
        match self {
            Self::Nowait => select.lock_with_behavior(LockType::Update, LockBehavior::Nowait),
            Self::Wait => select.lock(LockType::Update),
        }
    }
}
