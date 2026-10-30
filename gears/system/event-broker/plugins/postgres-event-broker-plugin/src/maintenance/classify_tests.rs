use std::borrow::Cow;
use std::error::Error as StdError;
use std::fmt;
use std::time::Duration;

use sqlx::error::{DatabaseError, ErrorKind};

use crate::maintenance::classify::{
    Class, Loudness, MediumDown, class, class_of_sqlstate, lock_timeout_for,
};

/// A server error with `code`, as the driver would report it.
#[derive(Debug)]
struct Refused(Option<&'static str>);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("refused")
    }
}

impl StdError for Refused {}

impl DatabaseError for Refused {
    fn message(&self) -> &'static str {
        "refused"
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

fn io() -> sqlx::Error {
    sqlx::Error::Io(std::io::Error::other("connection reset"))
}

#[test]
fn every_sqlstate_has_its_class() {
    let codes = [
        "08006", "08P01", "57P01", "57P02", "57P03", "53300", "25006", "55P03", "40P01", "40001",
        "57014", "25P03", "53200", "54001", "53100", "42P01", "42P07", "42P17", "23514", "42501",
        "3F000", "3D000", "28P01", "28000", "22012",
    ];
    let failed = Class::Failed;
    assert_eq!(
        codes.map(|code| (code, class_of_sqlstate(code))),
        [
            ("08006", Class::Medium),
            ("08P01", Class::Medium),
            ("57P01", Class::Medium),
            ("57P02", Class::Medium),
            ("57P03", Class::Medium),
            ("53300", Class::Medium),
            ("25006", Class::Medium),
            ("55P03", Class::Contended),
            ("40P01", Class::Contended),
            ("40001", Class::Contended),
            ("57014", failed(Loudness::Quiet)),
            ("25P03", failed(Loudness::Warn)),
            ("53200", failed(Loudness::Warn)),
            ("54001", failed(Loudness::Warn)),
            ("53100", failed(Loudness::Error)),
            ("42P01", Class::Gone),
            ("42P07", Class::Stale),
            ("42P17", Class::Stale),
            ("23514", Class::Bug),
            ("42501", Class::Permanent),
            ("3F000", Class::Permanent),
            ("3D000", Class::Permanent),
            ("28P01", Class::Permanent),
            ("28000", Class::Permanent),
            ("22012", failed(Loudness::Warn)),
        ]
    );
}

#[test]
fn every_error_kind_has_its_class() {
    let errors = [
        io(),
        sqlx::Error::Tls(Box::new(std::io::Error::other("handshake"))),
        sqlx::Error::Protocol("unexpected message".to_owned()),
        sqlx::Error::PoolTimedOut,
        sqlx::Error::PoolClosed,
        sqlx::Error::WorkerCrashed,
        sqlx::Error::RowNotFound,
        refused("40P01"),
        sqlx::Error::Database(Box::new(Refused(None))),
    ];
    assert_eq!(
        errors.iter().map(|err| class(err, 0)).collect::<Vec<_>>(),
        [
            Class::Medium,
            Class::Medium,
            Class::Medium,
            Class::Medium,
            Class::Medium,
            Class::Medium,
            Class::Failed(Loudness::Warn),
            Class::Contended,
            Class::Failed(Loudness::Warn),
        ]
    );
}

#[test]
fn a_stale_catalog_is_retried_once_then_a_bug() {
    let errors = [refused("42P07"), refused("42P17"), refused("55P03")];
    assert_eq!(
        errors.map(|err| (class(&err, 0), class(&err, 1), class(&err, 7))),
        [
            (Class::Stale, Class::Bug, Class::Bug),
            (Class::Stale, Class::Bug, Class::Bug),
            (Class::Contended, Class::Contended, Class::Contended),
        ]
    );
}

#[test]
fn a_medium_fault_names_the_job() {
    assert_eq!(
        MediumDown::new("extender", &io()).to_string(),
        "extender: the database is unavailable: error communicating with database: connection reset"
    );
}

#[test]
fn lock_timeouts_grow_with_the_attempt() {
    assert_eq!(
        [0, 1, 2, 3, 9].map(lock_timeout_for),
        [
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(5),
            Duration::from_secs(5),
        ]
    );
}
