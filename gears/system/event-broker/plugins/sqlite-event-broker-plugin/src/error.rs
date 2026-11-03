//! `sqlx::Error` -> `BackendError` (`docs/DESIGN.md` §7). A message carries the
//! operation and the `SQLite` result code or the kind of failure only:
//! `SQLite`'s message and the driver's text may quote stored values or the
//! file path.

use std::fmt;

use event_broker_sdk::backend::BackendError;

/// The `Backend` operation a failure happened in: it names the failure, and
/// decides what one the medium does not recover from on its own maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Opening the database file and applying migrations.
    Open,
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
    /// Removing events past a partition's retention bounds.
    Sweep,
}

impl Operation {
    /// The operation's name, as error messages carry it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Provision => "provision",
            Self::Append => "append",
            Self::Read => "read",
            Self::Resolve => "resolve",
            Self::Watch => "watch",
            Self::Query => "query",
            Self::Sweep => "sweep",
        }
    }

    /// `AppendFailed` on the append path, `ReadFailed` on a read path;
    /// the others have no variant of their own.
    fn failed(self, reason: String) -> BackendError {
        match self {
            Self::Append => BackendError::append_failed(reason),
            Self::Read | Self::Resolve | Self::Watch | Self::Query => {
                BackendError::read_failed(reason)
            }
            Self::Open | Self::Provision | Self::Sweep => BackendError::Internal(reason),
        }
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Primary result codes (the low byte of an extended code) a later call may
/// not meet: a lock held elsewhere, a full or failing disk, a file that
/// cannot be opened or written.
fn is_transient(primary: i64) -> bool {
    matches!(
        primary,
        5 // SQLITE_BUSY
            | 6 // SQLITE_LOCKED
            | 7 // SQLITE_NOMEM
            | 8 // SQLITE_READONLY
            | 10 // SQLITE_IOERR
            | 13 // SQLITE_FULL
            | 14 // SQLITE_CANTOPEN
            | 15 // SQLITE_PROTOCOL
    )
}

/// What went wrong outside the database, named without the driver's text.
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

/// Maps a failure of `operation`: transient result codes and a lost pool to
/// `Unavailable`, other database errors per [`Operation`], the driver's own
/// to `Internal`.
pub fn map(operation: Operation, err: &sqlx::Error) -> BackendError {
    match err {
        sqlx::Error::Database(db) => {
            let Some(code) = db.code().and_then(|code| code.parse::<i64>().ok()) else {
                return operation.failed(format!("{operation}: database error (no result code)"));
            };
            let reason = format!("{operation}: database error (SQLite code {code})");
            if is_transient(code & 0xff) {
                BackendError::unavailable(reason)
            } else {
                operation.failed(reason)
            }
        }
        sqlx::Error::Io(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => {
            BackendError::unavailable(format!("{operation}: {}", kind(err)))
        }
        _ => BackendError::Internal(format!("{operation}: {}", kind(err))),
    }
}
