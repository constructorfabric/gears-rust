//! Classifies whether redelivery can change an admission failure.
//!
//! The bounded delivery budget makes retry the safe default. Only deterministic
//! configuration, scope and programming failures are classified as permanent;
//! transaction-level contention classification remains in `toolkit_db`.

use toolkit_db::DbError;
use toolkit_db::advisory_locks::DbLockError;
use toolkit_db::secure::ScopeError;

/// Return whether a scoped failure reached the database and may clear.
/// Unknown `#[non_exhaustive]` variants remain permanent by default.
#[must_use]
pub fn scoped_failure_may_clear(error: &ScopeError) -> bool {
    matches!(error, ScopeError::Db(_))
}

/// Return whether a database failure may clear on redelivery.
/// The exhaustive match forces new [`DbError`] variants to be classified.
#[must_use]
pub fn database_failure_may_clear(error: &DbError) -> bool {
    match error {
        // Engine and transport failures may clear.
        DbError::Sqlx(_) | DbError::Sea(_) | DbError::Io(_) => true,

        DbError::Lock(error) => lock_failure_may_clear(error),

        // Configuration and programming failures are deterministic.
        DbError::UnknownDsn(_)
        | DbError::FeatureDisabled(_)
        | DbError::InvalidConfig(_)
        | DbError::ConfigConflict(_)
        | DbError::InvalidSqlitePragma { .. }
        | DbError::UnknownSqlitePragma(_)
        | DbError::InvalidParameter(_)
        | DbError::SqlitePragma(_)
        | DbError::EnvVar { .. }
        | DbError::UrlParse(_)
        | DbError::ConnRequestedInsideTx => false,

        // Preserve scope refusals; retry other opaque causes.
        DbError::Other(error) => error
            .downcast_ref::<ScopeError>()
            .is_none_or(scoped_failure_may_clear),
    }
}

/// Treat lock failures as retryable except deterministic configuration or misuse.
fn lock_failure_may_clear(error: &DbLockError) -> bool {
    !matches!(
        error,
        DbLockError::InvalidConfig { .. } | DbLockError::NotHeld
    )
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod retry_tests;
