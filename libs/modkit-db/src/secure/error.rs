use uuid::Uuid;

/// Errors that can occur during scoped query execution.
#[derive(thiserror::Error, Debug)]
pub enum ScopeError {
    /// Database error occurred during query execution.
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),

    /// Invalid scope configuration.
    #[error("invalid scope: {0}")]
    Invalid(&'static str),

    /// Tenant isolation violation: `tenant_id` is not included in the current scope.
    #[error("access denied: tenant_id not present in security scope ({tenant_id})")]
    TenantNotInScope { tenant_id: Uuid },

    /// Operation denied - entity not accessible in current security scope.
    #[error("access denied: {0}")]
    Denied(&'static str),
}

impl ScopeError {
    /// Returns `true` if this error wraps a unique-constraint violation.
    #[must_use]
    pub fn is_unique_violation(&self) -> bool {
        match self {
            Self::Db(db_err) => is_unique_violation(db_err),
            _ => false,
        }
    }
}

/// Check whether a `sea_orm::DbErr` represents a unique-constraint violation.
///
/// First tries `SeaORM`'s built-in `sql_err()` detection (SQLSTATE-based).
/// Falls back to string matching on the error message for cases where
/// `sql_err()` fails to classify the error (e.g. certain connection proxies
/// or driver wrappers that strip the SQLSTATE code).
///
/// Recognized patterns across backends:
/// - **Postgres** SQLSTATE `23505` — "`unique_violation`" / "duplicate key"
/// - **`SQLite`** extended code `2067` — "UNIQUE constraint failed"
/// - **`MySQL`** error `1062` — "Duplicate entry"
#[must_use]
pub fn is_unique_violation(err: &sea_orm::DbErr) -> bool {
    // Fast path: SeaORM parsed the SQLSTATE / vendor code correctly.
    if matches!(
        err.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    ) {
        return true;
    }

    // Fallback: string-based detection for wrapped / proxied errors.
    let msg = err.to_string().to_lowercase();
    msg.contains("unique constraint")
        || msg.contains("duplicate key")
        || msg.contains("unique_violation")
        || msg.contains("duplicate entry")
        || msg.contains("unique constraint failed")
}
