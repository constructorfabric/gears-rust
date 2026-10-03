//! REST error lift: `DomainError -> CanonicalError -> Problem` (RFC 9457).

use toolkit_canonical_errors::{CanonicalError, Problem};

use crate::domain::error::DomainError;

/// Handler result type. Every 4xx and 5xx is a `Problem`, boxed: a `Problem`
/// is past `clippy::result_large_err`, and `?` boxes one.
pub type ApiResult<T> = Result<T, Box<Problem>>;

impl From<DomainError> for Problem {
    fn from(err: DomainError) -> Self {
        Self::from(CanonicalError::from(err))
    }
}

impl From<DomainError> for Box<Problem> {
    fn from(err: DomainError) -> Self {
        Box::new(Problem::from(err))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
