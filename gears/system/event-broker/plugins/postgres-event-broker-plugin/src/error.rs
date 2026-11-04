//! `sqlx::Error` -> `BackendError` (`docs/DESIGN.md` §6). A message carries the
//! operation and the SQLSTATE or the kind of failure only: `PostgreSQL`'s
//! message and `DETAIL`, and the driver's own text, may quote stored values.

use std::fmt;

use event_broker_sdk::backend::BackendError;

/// The `Backend` operation a failure happened in: it names the failure, and
/// decides what one the medium does not recover from on its own maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Creating a topic's partitions and log record.
    Provision,
    /// Writing events under a fence.
    Append,
    /// Reading events from a partition.
    Read,
    /// Turning a requested position into a sequence.
    Resolve,
    /// Reading the current heads a watcher compares against.
    Watch,
    /// Describing what a partition stores.
    Query,
}

impl Operation {
    /// The operation's name, as error messages carry it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Provision => "provision",
            Self::Append => "append",
            Self::Read => "read",
            Self::Resolve => "resolve",
            Self::Watch => "watch",
            Self::Query => "query",
        }
    }

    /// `AppendFailed` on the append path, `ReadFailed` on a read path;
    /// `provision` has no variant of its own.
    fn failed(self, reason: String) -> BackendError {
        match self {
            Self::Append => BackendError::append_failed(reason),
            Self::Read | Self::Resolve | Self::Watch | Self::Query => {
                BackendError::read_failed(reason)
            }
            Self::Provision => BackendError::Internal(reason),
        }
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

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
            | "23514" // check_violation: no segment for a sequence
    ) || code.starts_with("08") // connection exceptions
}

/// What went wrong outside the server, named without the driver's text.
fn kind(err: &sqlx::Error) -> &'static str {
    match err {
        sqlx::Error::Configuration(_) => "configuration error",
        sqlx::Error::InvalidArgument(_) => "invalid argument",
        sqlx::Error::Database(_) => "database error",
        sqlx::Error::Io(_) => "i/o error",
        sqlx::Error::Tls(_) => "tls error",
        sqlx::Error::Protocol(_) => "protocol error",
        sqlx::Error::RowNotFound => "no row returned",
        sqlx::Error::TypeNotFound { .. } => "type not found",
        sqlx::Error::ColumnIndexOutOfBounds { .. } => "column index out of bounds",
        sqlx::Error::ColumnNotFound(_) => "column not found",
        sqlx::Error::ColumnDecode { .. } => "column decode error",
        sqlx::Error::Encode(_) => "encode error",
        sqlx::Error::Decode(_) => "decode error",
        sqlx::Error::PoolTimedOut => "pool timed out",
        sqlx::Error::PoolClosed => "pool closed",
        sqlx::Error::WorkerCrashed => "connection worker crashed",
        sqlx::Error::Migrate(_) => "migration error",
        _ => "driver error",
    }
}

/// Maps a failure of `operation`: transient ones and lost connectivity to
/// `Unavailable`, other server errors per [`Operation`], the driver's own to
/// `Internal`.
pub fn map(operation: Operation, err: &sqlx::Error) -> BackendError {
    match err {
        sqlx::Error::Database(db) => {
            let Some(code) = db.code() else {
                return operation.failed(format!("{operation}: database error (no SQLSTATE)"));
            };
            if code.starts_with("28") {
                tracing::error!(%operation, code = %code, "postgres backend: authentication failed");
                return BackendError::unavailable(format!(
                    "{operation}: authentication failed (SQLSTATE {code})"
                ));
            }
            let reason = format!("{operation}: database error (SQLSTATE {code})");
            if is_transient(&code) {
                BackendError::unavailable(reason)
            } else {
                operation.failed(reason)
            }
        }
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => {
            BackendError::unavailable(format!("{operation}: {}", kind(err)))
        }
        _ => BackendError::Internal(format!("{operation}: {}", kind(err))),
    }
}
