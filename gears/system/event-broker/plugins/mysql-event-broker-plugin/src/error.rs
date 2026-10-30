//! `sqlx::Error` -> `BackendError` (`docs/DESIGN.md` §6). A message carries the
//! operation and the server's error number and SQLSTATE, or the kind of
//! failure, only: the server's message and the driver's own text may quote
//! stored values.

use std::fmt;
use std::future::Future;
use std::time::Duration;

use event_broker_sdk::backend::BackendError;
use sqlx::mysql::MySqlDatabaseError;

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

/// `ER_NO_PARTITION_FOR_GIVEN_VALUE`: no segment covers a sequence.
pub const NO_PARTITION: u16 = 1526;

/// `ER_LOCK_DEADLOCK`: chosen as a deadlock victim, by `InnoDB` or by the
/// metadata lock subsystem; the statement or transaction is to be run again.
pub const DEADLOCK: u16 = 1213;

/// Runs of one statement or transaction a deadlock may cost before it is
/// reported.
const DEADLOCK_ATTEMPTS: u32 = 3;

/// Whether `err` chose its statement as a deadlock victim.
fn is_deadlock(err: &sqlx::Error) -> bool {
    number(err) == Some(DEADLOCK)
}

/// Runs `run` again while it is chosen as a deadlock victim, up to
/// [`DEADLOCK_ATTEMPTS`] times in all. A partition DDL takes an exclusive
/// metadata lock, and the server breaks a cycle it closes by failing a
/// statement on the other side, even a plain read.
pub async fn retried<T, F, Fut>(mut run: F) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    let mut attempt = 1;
    loop {
        match run().await {
            Err(err) if is_deadlock(&err) && attempt < DEADLOCK_ATTEMPTS => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(10 * u64::from(attempt))).await;
            }
            outcome => return outcome,
        }
    }
}

/// Server error numbers the medium recovers from on its own.
pub fn is_transient(number: u16) -> bool {
    matches!(
        number,
        1205 // ER_LOCK_WAIT_TIMEOUT (innodb_lock_wait_timeout, lock_wait_timeout)
            | 1213 // ER_LOCK_DEADLOCK
            | 1040 // ER_CON_COUNT_ERROR: too many connections
            | 1053 // ER_SERVER_SHUTDOWN
            | 1317 // ER_QUERY_INTERRUPTED
            | 1927 // MariaDB ER_CONNECTION_KILLED
            | 3024 // MySQL ER_QUERY_TIMEOUT (max_execution_time)
            | 1969 // MariaDB ER_STATEMENT_TIMEOUT (max_statement_time)
            | 1290 // ER_OPTION_PREVENTS_STATEMENT: --read-only, a failover in progress
            | 1836 // ER_READ_ONLY_MODE
            | 1792 // ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION
            | 3032 // ER_SERVER_OFFLINE_MODE
            | 4031 // ER_CLIENT_INTERACTION_TIMEOUT
            | 1203 // ER_TOO_MANY_USER_CONNECTIONS
            | 1226 // ER_USER_LIMIT_REACHED
            | 1180 // ER_ERROR_DURING_COMMIT
            | 3100 // ER_RUN_HOOK_ERROR: a replication hook (Group Replication) refused the commit
            | 3101 // ER_TRANSACTION_ROLLBACK_DURING_COMMIT: certification failed
            | NO_PARTITION
    )
}

/// Server error numbers that mean the credentials are refused.
fn is_authentication(number: u16) -> bool {
    matches!(
        number,
        1044 // ER_DBACCESS_DENIED_ERROR
            | 1045 // ER_ACCESS_DENIED_ERROR
            | 1698 // ER_ACCESS_DENIED_NO_PASSWORD_ERROR
            | 1227 // ER_SPECIFIC_ACCESS_DENIED_ERROR
            | 1142 // ER_TABLEACCESS_DENIED_ERROR
    )
}

/// What went wrong outside the server, named without the driver's text.
pub fn kind(err: &sqlx::Error) -> &'static str {
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

/// The server's error number of `err`, when the server refused a statement.
pub fn number(err: &sqlx::Error) -> Option<u16> {
    match err {
        sqlx::Error::Database(db) => db
            .try_downcast_ref::<MySqlDatabaseError>()
            .map(MySqlDatabaseError::number),
        _ => None,
    }
}

/// Whether `err` is the connection or the server failing rather than a
/// statement being refused.
pub fn is_connectivity(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::Protocol(_)
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::WorkerCrashed
    )
}

/// Maps a failure of `operation`: transient ones and lost connectivity to
/// `Unavailable`, other server errors per [`Operation`], the driver's own to
/// `Internal`.
pub fn map(operation: Operation, err: &sqlx::Error) -> BackendError {
    match err {
        sqlx::Error::Database(db) => {
            let sqlstate = db
                .code()
                .map_or_else(|| "none".to_owned(), std::borrow::Cow::into_owned);
            match number(err) {
                Some(number) => map_server(operation, number, &sqlstate),
                None => {
                    operation.failed(format!("{operation}: database error (SQLSTATE {sqlstate})"))
                }
            }
        }
        err if is_connectivity(err) => {
            BackendError::unavailable(format!("{operation}: {}", kind(err)))
        }
        _ => BackendError::Internal(format!("{operation}: {}", kind(err))),
    }
}

/// Maps the server's refusal `number` (with its `sqlstate`) of `operation`.
pub fn map_server(operation: Operation, number: u16, sqlstate: &str) -> BackendError {
    let reason = format!("{operation}: database error {number} (SQLSTATE {sqlstate})");
    if is_authentication(number) {
        tracing::error!(%operation, number, "mysql backend: access denied");
        return BackendError::unavailable(reason);
    }
    if is_transient(number) || sqlstate.starts_with("08") {
        BackendError::unavailable(reason)
    } else {
        operation.failed(reason)
    }
}
