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
//! Which leaves the question of where the line falls, since naming the five
//! conditions below is itself a choice. The test is **whether a caller in this
//! workspace branches on the distinction**:
//!
//! * A code earns a variant when some caller already acts differently on it.
//!   [`ConstraintViolation`] is `#[non_exhaustive]` so that a sixth one can be
//!   added the day a caller needs it, rather than in advance.
//! * Two codes collapse into one variant when no caller would branch between
//!   them. `23503` and `23001` differ in *when* the foreign key was checked,
//!   not in what the caller must now do: rows still reference this one.
//! * A distinction nobody branches on is not discarded, only left unnamed —
//!   [`DriverRefusal::code`] still returns the code verbatim, so a caller that
//!   does need to tell immediate `RESTRICT` from deferrable `NO ACTION` can,
//!   without this module having to guess on its behalf.

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
    /// A `CHECK` constraint rejected the row.
    Check,
    /// A `NOT NULL` column was given no value.
    NotNull,
    /// An `EXCLUDE` constraint rejected the row.
    Exclusion,
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
///   [`crate::secure::is_unique_violation`] and
///   [`crate::secure::is_foreign_key_violation`] remain the portable path.
#[must_use]
pub fn constraint_violation(sqlstate: &str) -> Option<ConstraintViolation> {
    match sqlstate {
        "23505" => Some(ConstraintViolation::Unique),
        // One condition, two codes. See `ConstraintViolation::ForeignKey`.
        "23503" | "23001" => Some(ConstraintViolation::ForeignKey),
        "23514" => Some(ConstraintViolation::Check),
        "23502" => Some(ConstraintViolation::NotNull),
        "23P01" => Some(ConstraintViolation::Exclusion),
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
        code: &'static str,
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
            Some(Cow::Borrowed(self.code))
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
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    pub fn refused(code: &'static str, message: &str) -> sea_orm::DbErr {
        sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(Arc::new(
            sqlx::Error::Database(Box::new(Refusal {
                code,
                message: message.to_owned(),
            })),
        )))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::{ConstraintViolation, constraint_violation, driver_refusal};

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

    #[test]
    fn the_other_class_23_codes_are_named() {
        assert_eq!(
            constraint_violation("23505"),
            Some(ConstraintViolation::Unique)
        );
        assert_eq!(
            constraint_violation("23514"),
            Some(ConstraintViolation::Check)
        );
        assert_eq!(
            constraint_violation("23502"),
            Some(ConstraintViolation::NotNull)
        );
        assert_eq!(
            constraint_violation("23P01"),
            Some(ConstraintViolation::Exclusion)
        );
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
    /// [`crate::secure::is_unique_violation`]. The `PostgreSQL` half, where the
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
        assert!(crate::secure::is_unique_violation(&err));
    }

    /// An error `SeaORM` produced itself carries no driver refusal. Asserted
    /// because the accessor's contract is that `None` means "no driver error
    /// here", never "the code was missing".
    #[test]
    fn an_internal_error_carries_no_driver_refusal() {
        assert!(driver_refusal(&sea_orm::DbErr::Custom("not from a driver".into())).is_none());
        assert!(driver_refusal(&sea_orm::DbErr::RecordNotFound("nope".into())).is_none());
    }
}
