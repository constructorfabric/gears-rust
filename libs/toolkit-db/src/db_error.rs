//! What the database said about a refusal, structurally.
//!
//! Three ways to tell why a write was refused existed before this module, and
//! none of them was both complete and stable (issue #4645):
//!
//! * [`sea_orm::DbErr::sql_err`] is typed and portable, but its table is
//!   incomplete — `sea-orm` 2.0 maps `23505` and `23503` for `PostgreSQL` and
//!   has no `Check` discriminant at all, so a `PostgreSQL` 18 `RESTRICT`
//!   refusal (`23001`) returns `None`.
//! * Matching `DbErr`'s message text works until the driver rewords it or the
//!   server is asked to speak another language. `PostgreSQL` 18 did reword the
//!   `RESTRICT` message, which is the kind of change that path cannot survive.
//! * Reading the driver error directly is correct, and used to require a gear
//!   to depend on `sqlx` itself — which the architecture lints forbid, and
//!   which is why the classification ended up copied into gears.
//!
//! This module is the third one, without the dependency: the accessor returns
//! owned strings, so **no `sqlx` type appears in its signature** and a gear
//! that classifies never links the driver.
//!
//! # What belongs here and what does not
//!
//! A SQLSTATE names a *condition*: "a unique constraint was violated". What
//! that condition means for a caller — `409 Conflict`, a domain error, a retry
//! — is the gear's decision (`docs/arch/errors/ADR/0004`, which delegates
//! fine-grained mapping to gears). So this module classifies conditions and
//! stops there.
//!
//! Which leaves the question of where the line falls, since naming any
//! condition at all is itself a choice. The test is **whether a caller in this
//! workspace branches on the distinction**:
//!
//! * A code earns a variant when some caller already acts differently on it.
//!   [`ConstraintViolation`] is `#[non_exhaustive]` so that the next one can
//!   be added the day a caller needs it, rather than in advance. Class 23 has
//!   more codes than the two named here, and they stay unnamed until then.
//! * Two codes collapse into one variant when no caller would branch between
//!   them. `23503` and `23001` differ in *when* the foreign key was checked,
//!   not in what the caller must now do: rows still reference this one.
//! * A distinction nobody branches on is not discarded, only left unnamed —
//!   [`DriverRefusal::code`] still returns the code verbatim, so a caller that
//!   does need to tell immediate `RESTRICT` from deferrable `NO ACTION` can,
//!   without this module having to guess on its behalf.
//!
//! # How a renumbering is caught
//!
//! The tables here name codes, and a server can renumber a condition out from
//! under them — which is what issue #4645 was. Nothing in this module reports a
//! code it fails to recognise, and that is deliberate: [`violation_of`] answers
//! `None` for every error that is not a constraint violation, so "a code no
//! table names" is the ordinary case rather than an anomaly. Narrowing the
//! alarm to Class 23 does not rescue it, because the codes this module
//! deliberately leaves unnamed — `23502`, `23514`, `23P01` — are all in it: the
//! alarm would have to be told which unmapped codes are expected, which is the
//! very table whose staleness it was meant to watch.
//!
//! What catches a renumbering is a test against a live server. Every condition
//! named here is provoked on every backend that supports it, and
//! `pg_restrict_delete_is_classified_as_foreign_key_violation` in
//! `tests/error_classification.rs` is the one `PostgreSQL` 18 would have
//! failed. `cargo xtask check-test-container-pins` holds those servers at
//! pinned images, so the next major version arrives as a deliberate pin bump
//! with CI attached, not as a production symptom.
//!
//! The operational half belongs to the caller, not here: these are pure
//! predicates with no backend, tenant or span to log against, while a gear's
//! classification boundary has all three and already logs the branch an
//! unmapped code falls into.

use std::borrow::Cow;

use sea_orm::DbErr;

/// The condition a SQLSTATE names, for the codes this platform acts on.
///
/// `#[non_exhaustive]`: the table grows as the platform starts acting on more
/// conditions, and that must not be a breaking change for a `match` in a gear.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ConstraintViolation {
    /// A unique index or constraint rejected the row.
    Unique,
    /// A foreign key rejected the row, or refused to leave one orphaned.
    ///
    /// Both `23503` (`foreign_key_violation`) and `23001`
    /// (`restrict_violation`) land here. They are genuinely different
    /// constraint actions — `RESTRICT` is checked immediately and cannot be
    /// deferred, `NO ACTION` can — and `PostgreSQL` 18 started reporting the
    /// first one under its own standard code. For a caller the condition is
    /// one and the same: rows still reference this one.
    ForeignKey,
}

/// The code the driver reported for a refusal, and the constraint name when it
/// named one.
///
/// Owned rather than borrowed so the type carries nothing from the driver: the
/// point of this module is that a gear can read a refusal without depending on
/// `sqlx`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverRefusal {
    code: String,
    constraint: Option<String>,
}

impl DriverRefusal {
    /// Build one, for a test of a mapping that consumes it.
    ///
    /// The fields are private, so without this the only source of a
    /// `DriverRefusal` is a `DbErr` from a live driver. A gear that factors its
    /// mapping as `fn map(refusal: &DriverRefusal) -> MyError` could then not
    /// unit-test that function at all: ten lines of `match` would need a
    /// container to exercise.
    ///
    /// Takes the code alone, because that is what every refusal has; add the
    /// constraint name with [`with_constraint`](Self::with_constraint) when the
    /// mapping under test reads it.
    ///
    /// ```rust,ignore
    /// let refusal = DriverRefusal::new("23503").with_constraint("orders_customer_fk");
    /// assert_eq!(map(&refusal), MyError::CustomerStillHasOrders);
    /// ```
    #[must_use]
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            constraint: None,
        }
    }

    /// Name the constraint, as the server would have.
    #[must_use]
    pub fn with_constraint(mut self, constraint: impl Into<String>) -> Self {
        self.constraint = Some(constraint.into());
        self
    }

    /// The code the driver reported, verbatim.
    ///
    /// On `PostgreSQL` and `MySQL` this is the five-character SQLSTATE. On
    /// `SQLite` it is the extended result code (`2067` for a unique
    /// violation), which is not a SQLSTATE and which
    /// [`constraint_violation`] therefore names no condition for — the
    /// accessor is called `code` rather than `sqlstate` for that reason.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The constraint the server named, when it named one.
    ///
    /// This is what tells two constraints on one table apart — which of two
    /// foreign keys refused, which unique index was hit — and no message-text
    /// classifier can do it reliably.
    #[must_use]
    pub fn constraint(&self) -> Option<&str> {
        self.constraint.as_deref()
    }

    /// The condition this code names, if it is one the table covers.
    #[must_use]
    pub fn violation(&self) -> Option<ConstraintViolation> {
        constraint_violation(&self.code)
    }
}

/// The condition a `PostgreSQL` SQLSTATE names.
///
/// Class 23 (`integrity_constraint_violation`) only, because that is the class
/// a caller can act on. Deliberate omissions:
///
/// * `23000` — `PostgreSQL` never sends it, and `MySQL` sends it for unique
///   *and* foreign-key violations alike, so it names no single condition. A
///   `MySQL` caller needs the vendor error number, which is what
///   [`sea_orm::DbErr::sql_err`] already reads.
/// * `SQLite` extended codes are not SQLSTATEs and are not in this table;
///   [`is_unique_violation`] and [`is_foreign_key_violation`] below remain the
///   portable path, because `SeaORM`'s own `sql_err()` does read them.
#[must_use]
pub fn constraint_violation(sqlstate: &str) -> Option<ConstraintViolation> {
    match sqlstate {
        "23505" => Some(ConstraintViolation::Unique),
        // One condition, two codes. See `ConstraintViolation::ForeignKey`.
        "23503" | "23001" => Some(ConstraintViolation::ForeignKey),
        _ => None,
    }
}

/// The driver's code and constraint name for a statement the database refused.
///
/// # What `None` means, exactly
///
/// The error carries no refusal this function will read. Four ways to get
/// there, and the last one is a limitation rather than a state:
///
/// * the error came from executing nothing — `SeaORM` produced it itself
///   (`DbErr::Custom`, `DbErr::RecordNotFound`, a type conversion);
/// * the error is a **connection** failure. Deliberately excluded even when it
///   carries a code of its own: a connect-time `28P01` or `53300` is not a
///   statement being refused, and a caller reading `constraint()` on it would
///   answer a transient or auth condition as though a constraint had spoken.
///   Retry decisions belong to [`crate::contention`], not here;
/// * this crate was built with no database driver, which also means nothing
///   could have produced a driver error;
/// * the driver reported a refusal but gave no code at all. `sqlx` allows it
///   in principle, and then this is indistinguishable from the cases above.
///   No backend the workspace supports does it — `PostgreSQL` and `MySQL`
///   always send a SQLSTATE, `SQLite` always an extended result code — so the
///   code stays a `String` rather than costing every caller an extra `Option`
///   for a state none of them can observe.
///
/// `Some` means one statement was refused and the driver said why. Whether the
/// reason is one this platform acts on is [`DriverRefusal::violation`]'s
/// answer, not this one's: a code outside class 23 (a syntax error, say) still
/// arrives as `Some` with `violation() == None`.
///
/// # Example
///
/// ```rust,ignore
/// use toolkit_db::db_error::{ConstraintViolation, driver_refusal};
///
/// match store.delete(id).await {
///     Err(err) => {
///         let refused_by_a_reference = driver_refusal(&err)
///             .and_then(|refusal| refusal.violation())
///             == Some(ConstraintViolation::ForeignKey);
///         // ... map that to this gear's own error
///     }
///     Ok(()) => {}
/// }
/// ```
#[must_use]
pub fn driver_refusal(err: &DbErr) -> Option<DriverRefusal> {
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    {
        let db_err = database_error(err)?;
        Some(DriverRefusal {
            code: db_err.code()?.into_owned(),
            constraint: db_err.constraint().map(ToOwned::to_owned),
        })
    }
    #[cfg(not(any(feature = "pg", feature = "mysql", feature = "sqlite")))]
    {
        let _ = err;
        None
    }
}

/// The driver error behind a refused statement, if this error is one.
///
/// The shape every reader in this module agrees on, written once. Exactly what
/// [`sea_orm::DbErr::sql_err`] reads, and for the same reason: `Exec` and
/// `Query` are a statement that ran and was refused. `DbErr::Conn` is not one
/// -- see [`driver_refusal`] for why a connect-time code must not surface here.
/// `a_connect_time_refusal_carries_no_driver_refusal` in
/// `tests/error_classification.rs` holds that exclusion against a live server,
/// which does answer a bad password with a SQLSTATE of its own.
#[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
fn database_error(err: &DbErr) -> Option<&(dyn sqlx::error::DatabaseError + 'static)> {
    let (DbErr::Exec(sea_orm::RuntimeErr::SqlxError(sqlx_err))
    | DbErr::Query(sea_orm::RuntimeErr::SqlxError(sqlx_err))) = err
    else {
        return None;
    };
    let sqlx::Error::Database(db_err) = &**sqlx_err else {
        return None;
    };
    Some(&**db_err)
}

/// The code the driver reported, borrowed rather than copied.
///
/// The primitive [`driver_refusal`] and [`violation_of`] are both built on, and
/// the one to reach for when the constraint name is not wanted: it allocates
/// nothing, where [`driver_refusal`] copies the code and the constraint name
/// into owned `String`s whether the caller reads them or not.
///
/// `Cow` keeps the signature free of `sqlx` exactly as the owned `String` did
/// -- it is a `std` type, and a gear that calls this still never links the
/// driver.
///
/// `None` means the same four things it means for [`driver_refusal`]; see there.
#[must_use]
pub fn driver_code(err: &DbErr) -> Option<Cow<'_, str>> {
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    {
        database_error(err)?.code()
    }
    #[cfg(not(any(feature = "pg", feature = "mysql", feature = "sqlite")))]
    {
        let _ = err;
        None
    }
}

/// The condition the driver named for this error, if it named one.
///
/// [`driver_refusal`] then [`DriverRefusal::violation`] in one step, without
/// building the `DriverRefusal`. This is the whole question for a caller that
/// only wants to know *what kind* of refusal it was.
///
/// `None` covers two different situations, which
/// [`driver_code`] tells apart when that matters: there was no driver refusal
/// to read, or the driver reported a code this table names no condition for (a
/// `SQLite` extended code, or any SQLSTATE outside class 23).
#[must_use]
pub fn violation_of(err: &DbErr) -> Option<ConstraintViolation> {
    constraint_violation(&driver_code(err)?)
}

/// The shared shape of both classifiers below: three tiers, and which of them
/// is allowed to answer.
///
/// # Why three tiers, and what would retire each
///
/// The two older tiers are not a transitional shim waiting on more coverage in
/// [this module](self); each answers a case the SQLSTATE cannot, and each has a
/// condition under which it goes away:
///
/// * `sql_err()` reads `MySQL`'s vendor error number. `MySQL` reports both a
///   duplicate key and a failed foreign key as `23000`, so the SQLSTATE alone
///   cannot tell the two conditions apart — only the vendor number can. It also
///   carries `SQLite`, whose extended result codes are not SQLSTATEs and which
///   [this module](self) therefore names no condition for. This tier retires
///   when [this module](self) reads both itself.
/// * The message match catches errors re-wrapped as [`DbErr::Custom`] on the
///   way here, which have no driver error left to read at all. It retires when
///   no call site can hand these classifiers a re-wrapped error — a property of
///   the callers, not of this module.
///
/// # Why the driver gets the last word, not just the first
///
/// The tiers are ordered by how much they assume, and a tier may only speak
/// when the ones above it had nothing to say — *including when what they had to
/// say was "no"*.
///
/// The message tier is the reason this matters. `PostgreSQL` echoes the
/// offending value into its message text, so a caller can put our own search
/// strings there: `invalid input syntax for type uuid: "duplicate key"` is a
/// `22P02`, a malformed input, and it used to classify as a unique violation
/// because `22P02` names no condition and the fall-through reached the text.
/// A gear answers `409 Conflict` to what is a `400`, and one that treats a
/// conflict as "it already exists, return that one" takes a branch the caller
/// chose for it.
///
/// So: a code that names a condition is the final answer, yes or no. A code
/// that names none (a `SQLite` extended code, a SQLSTATE outside class 23)
/// still leaves `sql_err()` its turn. And the text is read only when no driver
/// spoke at all, which is exactly the re-wrapped case it documents.
///
/// [`DbErr::Custom`]: sea_orm::DbErr::Custom
fn classifies_as(
    err: &sea_orm::DbErr,
    violation: ConstraintViolation,
    sea_orm_says: impl FnOnce() -> bool,
    message_says: impl FnOnce(&str) -> bool,
) -> bool {
    // The driver named a condition: that is the answer, either way.
    if let Some(named) = violation_of(err) {
        return named == violation;
    }

    if sea_orm_says() {
        return true;
    }

    // A driver spoke and neither tier above recognised what it said. Guessing
    // from text here is what let caller-supplied content decide the answer.
    if driver_code(err).is_some() {
        return false;
    }

    message_says(&err.to_string().to_lowercase())
}

/// Check whether a `sea_orm::DbErr` represents a unique-constraint violation.
///
/// Three paths, in order of how much they assume: the SQLSTATE the driver
/// reported ([this module](self)), then `SeaORM`'s own `sql_err()`
/// classification, then a match on the message text for errors that were
/// re-wrapped on the way here and lost their typed shape. See
/// [`classifies_as`] for which of them is allowed to answer when.
///
/// Recognized patterns across backends:
/// - **Postgres** SQLSTATE `23505` — "`unique_violation`" / "duplicate key"
/// - **`SQLite`** extended code `2067` — "UNIQUE constraint failed"
/// - **`MySQL`** error `1062` — "Duplicate entry"
#[must_use]
pub fn is_unique_violation(err: &sea_orm::DbErr) -> bool {
    classifies_as(
        err,
        ConstraintViolation::Unique,
        || {
            matches!(
                err.sql_err(),
                Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
            )
        },
        |msg| {
            msg.contains("unique constraint")
                || msg.contains("duplicate key")
                || msg.contains("unique_violation")
                || msg.contains("duplicate entry")
                || msg.contains("unique constraint failed")
        },
    )
}

/// Check whether a `sea_orm::DbErr` represents a foreign-key violation.
///
/// The counterpart of [`is_unique_violation`], detected the same three ways.
///
/// Useful where a referencing row is the invariant and the `RESTRICT` on the
/// foreign key is what actually enforces it -- a preceding count is a nicer
/// message, not the guard, and under concurrency the constraint is what
/// answers.
///
/// `RESTRICT` is why the structured path leads. `PostgreSQL` 18 reports such a
/// refusal as `23001` (`restrict_violation`) where 17 and earlier reported
/// `23503`, and `sea-orm` 2.0 maps only the latter — so `sql_err()` returns
/// `None` for it, and what recognised it here was the *message* still
/// containing "foreign key constraint" after 18 reworded it (issue #4645).
/// Both codes name one condition in [this module](self).
///
/// Recognized patterns across backends:
/// - **Postgres** SQLSTATE `23503` / `23001` — "`foreign_key_violation`" /
///   "violates foreign key constraint" / "violates RESTRICT setting of"
/// - **`SQLite`** extended code `787` (`SQLITE_CONSTRAINT_FOREIGNKEY`) —
///   "FOREIGN KEY constraint failed"
/// - **`MySQL`** errors `1451`/`1452` — "a foreign key constraint fails"
#[must_use]
pub fn is_foreign_key_violation(err: &sea_orm::DbErr) -> bool {
    classifies_as(
        err,
        ConstraintViolation::ForeignKey,
        || {
            matches!(
                err.sql_err(),
                Some(sea_orm::SqlErr::ForeignKeyConstraintViolation(_))
            )
        },
        |msg| {
            msg.contains("foreign key constraint")
                || msg.contains("foreign_key_violation")
                || msg.contains("violates foreign key")
                // PostgreSQL 18's RESTRICT wording, for an error that reached
                // here stripped of its SQLSTATE.
                || msg.contains("violates restrict setting")
        },
    )
}

#[cfg(test)]
/// A `DbErr` shaped exactly like one a driver produces, with a code and a
/// message of our choosing.
///
/// The tier rules cannot be exercised through `DbErr::Custom`, which by
/// definition carries no driver error: what has to be tested is a *live*
/// shape whose code says one thing and whose text says another. A live
/// server can produce it (and does, in `tests/error_classification.rs`),
/// but the rule itself deserves a test that runs without one.
#[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
pub(crate) mod driver_shaped {
    use std::borrow::Cow;
    use std::sync::Arc;

    #[derive(Debug)]
    struct Refusal {
        code: Option<&'static str>,
        message: String,
    }

    impl std::fmt::Display for Refusal {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.message)
        }
    }

    impl std::error::Error for Refusal {}

    impl sqlx::error::DatabaseError for Refusal {
        fn message(&self) -> &str {
            &self.message
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            self.code.map(Cow::Borrowed)
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        /// `SeaORM`'s `sql_err()` classifies by downcasting to a concrete
        /// driver type, not by this, so the kind is never the deciding factor
        /// for anything a test double can reach.
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    pub fn refused(code: &'static str, message: &str) -> sea_orm::DbErr {
        build(Some(code), message)
    }

    /// A refusal the driver reported without any code -- the fourth `None`
    /// case `driver_refusal` documents, which no supported backend produces.
    pub fn refused_without_a_code(message: &str) -> sea_orm::DbErr {
        build(None, message)
    }

    fn build(code: Option<&'static str>, message: &str) -> sea_orm::DbErr {
        sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(Arc::new(
            sqlx::Error::Database(Box::new(Refusal {
                code,
                message: message.to_owned(),
            })),
        )))
    }

    /// A driver error that is not a database refusal at all.
    pub fn not_a_refusal() -> sea_orm::DbErr {
        sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(Arc::new(
            sqlx::Error::RowNotFound,
        )))
    }

    /// The double is load-bearing for every test that uses it, so it is checked
    /// like anything else: a double that reports the wrong thing would weaken
    /// its callers silently.
    #[cfg(test)]
    mod self_check {
        use sqlx::error::DatabaseError as _;

        fn database_error_of(err: &sea_orm::DbErr) -> &(dyn sqlx::error::DatabaseError + 'static) {
            let (sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(e))
            | sea_orm::DbErr::Query(sea_orm::RuntimeErr::SqlxError(e))) = err
            else {
                panic!("the double must build a driver error");
            };
            let sqlx::Error::Database(db) = &**e else {
                panic!("the double must build a database error");
            };
            &**db
        }

        #[test]
        fn it_reports_what_it_was_built_with() {
            let err = super::refused("23505", "duplicate key");
            let db = database_error_of(&err);
            assert_eq!(db.message(), "duplicate key");
            assert_eq!(db.code().as_deref(), Some("23505"));
            assert_eq!(db.kind(), sqlx::error::ErrorKind::Other);
            assert!(db.as_error().to_string().contains("duplicate key"));

            let err = super::refused_without_a_code("no code here");
            assert!(database_error_of(&err).code().is_none());
        }

        #[test]
        fn it_can_be_taken_apart_the_way_sqlx_takes_errors_apart() {
            let mut refusal = super::Refusal {
                code: Some("23505"),
                message: "duplicate key".to_owned(),
            };
            assert!(refusal.as_error_mut().to_string().contains("duplicate key"));
            assert!(
                Box::new(refusal)
                    .into_error()
                    .to_string()
                    .contains("duplicate key")
            );
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{
        ConstraintViolation, DriverRefusal, constraint_violation, driver_refusal,
        is_foreign_key_violation, is_unique_violation,
    };
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    use super::{driver_code, driver_shaped, violation_of};
    use sea_orm::DbErr;

    /// The point of the table: one condition, both codes. A `RESTRICT` refusal
    /// on `PostgreSQL` 18 and a `NO ACTION` refusal on any major must classify
    /// the same, or a gear answers 500 on one of them.
    #[test]
    fn restrict_and_foreign_key_are_one_condition() {
        assert_eq!(
            constraint_violation("23001"),
            Some(ConstraintViolation::ForeignKey)
        );
        assert_eq!(
            constraint_violation("23503"),
            Some(ConstraintViolation::ForeignKey)
        );
    }

    /// `driver_refusal` on the shape it is written for: a refused statement.
    ///
    /// The owned accessor, unlike `driver_code`, also carries the constraint
    /// name, and most refusals name none.
    #[test]
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    fn a_refused_statement_reaches_the_owned_accessor() {
        let err = driver_shaped::refused("23505", "duplicate key value violates unique constraint");
        let refusal = driver_refusal(&err).expect("a refused statement carries a refusal");
        assert_eq!(refusal.code(), "23505");
        assert_eq!(refusal.constraint(), None);
        assert_eq!(refusal.violation(), Some(ConstraintViolation::Unique));
    }

    /// A driver error that is not a database refusal reads as no refusal, which
    /// is the other early return in `database_error`.
    #[test]
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    fn a_driver_error_that_is_not_a_refusal_carries_none() {
        let err = driver_shaped::not_a_refusal();
        assert!(driver_refusal(&err).is_none());
        assert!(driver_code(&err).is_none());
        assert!(violation_of(&err).is_none());
    }

    /// The fourth documented `None`: the driver refused but named no code.
    ///
    /// No backend the workspace supports does it, which is why the code is a
    /// `String` rather than an `Option<String>` -- but the contract says this
    /// is indistinguishable from the other three, and that is asserted here
    /// rather than only described.
    #[test]
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    fn a_refusal_without_a_code_reads_as_no_refusal() {
        let err = driver_shaped::refused_without_a_code("refused, but said nothing");
        assert!(driver_refusal(&err).is_none());
        assert!(driver_code(&err).is_none());
        assert!(violation_of(&err).is_none());
    }

    /// A code this table names no condition for does not reach the text, even
    /// when the text would have matched.
    ///
    /// The tier below is `sql_err()`, and it cannot be reached from here:
    /// `SeaORM` classifies by `try_downcast_ref` to a concrete driver type
    /// (`SqliteError`, `PgDatabaseError`, `MySqlDatabaseError`), which a test
    /// double is not. So this asserts the part that is testable without a
    /// driver -- that the message is not consulted -- and the real `SQLite`
    /// path, where `sql_err()` does answer for `2067`, is covered by the
    /// sqlite lane in `tests/error_classification.rs`.
    #[test]
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    fn a_code_that_names_nothing_does_not_fall_through_to_the_text() {
        let err = driver_shaped::refused("2067", "UNIQUE constraint failed: users.email");
        assert_eq!(
            violation_of(&err),
            None,
            "the table names no condition for it"
        );
        assert!(
            !is_unique_violation(&err),
            "and with a driver code in hand the message is not evidence"
        );
    }

    /// The constructor exists so a gear can test its own mapping; this is that
    /// use, in miniature.
    #[test]
    fn a_refusal_can_be_built_for_a_test() {
        let refusal = DriverRefusal::new("23503").with_constraint("orders_customer_fk");
        assert_eq!(refusal.code(), "23503");
        assert_eq!(refusal.constraint(), Some("orders_customer_fk"));
        assert_eq!(refusal.violation(), Some(ConstraintViolation::ForeignKey));

        // Most refusals name no constraint, and that is the default.
        assert_eq!(DriverRefusal::new("23505").constraint(), None);
    }

    #[test]
    fn a_duplicate_key_is_named() {
        assert_eq!(
            constraint_violation("23505"),
            Some(ConstraintViolation::Unique)
        );
    }

    /// The rest of class 23 is unnamed on purpose, by this module's own rule:
    /// nothing in the workspace branches on a `CHECK`, `NOT NULL` or
    /// `EXCLUDE` refusal. The code still reaches a caller through
    /// [`DriverRefusal::code`], and the enum is `#[non_exhaustive]` so naming
    /// one the day something does branch on it is not a breaking change.
    #[test]
    fn the_class_23_codes_no_caller_branches_on_stay_unnamed() {
        for unnamed in ["23514", "23502", "23P01"] {
            assert_eq!(constraint_violation(unnamed), None, "{unnamed}");
        }
    }

    /// `23000` is `MySQL`'s answer for unique *and* foreign-key violations, so
    /// naming a condition for it would be a guess. Unknown codes are `None`,
    /// which every caller reads as "classify this some other way".
    #[test]
    fn an_ambiguous_or_unknown_code_names_no_condition() {
        assert_eq!(constraint_violation("23000"), None);
        assert_eq!(constraint_violation("40001"), None);
        assert_eq!(constraint_violation(""), None);
    }

    /// A refusal from a real driver, reached end to end, with no server to
    /// start: `SQLite` in memory is enough to exercise the extraction path and
    /// all three accessors.
    ///
    /// It also pins the documented `SQLite` behaviour rather than assuming it:
    /// the driver reports an extended result code (`2067`), not a SQLSTATE, and
    /// names no constraint — so the table names no condition for it and a
    /// `SQLite` caller keeps using
    /// [`is_unique_violation`]. The `PostgreSQL` half, where the
    /// code *is* a SQLSTATE, is covered by
    /// `tests/error_classification.rs::pg_restrict_delete_is_classified_as_foreign_key_violation`
    /// against a real server.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_real_sqlite_refusal_is_reached_but_names_no_condition() {
        use sea_orm::ConnectionTrait as _;

        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        db.execute_unprepared("CREATE TABLE t (id INTEGER PRIMARY KEY, k TEXT UNIQUE)")
            .await
            .expect("create");
        db.execute_unprepared("INSERT INTO t (id, k) VALUES (1, 'a')")
            .await
            .expect("first insert");

        let err = db
            .execute_unprepared("INSERT INTO t (id, k) VALUES (2, 'a')")
            .await
            .expect_err("the unique index must refuse the second insert");

        let refusal = driver_refusal(&err).expect("a driver refusal must be reachable");
        assert_eq!(
            refusal.code(),
            "2067",
            "SQLITE_CONSTRAINT_UNIQUE, the extended result code"
        );
        assert_eq!(
            refusal.constraint(),
            None,
            "SQLite names no constraint in its error"
        );
        assert_eq!(
            refusal.violation(),
            None,
            "an extended result code is not a SQLSTATE, so the table names no \
             condition for it"
        );
        // The portable classifier still recognises it, which is the path a
        // SQLite caller is meant to use.
        assert!(is_unique_violation(&err));
    }

    /// An error `SeaORM` produced itself carries no driver refusal. Asserted
    /// because the accessor's contract is that `None` means "no driver error
    /// here", never "the code was missing".
    #[test]
    fn an_internal_error_carries_no_driver_refusal() {
        assert!(driver_refusal(&sea_orm::DbErr::Custom("not from a driver".into())).is_none());
        assert!(driver_refusal(&sea_orm::DbErr::RecordNotFound("nope".into())).is_none());
    }

    // The classifiers are reached through two shapes: the typed `SqlErr` the
    // driver produces, and the `DbErr::Custom` left by a caller that re-wrapped
    // the error through `to_string()`.
    //
    // Both are production shapes now that these functions are public --
    // ledger's `repo_to_db` formats a repo error into `DbErr::Custom` before
    // the classifier ever sees it -- so what keeps the message tier honest is
    // not who calls it but `classifies_as`: the text is read only when the
    // error carries no driver code at all. A driver that spoke cannot be
    // contradicted by a rendering of what it said, and
    // `a_value_the_caller_chose_does_not_classify_the_error` pins the other
    // half of it, that a value the caller supplied cannot answer for the
    // driver. (`is_retryable_contention` reaches the same conclusion under a
    // stricter rule, because retrying is an action and classifying is not --
    // do not carry one across.)
    //
    // The typed path cannot be exercised from here: it needs a `DbErr` whose
    // `sql_err()` resolves, and that requires a real `PgDatabaseError` or
    // `SqliteError`, both of which have crate-private constructors. It is
    // covered end-to-end instead, by tests that provoke a genuine violation
    // against live SQLite. What is left for a unit test is the message
    // matching below, per backend.

    #[test]
    fn foreign_key_violation_detected_per_backend_message() {
        for msg in [
            "error returned from database: update or delete on table \"gts_type\" violates \
             foreign key constraint \"resource_group_gts_type_id_fkey\" on table \
             \"resource_group\"",
            "error returned from database: (code: 787) FOREIGN KEY constraint failed",
            "Cannot delete or update a parent row: a foreign key constraint fails",
        ] {
            assert!(
                is_foreign_key_violation(&DbErr::Custom(msg.to_owned())),
                "should classify as a foreign-key violation: {msg}"
            );
        }
    }

    /// The wording `PostgreSQL` 18 introduced for a `RESTRICT` refusal, on the
    /// text path.
    ///
    /// The structured path is what recognises this in production (the SQLSTATE
    /// is `23001`, and `crate::db_error` names both codes one condition). This
    /// branch is the last resort, for an error that reached a classifier
    /// stripped of its code — and it exists because 18 reworded the message
    /// as well as renumbering it, so the pre-18 substrings no longer match
    /// (issue #4645).
    #[test]
    fn the_postgres_18_restrict_wording_is_recognised_without_a_code() {
        let restrict = DbErr::Custom(
            "error returned from database: update or delete on table \"usage_type\" violates \
             RESTRICT setting of foreign key constraint \"usage_records_gts_id_fk\" on table \
             \"usage_records\""
                .to_owned(),
        );
        assert!(
            is_foreign_key_violation(&restrict),
            "PostgreSQL 18's RESTRICT wording must classify as a foreign-key violation"
        );
        assert!(
            !is_unique_violation(&restrict),
            "and must not be confused with a duplicate key"
        );
    }

    #[test]
    fn foreign_key_and_unique_are_not_confused() {
        // They map to different domain answers -- "still referenced" versus
        // "already exists" -- so a classifier that matched both would report
        // the wrong conflict.
        let unique = DbErr::Custom("UNIQUE constraint failed: gts_type.schema_id".to_owned());
        let unique_pg = DbErr::Custom(
            "error returned from database: duplicate key value violates unique constraint \
             \"gts_type_schema_id_key\""
                .to_owned(),
        );
        assert!(is_unique_violation(&unique_pg));
        assert!(!is_foreign_key_violation(&unique_pg));
        let fk = DbErr::Custom("FOREIGN KEY constraint failed".to_owned());

        assert!(is_unique_violation(&unique));
        assert!(!is_foreign_key_violation(&unique));

        assert!(is_foreign_key_violation(&fk));
        assert!(!is_unique_violation(&fk));
    }

    #[test]
    fn an_unrelated_error_is_neither() {
        let err = DbErr::Custom("connection reset by peer".to_owned());
        assert!(!is_unique_violation(&err));
        assert!(!is_foreign_key_violation(&err));
    }

    /// The finding this rule exists for: `PostgreSQL` echoes the offending
    /// value into the message, so a caller can put our own search strings
    /// there. `22P02` is a malformed input, not a conflict, and the value is
    /// whatever was submitted.
    #[test]
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    fn a_value_in_the_message_cannot_decide_the_condition() {
        let err = super::driver_shaped::refused(
            "22P02",
            r#"invalid input syntax for type uuid: "duplicate key""#,
        );
        assert!(
            !is_unique_violation(&err),
            "a caller-supplied 'duplicate key' must not classify a 22P02 as a conflict"
        );

        let err = super::driver_shaped::refused(
            "22P02",
            r#"invalid input syntax for type uuid: "violates foreign key constraint""#,
        );
        assert!(
            !is_foreign_key_violation(&err),
            "and the same for the foreign-key wording"
        );
    }

    /// The other half of the rule: when the code does name a condition, it is
    /// the whole answer, for both classifiers.
    #[test]
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    fn a_code_that_names_a_condition_is_the_whole_answer() {
        let unique = super::driver_shaped::refused(
            "23505",
            "duplicate key value violates unique constraint \"users_email_key\"",
        );
        assert!(is_unique_violation(&unique));
        assert!(!is_foreign_key_violation(&unique));

        // PostgreSQL 18's RESTRICT code, which sea-orm's own table does not map.
        let restrict = super::driver_shaped::refused(
            "23001",
            "update or delete on table \"parent\" violates RESTRICT setting",
        );
        assert!(is_foreign_key_violation(&restrict));
        assert!(!is_unique_violation(&restrict));
    }

    /// And the message tier is still there for what it documents: an error
    /// re-wrapped on the way here, with no driver error left to read.
    #[test]
    fn a_rewrapped_error_is_still_read_from_its_text() {
        let err = DbErr::Custom(
            "duplicate key value violates unique constraint \"users_email_key\"".to_owned(),
        );
        assert!(is_unique_violation(&err));
    }
}
