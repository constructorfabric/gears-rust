//! Recognized temporary storage failures for idempotent operations.
//! This classifies redelivery; it does not change transaction retry defaults.
//! Unknown errors are permanent: retries require a recognized temporary cause.

use crate::DbError;
use crate::secure::ScopeError;
use sea_orm::{ConnAcquireErr, DbBackend, DbErr, RuntimeErr, SqlxError};

/// Whether a scoped storage failure has a recognized temporary cause.
#[must_use]
pub fn scope(error: &ScopeError, backend: DbBackend) -> bool {
    match error {
        ScopeError::Db(error) => sea(error, backend),
        _ => false,
    }
}

/// Whether a database failure can be retried by an idempotent caller.
/// Unknown/configuration/access failures are permanent.
#[must_use]
pub fn database(error: &DbError, backend: DbBackend) -> bool {
    match error {
        DbError::Sea(error) => sea(error, backend),
        DbError::Sqlx(error) => sqlx(error, backend),
        DbError::Io(error) => transport(error),
        // DBProvider preserves scoped failures in this wrapper.
        DbError::Other(error) => {
            if let Some(error) = error.downcast_ref::<ScopeError>() {
                scope(error, backend)
            } else if let Some(error) = error.downcast_ref::<DbErr>() {
                sea(error, backend)
            } else if let Some(error) = error.downcast_ref::<SqlxError>() {
                sqlx(error, backend)
            } else {
                false
            }
        }
        _ => false,
    }
}

fn sea(error: &DbErr, backend: DbBackend) -> bool {
    match error {
        DbErr::ConnectionAcquire(ConnAcquireErr::Timeout) => true,
        DbErr::Conn(RuntimeErr::SqlxError(error))
        | DbErr::Exec(RuntimeErr::SqlxError(error))
        | DbErr::Query(RuntimeErr::SqlxError(error)) => sqlx(error, backend),
        // Preserve ToolKit's scoped contention fallback only when the driver
        // did not retain a typed error. A SQL message cannot override its code.
        _ => crate::contention::is_retryable_contention(backend, error),
    }
}

fn sqlx(error: &SqlxError, backend: DbBackend) -> bool {
    match error {
        SqlxError::PoolTimedOut => true,
        SqlxError::Io(error) => transport(error),
        #[cfg(feature = "mysql")]
        SqlxError::Database(error) if backend == DbBackend::MySql => {
            // MySQL code() is SQLSTATE; number() is the more precise vendor code.
            error
                .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
                .is_some_and(|error| matches!(error.number(), 1040 | 1205 | 1213 | 2006 | 2013))
        }
        SqlxError::Database(error) => error.code().is_some_and(|code| match backend {
            DbBackend::Postgres => matches!(
                code.as_ref(),
                // Connection loss, serialization/deadlock, lock unavailable,
                // shutdown/recovery and temporary connection exhaustion.
                "08000"
                    | "08001"
                    | "08003"
                    | "08006"
                    | "08007"
                    | "40001"
                    | "40P01"
                    | "55P03"
                    | "57P01"
                    | "57P02"
                    | "57P03"
                    | "53300"
            ),

            DbBackend::Sqlite => code
                .parse::<u32>()
                .is_ok_and(|code| matches!(code & 0xff, 5 | 6)),
            _ => false,
        }),
        _ => false,
    }
}

fn transport(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        error.kind(),
        ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::NotConnected
            | ErrorKind::BrokenPipe
            | ErrorKind::TimedOut
            | ErrorKind::Interrupted
            | ErrorKind::UnexpectedEof
            | ErrorKind::WouldBlock
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::error::{DatabaseError, ErrorKind};
    use std::borrow::Cow;
    use std::error::Error;
    use std::sync::Arc;

    #[derive(Debug, thiserror::Error)]
    #[error("{message}")]
    struct ServerError {
        code: &'static str,
        message: &'static str,
    }

    impl DatabaseError for ServerError {
        fn message(&self) -> &str {
            self.message
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.code))
        }
        fn as_error(&self) -> &(dyn Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    #[test]
    fn typed_server_codes_take_precedence_over_error_text() {
        for (backend, code, expected) in [
            (DbBackend::Postgres, "40001", true),
            (DbBackend::Postgres, "40P01", true),
            (DbBackend::Postgres, "55P03", true),
            (DbBackend::Postgres, "57P03", true),
            (DbBackend::Postgres, "08006", true),
            (DbBackend::Postgres, "28P01", false),
            (DbBackend::Postgres, "42601", false),
            (DbBackend::Postgres, "23505", false),
            (DbBackend::Sqlite, "5", true),
            (DbBackend::Sqlite, "517", true),
            (DbBackend::Sqlite, "6", true),
            (DbBackend::Sqlite, "2067", false),
        ] {
            let error = DbErr::Query(RuntimeErr::SqlxError(Arc::new(SqlxError::Database(
                Box::new(ServerError {
                    code,
                    message: "SQL text mentioning deadlock detected (code: 5) database is locked",
                }),
            ))));
            assert_eq!(sea(&error, backend), expected, "{backend:?} code {code}");
        }
    }

    #[test]
    fn only_temporary_transport_errors_are_retried() {
        for (kind, expected) in [
            (std::io::ErrorKind::ConnectionReset, true),
            (std::io::ErrorKind::ConnectionRefused, true),
            (std::io::ErrorKind::TimedOut, true),
            (std::io::ErrorKind::PermissionDenied, false),
            (std::io::ErrorKind::InvalidInput, false),
        ] {
            let error = DbErr::Query(RuntimeErr::SqlxError(Arc::new(SqlxError::Io(kind.into()))));
            assert_eq!(sea(&error, DbBackend::Sqlite), expected);
        }
        assert!(sea(
            &DbErr::ConnectionAcquire(ConnAcquireErr::Timeout),
            DbBackend::Sqlite
        ));
        assert!(!sea(
            &DbErr::ConnectionAcquire(ConnAcquireErr::ConnectionClosed),
            DbBackend::Sqlite
        ));
        assert!(!database(
            &DbError::Sqlx(SqlxError::PoolClosed),
            DbBackend::Sqlite
        ));
    }

    #[test]
    fn contention_is_recognized_but_query_and_constraint_failures_are_not() {
        for (backend, message, expected) in [
            (DbBackend::Sqlite, "(code: 5) database is locked", true),
            (DbBackend::Postgres, "SQLSTATE 40001", true),
            (DbBackend::Postgres, "SQLSTATE 40P01", true),
            (DbBackend::MySql, "1213 (40001): Deadlock found", true),
            (DbBackend::Sqlite, "no such table: operations", false),
            (
                DbBackend::Sqlite,
                "UNIQUE constraint failed: operation.id",
                false,
            ),
            (DbBackend::Postgres, "syntax error near SELECT", false),
        ] {
            let error = DbErr::Query(RuntimeErr::Internal(message.into()));
            assert_eq!(sea(&error, backend), expected, "{message}");
        }
    }

    #[test]
    fn wrapped_scope_failures_keep_their_classification() {
        let temporary = ScopeError::Db(DbErr::ConnectionAcquire(ConnAcquireErr::Timeout));
        assert!(database(&DbError::from(temporary), DbBackend::Sqlite));
        assert!(!database(
            &DbError::from(ScopeError::Denied("scope-secret")),
            DbBackend::Sqlite
        ));
        assert!(!database(
            &DbError::Other(anyhow::anyhow!("connection reset")),
            DbBackend::Sqlite
        ));
    }
}
