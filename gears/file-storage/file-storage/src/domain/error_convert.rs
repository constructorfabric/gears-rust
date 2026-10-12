//! Infrastructure → domain error conversions.
//!
//! Kept out of `error.rs` so the widely-imported `DomainError` type stays free of
//! `toolkit_db` dependencies.

use sea_orm::DbErr;
use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;

use super::error::DomainError;

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<DbError> for DomainError {
    fn from(e: DbError) -> Self {
        Self::database(e.to_string())
    }
}

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<ScopeError> for DomainError {
    fn from(e: ScopeError) -> Self {
        Self::database(e.to_string())
    }
}

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<DbErr> for DomainError {
    fn from(e: DbErr) -> Self {
        Self::database(e.to_string())
    }
}
