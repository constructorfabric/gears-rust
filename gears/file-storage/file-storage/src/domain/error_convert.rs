//! Infrastructure → domain error conversions.
//!
//! Kept out of `error.rs` on purpose: `DomainError` is imported by nearly every
//! module in the gear (very high fan-in), so letting it also depend on
//! `toolkit_db` would make it a high-coupling crossroads (Henry–Kafura). The
//! `?`-driven `From` impls live here instead, so the widely-used error *type*
//! stays dependency-light while the infra coupling is confined to this small,
//! rarely-imported module.

use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;

use super::error::DomainError;

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<DbError> for DomainError {
    fn from(e: DbError) -> Self {
        Self::database(e.to_string())
    }
}

/// `domain::pagination`'s cursor codec returns `toolkit_odata::Error`
/// directly rather than a gear-local wrapper, so callers can `?` it straight
/// into `Result<_, DomainError>` and let `api/rest/error.rs` map it via
/// `toolkit_odata`'s own `Error -> CanonicalError` (see `DomainError::Cursor`'s
/// doc comment).
impl From<toolkit_odata::Error> for DomainError {
    fn from(e: toolkit_odata::Error) -> Self {
        Self::Cursor(e)
    }
}

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<ScopeError> for DomainError {
    fn from(e: ScopeError) -> Self {
        Self::database(e.to_string())
    }
}
