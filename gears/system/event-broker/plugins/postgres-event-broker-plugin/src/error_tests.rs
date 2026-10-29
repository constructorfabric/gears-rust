use std::borrow::Cow;
use std::error::Error as StdError;
use std::fmt;

use sqlx::error::{DatabaseError, ErrorKind};

use crate::error::{Operation, map};

/// A server error with `code`, whose message quotes a stored value, as
/// `PostgreSQL`'s messages may.
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
        "invalid input value for enum: \"tenant-secret-42\""
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

#[test]
fn a_database_error_in_an_append_is_append_failed() {
    assert_eq!(
        mapped(Operation::Append, &refused("22P02")),
        r#"AppendFailed { reason: "append: database error (SQLSTATE 22P02)", detail: "" }"#
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
        assert_eq!(
            mapped(operation, &refused("42P01")),
            format!(
                r#"ReadFailed {{ reason: "{name}: database error (SQLSTATE 42P01)", detail: "" }}"#
            )
        );
    }
}

#[test]
fn a_database_error_in_provision_is_internal() {
    assert_eq!(
        mapped(Operation::Provision, &refused("42501")),
        r#"Internal("provision: database error (SQLSTATE 42501)")"#
    );
}

#[test]
fn errors_the_medium_recovers_from_are_unavailable_in_every_operation() {
    for code in [
        "55P03", "57014", "40001", "40P01", "53300", "57P01", "57P02", "57P03", "23514", "08006",
    ] {
        assert_eq!(
            mapped(Operation::Append, &refused(code)),
            format!(
                r#"Unavailable {{ reason: "append: database error (SQLSTATE {code})", detail: "" }}"#
            )
        );
        assert_eq!(
            mapped(Operation::Read, &refused(code)),
            format!(
                r#"Unavailable {{ reason: "read: database error (SQLSTATE {code})", detail: "" }}"#
            )
        );
    }
}

#[test]
fn an_authentication_failure_is_unavailable() {
    assert_eq!(
        mapped(Operation::Append, &refused("28P01")),
        r#"Unavailable { reason: "append: authentication failed (SQLSTATE 28P01)", detail: "" }"#
    );
}

#[test]
fn a_database_error_without_a_code_names_none() {
    assert_eq!(
        mapped(
            Operation::Read,
            &sqlx::Error::Database(Box::new(Refused(None)))
        ),
        r#"ReadFailed { reason: "read: database error (no SQLSTATE)", detail: "" }"#
    );
}

#[test]
fn connectivity_failures_are_unavailable_by_kind_only() {
    let cases = [
        (
            sqlx::Error::Io(std::io::Error::other("10.0.0.7:5432 secret")),
            "i/o error",
        ),
        (sqlx::Error::Tls("secret".into()), "tls error"),
        (sqlx::Error::Protocol("secret".to_owned()), "protocol error"),
        (sqlx::Error::PoolTimedOut, "pool timed out"),
        (sqlx::Error::PoolClosed, "pool closed"),
        (sqlx::Error::WorkerCrashed, "connection worker crashed"),
    ];
    for (err, kind) in cases {
        assert_eq!(
            mapped(Operation::Append, &err),
            format!(r#"Unavailable {{ reason: "append: {kind}", detail: "" }}"#)
        );
    }
}

#[test]
fn a_driver_failure_is_internal_by_kind_only() {
    let cases = [
        (sqlx::Error::RowNotFound, "no row returned"),
        (
            sqlx::Error::ColumnNotFound("secret".to_owned()),
            "column not found",
        ),
        (sqlx::Error::Decode("secret".into()), "decode error"),
        (sqlx::Error::Encode("secret".into()), "encode error"),
        (
            sqlx::Error::Configuration("secret".into()),
            "configuration error",
        ),
    ];
    for (err, kind) in cases {
        assert_eq!(
            mapped(Operation::Read, &err),
            format!(r#"Internal("read: {kind}")"#)
        );
    }
}
