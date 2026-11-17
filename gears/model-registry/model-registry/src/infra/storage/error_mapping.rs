//! Storage error classification into [`DomainError`].

use toolkit_db::secure::{ScopeError, is_foreign_key_violation, is_unique_violation};

use crate::domain::error::DomainError;

/// What a failed insert tells about the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertFailure {
    UniqueViolation,
    ForeignKeyViolation,
    Other,
}

/// Classifies an insert failure before it becomes a domain error.
#[must_use]
pub fn classify_insert(err: &ScopeError) -> InsertFailure {
    match err {
        ScopeError::Db(db) if is_unique_violation(db) => InsertFailure::UniqueViolation,
        ScopeError::Db(db) if is_foreign_key_violation(db) => InsertFailure::ForeignKeyViolation,
        _ => InsertFailure::Other,
    }
}

/// Scope errors on reads and scoped writes. `Denied` and `TenantNotInScope`
/// cannot happen for scopes the gear builds itself, so they are invariant
/// failures.
#[must_use]
pub fn map_scope_error(err: ScopeError) -> DomainError {
    match err {
        ScopeError::Db(db) => DomainError::from_db_err(&db),
        other => DomainError::internal(format!("scope error: {other}")),
    }
}
