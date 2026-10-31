//! Persistence of the foundation: entities, migrations and the repositories over `SecureConn`.
//! Every repository function takes `&impl DBRunner` and the `AccessScope` of the operation, so it
//! runs on a connection or inside the caller's transaction alike.

pub mod entity;
pub mod migrations;
pub mod repo;

use toolkit_db::secure::ScopeError;

use crate::domain::error::{Cause, DomainError};

/// Maps a scoped-query failure: a unique violation is a lost race (the caller retries and finds
/// the winner's row), a scope refusal hides the row, anything else is a database failure whose
/// detail is logged and never returned. Each is logged with the storage error, so the violated
/// constraint or the refused tenant is on record even where the caller sees only a 409 or a 403.
pub(crate) fn scope_failure(context: &'static str, err: ScopeError) -> DomainError {
    if err.is_unique_violation() {
        return lost_race(context, &err);
    }
    match err {
        ScopeError::TenantNotInScope { .. } | ScopeError::Denied(_) => refused(context, &err),
        other => {
            tracing::error!(target: "bss_rating.storage", context, error = %other, "storage failure");
            DomainError::Database(
                format!("{context}: storage failure"),
                Some(Cause::new(other)),
            )
        }
    }
}

/// A unique violation: a lost race the caller retries. The violated constraint is logged, as a
/// violation that is no race still reaches the caller only as a 409.
fn lost_race(context: &'static str, err: &ScopeError) -> DomainError {
    tracing::info!(target: "bss_rating.storage", context, error = %err, "unique violation; treated as a lost race");
    DomainError::Concurrent(format!("{context}: a concurrent writer inserted first"))
}

/// A scope refusal. Every write runs under a scope already narrowed to its row's tenant, so a
/// refusal here is a wiring fault, not a caller without a grant: it is logged with the refused
/// tenant or property, while the caller sees only a 403.
fn refused(context: &'static str, err: &ScopeError) -> DomainError {
    tracing::error!(target: "bss_rating.storage", context, error = %err, "the access scope refused a write");
    DomainError::PermissionDenied(format!("{context}: outside the access scope"))
}

/// A stored value that does not decode into its domain type.
pub(crate) fn corrupt(table: &'static str, column: &'static str, value: &str) -> DomainError {
    tracing::error!(target: "bss_rating.storage", table, column, value, "corrupt row");
    DomainError::CorruptRow(format!("{table}.{column}"))
}
