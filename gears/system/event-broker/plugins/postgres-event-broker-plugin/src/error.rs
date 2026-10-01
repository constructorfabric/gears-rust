//! `sqlx::Error` -> `BackendError` (`docs/DESIGN.md` §6). A message carries the
//! operation and `PostgreSQL`'s error code or message, never a row's `DETAIL`,
//! where values would appear.

use event_broker_sdk::backend::BackendError;

/// SQLSTATE classes and codes the medium recovers from on its own.
fn is_transient(code: &str) -> bool {
    matches!(
        code,
        "55P03" // lock_not_available (lock_timeout)
            | "57014" // query_canceled (statement or idle-in-transaction timeout)
            | "40001" // serialization_failure
            | "40P01" // deadlock_detected
            | "53300" // too_many_connections
            | "57P01" | "57P02" | "57P03" // admin shutdown, crash shutdown, cannot connect now
            | "23514" // check_violation: no segment for a sequence (maintenance is behind, §4.9)
    ) || code.starts_with("08") // connection exceptions
}

pub fn map(operation: &str, err: &sqlx::Error) -> BackendError {
    match err {
        sqlx::Error::Database(db) => {
            let code = db
                .code()
                .map(std::borrow::Cow::into_owned)
                .unwrap_or_default();
            if code.starts_with("28") {
                tracing::error!(operation, code = %code, "postgres backend: authentication failed");
                return BackendError::unavailable(format!(
                    "{operation}: authentication failed ({code})"
                ));
            }
            if is_transient(&code) {
                return BackendError::unavailable(format!(
                    "{operation}: {} ({code})",
                    db.message()
                ));
            }
            BackendError::Internal(format!("{operation}: {} ({code})", db.message()))
        }
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => BackendError::unavailable(format!("{operation}: {err}")),
        _ => BackendError::Internal(format!("{operation}: {err}")),
    }
}
