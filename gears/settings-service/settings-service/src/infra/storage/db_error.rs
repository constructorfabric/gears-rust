//! The one place a failed database call becomes a [`DomainError`].
//!
//! A database that cannot answer is an outage: the caller may retry, so it is
//! `Unavailable` (`503`). Anything else the database refuses — a malformed
//! row, a constraint, a statement that cannot run — is the service's own fault
//! and is `Internal` (`500`).
//!
//! Only the structural signs of an outage are recognised: the pool could not
//! hand out a connection (`ConnectionAcquire`) or the driver could not open
//! one (`Conn`). A connection that drops in the middle of a statement reaches
//! here as `Query` or `Exec` wrapping the driver's error, which carries no
//! stable structure to match; reading its message text would be fragile, so it
//! stays `Internal`.

use sea_orm::DbErr;
use toolkit_db::secure::ScopeError;

use crate::domain::error::DomainError;

/// A database failure the classifier can read.
pub trait DbFailure: std::fmt::Display {
    /// Whether the database could not be reached at all.
    fn is_unreachable(&self) -> bool;
}

impl DbFailure for DbErr {
    fn is_unreachable(&self) -> bool {
        matches!(self, Self::ConnectionAcquire(_) | Self::Conn(_))
    }
}

impl DbFailure for ScopeError {
    fn is_unreachable(&self) -> bool {
        matches!(self, Self::Db(err) if err.is_unreachable())
    }
}

impl<T: DbFailure + ?Sized> DbFailure for &T {
    fn is_unreachable(&self) -> bool {
        (**self).is_unreachable()
    }
}

/// Project a failed database call.
pub fn db_error(err: impl DbFailure) -> DomainError {
    if err.is_unreachable() {
        DomainError::dependency_unavailable("database", "answer the request", err)
    } else {
        DomainError::Internal {
            diagnostic: err.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "db_error_tests.rs"]
mod db_error_tests;
