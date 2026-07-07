//! Error types for the distributed-lease primitive.
//!
//! These are deliberately free of any gear's domain taxonomy: a consuming gear
//! maps [`CoordError`] / [`AckError`] onto its own error type at the call site
//! (e.g. `LeaseHeld` → "a run is already in progress"). That is the one place
//! the AM original differs — it carried a `From<CoordError> for DomainError`;
//! a shared crate cannot, so the mapping lives with the consumer.

use sea_orm::DbErr;

/// Result of an acquire / renew / release call against a lease row.
///
/// `LeaseHeld` is the "another worker already holds this gate" outcome surfaced
/// by the acquire/steal path; `LeaseLost` is the "you used to hold it but a
/// peer stole it (or your TTL lapsed)" outcome surfaced by `renew` and by
/// [`super::guard::LeaseGuard::with_ack_in_tx`]'s fence SELECT.
#[derive(Debug, thiserror::Error)]
pub enum CoordError {
    #[error("lease already held by another worker")]
    LeaseHeld,
    #[error("lease was lost (taken over) before this call could complete")]
    LeaseLost,
    #[error(transparent)]
    Db(#[from] toolkit_db::DbError),
}

impl CoordError {
    /// Extract the underlying `DbErr` for the contention-retry helper.
    ///
    /// Used as the `extract_db_err` accessor passed into
    /// [`toolkit_db::Db::transaction_with_retry`] (the fenced-write path, and
    /// in tests). Returns `None` for non-DB variants so the retry loop
    /// short-circuits on `LeaseHeld` / `LeaseLost`.
    #[must_use]
    pub fn db_err(&self) -> Option<&DbErr> {
        match self {
            Self::Db(toolkit_db::DbError::Sea(e)) => Some(e),
            _ => None,
        }
    }
}

/// Outcome envelope for [`super::guard::LeaseGuard::with_ack_in_tx`].
///
/// `E` is the caller-defined work-error type; the caller hands the guard a
/// `Fn(&E) -> Option<&DbErr>` extractor so the retry helper can decide whether
/// a `Work(_)` failure is retryable contention or a hard failure.
///
/// `LeaseLost` is **never** retried — re-running under a stolen lease cannot
/// succeed and would commit work against the new holder's slot.
#[derive(Debug, thiserror::Error)]
pub enum AckError<E> {
    #[error("lease was lost before the fenced commit could complete")]
    LeaseLost,
    #[error(transparent)]
    Work(E),
    #[error(transparent)]
    Db(#[from] toolkit_db::DbError),
}

impl<E> AckError<E> {
    /// Extract the underlying `DbErr` from any variant for the contention-retry
    /// helper. The caller-supplied `extract_work_db_err` drills into the
    /// `Work(E)` arm; this method composes that accessor with the built-in
    /// `Db(_)` arm so [`toolkit_db::Db::transaction_with_retry`] sees a single
    /// `Fn(&AckError<E>) -> Option<&DbErr>`.
    pub fn db_err<'a, X>(&'a self, extract_work_db_err: &X) -> Option<&'a DbErr>
    where
        X: Fn(&E) -> Option<&DbErr>,
    {
        match self {
            Self::Db(toolkit_db::DbError::Sea(e)) => Some(e),
            Self::Db(_) | Self::LeaseLost => None,
            Self::Work(w) => extract_work_db_err(w),
        }
    }
}
