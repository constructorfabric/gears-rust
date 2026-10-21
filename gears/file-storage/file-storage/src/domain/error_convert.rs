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
        match e {
            DbError::Sea(db) => Self::from(db),
            other => Self::database(other.to_string()),
        }
    }
}

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<ScopeError> for DomainError {
    fn from(e: ScopeError) -> Self {
        match e {
            ScopeError::Db(db) => Self::from(db),
            other => Self::database(other.to_string()),
        }
    }
}

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<DbErr> for DomainError {
    fn from(e: DbErr) -> Self {
        Self::database_with_source(e.to_string(), e)
    }
}

/// The original `DbErr` behind a database error, for `Db::transaction_with_retry`.
pub(crate) fn db_err(e: &DomainError) -> Option<&DbErr> {
    match e {
        DomainError::Database {
            source: Some(source),
            ..
        } => source.downcast_ref::<DbErr>(),
        _ => None,
    }
}
