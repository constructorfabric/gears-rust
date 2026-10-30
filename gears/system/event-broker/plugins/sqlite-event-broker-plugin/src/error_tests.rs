use std::borrow::Cow;
use std::error::Error as StdError;
use std::fmt;

use sqlx::error::{DatabaseError, ErrorKind};

use crate::error::{Operation, map};

/// A database error with `code`, whose message quotes a stored value and the
/// file path, as `SQLite`'s messages may.
#[derive(Debug)]
struct Refused(Option<&'static str>);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl StdError for Refused {}

impl DatabaseError for Refused {
    fn message(&self) -> &'static str {
        "CHECK constraint failed: tenant-secret-42 in /srv/secret/events.db"
    }

    fn code(&self) -> Option<Cow<'_, str>> {
        self.0.map(Cow::Borrowed)
    }

    fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
        self
    }

    fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
        self
    }

    fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
        self
    }

    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

fn refused(code: &'static str) -> sqlx::Error {
    sqlx::Error::Database(Box::new(Refused(Some(code))))
}

fn mapped(operation: Operation, err: &sqlx::Error) -> String {
    format!("{:?}", map(operation, err))
}

const EVERY_OPERATION: [Operation; 8] = [
    Operation::Open,
    Operation::Provision,
    Operation::Append,
    Operation::Read,
    Operation::Resolve,
    Operation::Watch,
    Operation::Query,
    Operation::Sweep,
];

#[test]
fn a_database_error_in_an_append_is_append_failed() {
    // SQLITE_CONSTRAINT_CHECK
    assert_eq!(
        mapped(Operation::Append, &refused("275")),
        r#"AppendFailed { reason: "append: database error (SQLite code 275)", detail: "" }"#
    );
}

#[test]
fn a_database_error_in_every_read_path_is_read_failed() {
    let cases = [
        (Operation::Read, "read"),
        (Operation::Resolve, "resolve"),
        (Operation::Watch, "watch"),
        (Operation::Query, "query"),
    ];
    for (operation, name) in cases {
        // SQLITE_CORRUPT
        assert_eq!(
            mapped(operation, &refused("11")),
            format!(
                r#"ReadFailed {{ reason: "{name}: database error (SQLite code 11)", detail: "" }}"#
            )
        );
    }
}

#[test]
fn a_database_error_elsewhere_is_internal() {
    for (operation, name) in [
        (Operation::Open, "open"),
        (Operation::Provision, "provision"),
        (Operation::Sweep, "sweep"),
    ] {
        assert_eq!(
            mapped(operation, &refused("1")),
            format!(r#"Internal("{name}: database error (SQLite code 1)")"#)
        );
    }
}

#[test]
fn transient_result_codes_are_unavailable_in_every_operation() {
    // BUSY, BUSY_SNAPSHOT, LOCKED, NOMEM, READONLY, IOERR_FSYNC, FULL,
    // CANTOPEN, PROTOCOL.
    for code in ["5", "517", "6", "7", "8", "1034", "13", "14", "15"] {
        for operation in EVERY_OPERATION {
            assert_eq!(
                mapped(operation, &refused(code)),
                format!(
                    r#"Unavailable {{ reason: "{operation}: database error (SQLite code {code})", detail: "" }}"#
                )
            );
        }
    }
}

#[test]
fn a_database_error_without_a_code_is_named_as_such() {
    let err = sqlx::Error::Database(Box::new(Refused(None)));
    assert_eq!(
        mapped(Operation::Append, &err),
        r#"AppendFailed { reason: "append: database error (no result code)", detail: "" }"#
    );
}

#[test]
fn a_lost_pool_or_io_is_unavailable() {
    let cases = [
        (sqlx::Error::PoolTimedOut, "pool timed out"),
        (sqlx::Error::PoolClosed, "pool closed"),
        (sqlx::Error::WorkerCrashed, "connection worker crashed"),
        (
            sqlx::Error::Io(std::io::Error::other("/srv/secret/events.db")),
            "i/o error",
        ),
    ];
    for (err, kind) in cases {
        assert_eq!(
            mapped(Operation::Read, &err),
            format!(r#"Unavailable {{ reason: "read: {kind}", detail: "" }}"#)
        );
    }
}

#[test]
fn driver_failures_are_internal() {
    assert_eq!(
        mapped(Operation::Append, &sqlx::Error::RowNotFound),
        r#"Internal("append: no row returned")"#
    );
    assert_eq!(
        mapped(
            Operation::Read,
            &sqlx::Error::ColumnNotFound("tenant-secret-42".to_owned())
        ),
        r#"Internal("read: column not found")"#
    );
}

#[test]
fn no_message_quotes_the_database_or_the_driver() {
    for operation in EVERY_OPERATION {
        for err in [
            refused("275"),
            refused("5"),
            sqlx::Error::Io(std::io::Error::other("tenant-secret-42")),
            sqlx::Error::Protocol("tenant-secret-42".to_owned()),
        ] {
            let text = format!("{:?}", map(operation, &err));
            assert!(
                !text.contains("secret"),
                "{operation} quotes the driver: {text}"
            );
        }
    }
}
