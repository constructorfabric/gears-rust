//! Database contention detection utility.
//!
//! Detects transient lock-contention errors that are safe to retry.
//! The entire transaction must be retried from `BEGIN` — not just the
//! failing statement — because the database has already rolled it back.
//!
//! # Covered engines
//!
//! * **`MySQL` / `MariaDB` / Percona `XtraDB` Cluster** — `InnoDB` deadlock
//!   (SQLSTATE `40001`) and Galera/wsrep certification conflicts that abort
//!   the transaction and require a full retry.
//!   `InnoDB` detects deadlocks instantly and rolls back one transaction.
//!
//!   > "Always be prepared to re-issue a transaction if it fails due to
//!   > deadlock. Deadlocks are not dangerous. Just try again."
//!   > — [MySQL 8.0 Reference Manual, InnoDB Deadlocks](https://dev.mysql.com/doc/refman/8.0/en/innodb-deadlocks.html)
//!
//! * **`PostgreSQL`** — serialization failure (SQLSTATE `40001`) and
//!   deadlock detected (SQLSTATE `40P01`).
//!
//!   > "Applications using this level must be prepared to retry transactions
//!   > due to serialization failures."
//!   > — [PostgreSQL docs, Transaction Isolation](https://www.postgresql.org/docs/current/transaction-iso.html#XACT-SERIALIZABLE)
//!
//! * **`SQLite`** — `SQLITE_BUSY` (code 5) and `SQLITE_BUSY_SNAPSHOT` (code 517).
//!   `SQLite` supports only one writer at a time; concurrent writers receive
//!   `SQLITE_BUSY` when the `busy_timeout` expires, or `SQLITE_BUSY_SNAPSHOT`
//!   immediately when a WAL snapshot cannot be upgraded.
//!   See [Result Codes — SQLITE_BUSY](https://www.sqlite.org/rescode.html#busy).
//!
//! # Backend dispatch
//!
//! The caller must supply the [`DbBackend`] so that pattern matching is scoped
//! to the correct engine, avoiding false positives from shared SQLSTATE codes
//! (e.g., `40001` means different things in `MySQL` vs `PostgreSQL`).
//!
//! This gear provides detection helpers for callers that manage their own
//! transaction lifecycle (e.g., the outbox sequencer).

use sea_orm::{DbBackend, DbErr};

/// `MySQL` deadlock SQLSTATE code.
const MYSQL_DEADLOCK_SQLSTATE: &str = "40001";
const MYSQL_DEADLOCK_MSG: &str = "deadlock";
const MYSQL_WSREP_DEADLOCK_MSG: &str = "wsrep detected deadlock/conflict";
const MYSQL_WSREP_CERTIFICATION_ERROR_MSG: &str = "transaction failed due to certification error";
const MYSQL_WSREP_CANNOT_CERTIFY_MSG: &str = "transaction cannot be certified";
const MYSQL_WSREP_WRITE_SET_CONFLICT_MSG: &str = "write-set conflict";
const MYSQL_WSREP_CERTIFICATION_FAILURE_MSG: &str =
    "transaction rolled back due to certification failure";
const MYSQL_RESTART_MSG: &str = "try restarting transaction";

/// `PostgreSQL` retryable SQLSTATE codes.
const PG_SERIALIZATION_FAILURE: &str = "40001";
const PG_DEADLOCK_DETECTED: &str = "40P01";

/// `PostgreSQL` retryable error MESSAGE fragments. sea-orm/sqlx surface the
/// condition through the error's *message text* on the `Display` path, not the
/// numeric SQLSTATE — so matching the code alone (`40001` / `40P01`) misses real
/// serialization failures and deadlocks (e.g. `"could not serialize access due
/// to concurrent update"` carries no `"40001"` substring). Matching the message
/// is the reliable signal here. Message text is `lc_messages`-dependent, so the
/// numeric-code checks above remain as a locale-independent belt-and-suspenders
/// -- restricted to the two literal shapes a SQLSTATE actually appears in
/// (`SQLSTATE 40001`, `(40001)`) rather than the bare digits, since `is_pg_contention`
/// also sees `DbErr::Custom` messages a caller composed itself, and a bare
/// `40001` is a run of six hex digits that a UUID can produce by chance (see
/// `contains_sqlstate`).
const PG_SERIALIZATION_MSG: &str = "could not serialize access";
const PG_DEADLOCK_MSG: &str = "deadlock detected";

/// `SQLite` error codes for write contention.
///
/// sqlx surfaces these as `"error returned from database: (code: N) database is locked"`.
const SQLITE_BUSY_CODE: &str = "(code: 5)";
const SQLITE_BUSY_SNAPSHOT_CODE: &str = "(code: 517)";
/// The same two, as the driver reports them rather than as a message renders
/// them.
const SQLITE_BUSY: &str = "5";
const SQLITE_BUSY_SNAPSHOT: &str = "517";
const SQLITE_LOCKED_MSG: &str = "database is locked";

/// Returns `true` if the error is a transient lock-contention error that is
/// safe to retry.
///
/// Covers:
/// * `MySQL` / `MariaDB` deadlock — SQLSTATE `40001`
/// * `PostgreSQL` serialization failure (`40001`) / deadlock (`40P01`)
/// * `SQLite` `SQLITE_BUSY` (code 5) — `busy_timeout` expired
/// * `SQLite` `SQLITE_BUSY_SNAPSHOT` (code 517) — WAL snapshot conflict
///
/// Detection prefers the code the driver reported ([`crate::db_error`]), and
/// falls back to the error's string representation when there is none. Neither
/// puts a `sqlx` type in this signature.
///
/// # Why `DbErr::Custom` is also checked
///
/// A caller may re-wrap a `DbErr` before it reaches `transaction_with_retry`,
/// producing `DbErr::Custom(<message>)` and losing the `Exec`/`Query` shape
/// but keeping the message text this function matches on.
///
/// Without this arm, retry detection silently returns `false` for a
/// genuinely retryable error, so `transaction_with_retry` never fires (RG-15).
///
/// The flip side of accepting `DbErr::Custom` here is that its message can be
/// anything calling code chose to put there, including an interpolated id --
/// see `contains_sqlstate` for why the numeric `40001`/`40P01` checks are
/// narrowed to their actual SQLSTATE renderings rather than a bare digit
/// match.
#[must_use]
pub fn is_retryable_contention(backend: DbBackend, err: &DbErr) -> bool {
    // The server's own code, when it gave one: no locale, no rendering, and no
    // interpolated identifier that happens to contain `40001`. Pulling the
    // SQLSTATE back out of the rendered message is the path `crate::db_error`
    // exists to replace, and `PostgreSQL` 18 already showed the server can
    // renumber and reword what this module reads.
    if let Some(code) = crate::db_error::driver_code(err) {
        if is_contention_code(backend, &code) {
            return true;
        }
        // The code the driver gave us settles the question -- except on
        // `MySQL`, where Galera reports a certification conflict in wording and
        // a build we have not measured may do so under a generic code such as
        // `HY000`. Everywhere else every contention condition has a code of its
        // own, and letting the rendered text answer after the code has spoken
        // only invites the value the statement supplied to answer for it:
        // `PostgreSQL` prints that value into the message of a `22P02` just as
        // it does into a `23505`, so a row carrying the text `deadlock
        // detected` would be retried forever instead of reported as the
        // deterministic refusal it is.
        //
        // The same reasoning bounds the `MySQL` exception: Class 23 ends the
        // question there too, because a duplicate-key message also quotes the
        // offending value.
        return backend == DbBackend::MySql
            && !mysql_names_a_refusal(&code)
            && is_contention_wording(backend, &err.to_string());
    }

    // No code to read: a `DbErr::Custom` a caller composed, or a driver error
    // that carried none. The message is all there is, numeric shapes included.
    match err {
        DbErr::Exec(runtime_err) | DbErr::Query(runtime_err) => {
            let msg = runtime_err.to_string();
            is_contention_message(backend, &msg)
        }
        DbErr::Custom(msg) => is_contention_message(backend, msg),
        _ => false,
    }
}

/// Whether `code` names a deterministic refusal on `MySQL` -- something the
/// statement did wrong, which retrying cannot fix.
///
/// Class 23 (`integrity_constraint_violation`) entire, wider than
/// [`crate::db_error::constraint_violation`] on purpose: that table names only
/// the conditions a caller in this workspace branches on, while this question
/// is the cruder one of whether retrying could possibly help.
///
/// `MySQL` is the only backend that has to ask. On the others a code that is
/// not a contention code already ends the question, so a refusal never reaches
/// the wording tier.
fn mysql_names_a_refusal(code: &str) -> bool {
    code.starts_with("23")
}

/// Whether `code`, as the driver reported it, is a contention condition.
///
/// `PostgreSQL` and `MySQL` report a SQLSTATE; `SQLite` reports its extended
/// result code, which is what `(code: 5)` renders in a message.
fn is_contention_code(backend: DbBackend, code: &str) -> bool {
    match backend {
        DbBackend::MySql => code == MYSQL_DEADLOCK_SQLSTATE,
        DbBackend::Postgres => code == PG_SERIALIZATION_FAILURE || code == PG_DEADLOCK_DETECTED,
        DbBackend::Sqlite => code == SQLITE_BUSY || code == SQLITE_BUSY_SNAPSHOT,
        _ => false,
    }
}

/// The message signals that are *words*, not codes.
///
/// Kept for an error that carried a code we do not recognise: Galera surfaces
/// certification conflicts in wording, and a serialization failure says so in
/// its text. What is deliberately absent is the numeric matching in
/// `contains_sqlstate` -- when the driver handed us a code, digging a different
/// one out of the rendered text can only be a coincidence, and a UUID in an
/// interpolated message supplies those.
fn is_contention_wording(backend: DbBackend, msg: &str) -> bool {
    match backend {
        DbBackend::MySql => {
            let msg = msg.to_ascii_lowercase();
            msg.contains(MYSQL_DEADLOCK_MSG)
                || msg.contains(MYSQL_WSREP_DEADLOCK_MSG)
                || msg.contains(MYSQL_WSREP_CERTIFICATION_ERROR_MSG)
                || msg.contains(MYSQL_WSREP_CANNOT_CERTIFY_MSG)
                || msg.contains(MYSQL_WSREP_WRITE_SET_CONFLICT_MSG)
                || msg.contains(MYSQL_WSREP_CERTIFICATION_FAILURE_MSG)
                || msg.contains(MYSQL_RESTART_MSG)
        }
        DbBackend::Postgres => msg.contains(PG_SERIALIZATION_MSG) || msg.contains(PG_DEADLOCK_MSG),
        DbBackend::Sqlite => is_sqlite_busy(msg),
        _ => false,
    }
}

/// Match an error message against the contention signatures of `backend`.
///
/// The single point where backend dispatch happens. A backend with no known
/// signatures returns `false` -- non-retryable rather than retried blindly.
fn is_contention_message(backend: DbBackend, msg: &str) -> bool {
    match backend {
        DbBackend::MySql => is_mysql_deadlock(msg),
        DbBackend::Postgres => is_pg_contention(msg),
        DbBackend::Sqlite => is_sqlite_busy(msg),
        // `DbBackend` is `#[non_exhaustive]` as of SeaORM 2.0. We have no
        // contention signatures for a backend we don't know, so treat it
        // as non-retryable rather than retrying blindly.
        _ => false,
    }
}

fn is_mysql_deadlock(msg: &str) -> bool {
    let msg = msg.to_ascii_lowercase();
    msg.contains(MYSQL_DEADLOCK_SQLSTATE)
        || msg.contains(MYSQL_DEADLOCK_MSG)
        || msg.contains(MYSQL_WSREP_DEADLOCK_MSG)
        || msg.contains(MYSQL_WSREP_CERTIFICATION_ERROR_MSG)
        || msg.contains(MYSQL_WSREP_CANNOT_CERTIFY_MSG)
        || msg.contains(MYSQL_WSREP_WRITE_SET_CONFLICT_MSG)
        || msg.contains(MYSQL_WSREP_CERTIFICATION_FAILURE_MSG)
        || msg.contains(MYSQL_RESTART_MSG)
}

fn is_pg_contention(msg: &str) -> bool {
    contains_sqlstate(msg, PG_SERIALIZATION_FAILURE)
        || contains_sqlstate(msg, PG_DEADLOCK_DETECTED)
        || msg.contains(PG_SERIALIZATION_MSG)
        || msg.contains(PG_DEADLOCK_MSG)
}

/// Match `sqlstate` in `msg` as an actual SQLSTATE occurrence, not a bare
/// digit run.
///
/// `is_retryable_contention` also classifies `DbErr::Custom` messages that
/// calling code assembled itself (see its doc comment), and those routinely
/// interpolate identifiers -- a group id, a parent id -- into the text. A
/// UUID is 32 hex characters, and `40001`/`40P01` are common enough digit
/// sequences that one turns up in an unrelated UUID by chance; a bare
/// `msg.contains("40001")` would then misclassify a "group <uuid> not found"
/// message as a serialization failure. Restricting the numeric match to the
/// two shapes a driver/log actually renders a SQLSTATE in -- `SQLSTATE
/// 40001` (verbose driver or log output) and `(40001)` (a parenthesized
/// code, e.g. `error 1213 (40001)`) -- keeps the numeric check a
/// belt-and-suspenders on top of the message-text match above without that
/// false-positive risk.
fn contains_sqlstate(msg: &str, sqlstate: &str) -> bool {
    msg.contains(&format!("SQLSTATE {sqlstate}")) || msg.contains(&format!("({sqlstate})"))
}

fn is_sqlite_busy(msg: &str) -> bool {
    (msg.contains(SQLITE_BUSY_CODE) || msg.contains(SQLITE_BUSY_SNAPSHOT_CODE))
        && msg.contains(SQLITE_LOCKED_MSG)
}

#[cfg(test)]
mod tests {
    use sea_orm::RuntimeErr;

    use super::*;

    /// A driver error whose code says one thing and whose rendered message
    /// carries another, which is the shape the code path exists to get right.
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    use crate::db_error::driver_shaped::refused;

    /// The hazard `contains_sqlstate` was narrowed for, closed at the source.
    ///
    /// A `DbErr` a caller composed can interpolate an id, and `40001` is a run
    /// of digits a UUID produces by chance -- the existing doc says so. When
    /// the driver gave us a code, that guesswork is not needed at all: this
    /// refusal is a unique violation whose message happens to render `(40001)`,
    /// and it is not retryable.
    #[test]
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    fn a_code_the_driver_gave_beats_a_sqlstate_shape_in_the_text() {
        let err = refused(
            "23505",
            "duplicate key value violates unique constraint \"orders_pkey\" \
             for group 8f3c40001a2b4d5e9f00040001bbccdd (40001)",
        );
        assert!(
            !is_retryable_contention(DbBackend::Postgres, &err),
            "a unique violation must not be retried because its text renders a SQLSTATE shape"
        );
    }

    /// The code path is per backend, and each backend spells contention
    /// differently: a SQLSTATE on `MySQL`, an extended result code on `SQLite`.
    #[test]
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    fn every_backend_recognises_its_own_contention_code() {
        let mysql = refused("40001", "Deadlock found when trying to get lock");
        assert!(is_retryable_contention(DbBackend::MySql, &mysql));

        // SQLITE_BUSY and SQLITE_BUSY_SNAPSHOT, as the driver reports them
        // rather than as `(code: 5)` in a rendered message.
        for code in ["5", "517"] {
            let sqlite = refused(code, "database is locked");
            assert!(
                is_retryable_contention(DbBackend::Sqlite, &sqlite),
                "SQLite code {code} must be retryable"
            );
        }

        // A refusal is not contention, on any of them.
        let refusal = refused("23505", "duplicate key value violates unique constraint");
        for backend in [DbBackend::MySql, DbBackend::Postgres, DbBackend::Sqlite] {
            assert!(!is_retryable_contention(backend, &refusal), "{backend:?}");
        }
    }

    /// A refusal stays a refusal, whatever its message happens to say.
    ///
    /// `PostgreSQL` prints the rejected value into a unique violation's detail,
    /// so the text of a `23505` is partly written by whoever supplied the row.
    /// Retrying a constraint violation cannot help, and a transaction that
    /// retries one spins until it gives up.
    #[test]
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    fn a_refusal_is_not_contention_however_its_message_reads() {
        let err = refused("23505", "duplicate key: deadlock detected");
        assert!(
            !is_retryable_contention(DbBackend::Postgres, &err),
            "a unique violation must not be retried because its value spells contention"
        );

        // Class 23 entire, not only the two codes `constraint_violation` names.
        for code in ["23505", "23503", "23001", "23514", "23502", "23P01"] {
            let err = refused(code, "could not serialize access due to concurrent update");
            assert!(
                !is_retryable_contention(DbBackend::Postgres, &err),
                "{code} is a refusal"
            );
        }

        // SQLite carries the primary code in the low byte of the extended one.
        for code in ["787", "1555", "2067"] {
            let err = refused(code, "database is locked");
            assert!(
                !is_retryable_contention(DbBackend::Sqlite, &err),
                "SQLite {code} is a constraint refusal"
            );
        }

        // `MySQL` is where the guard earns its keep, because `MySQL` is the one
        // backend whose wording is still consulted after a code. A duplicate
        // key quotes the offending value the same way.
        let duplicate = refused("23000", "Duplicate entry 'deadlock' for key 'PRIMARY'");
        assert!(
            !is_retryable_contention(DbBackend::MySql, &duplicate),
            "a duplicate key must not be retried because its value spells contention"
        );
    }

    /// Galera says it in words. The code path keeps those, and they are the
    /// reason the wording tier survives a driver error with a code.
    #[test]
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    fn a_wsrep_conflict_is_retryable_on_a_code_we_do_not_name() {
        let err = refused(
            "HY000",
            "WSREP detected deadlock/conflict and aborted the transaction",
        );
        assert!(is_retryable_contention(DbBackend::MySql, &err));
    }

    /// And the wording tier is `MySQL`'s alone. `PostgreSQL` and `SQLite` name
    /// every contention condition with a code, so once a code has spoken the
    /// text cannot answer over it -- which matters because the text is partly
    /// written by whoever supplied the row.
    ///
    /// `22P02` (`invalid_text_representation`) is the witness: `PostgreSQL`
    /// quotes the rejected value, so a column fed the literal string
    /// `deadlock detected` renders a message that reads like a conflict and is
    /// nothing of the sort. Retrying it cannot change the value.
    #[test]
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    fn wording_cannot_answer_over_a_code_outside_mysql() {
        let bad_value = refused(
            "22P02",
            "invalid input syntax for type uuid: \"deadlock detected\"",
        );
        assert!(
            !is_retryable_contention(DbBackend::Postgres, &bad_value),
            "a data exception must not be retried because its value spells contention"
        );

        // `SQLITE_ERROR`, the generic code, with the busy wording in its text.
        let generic = refused("1", "database is locked");
        assert!(
            !is_retryable_contention(DbBackend::Sqlite, &generic),
            "SQLite reports a busy database as code 5 or 517, not as wording under code 1"
        );
    }

    /// And the structured path recognises a real one, on a code sea-orm's own
    /// table has nothing to say about.
    #[test]
    #[cfg(any(feature = "pg", feature = "mysql", feature = "sqlite"))]
    fn a_serialization_failure_is_recognised_by_its_code() {
        let err = refused(
            "40001",
            "could not serialize access due to concurrent update",
        );
        assert!(is_retryable_contention(DbBackend::Postgres, &err));

        // Wording gone, code intact: still retryable.
        let terse = refused("40P01", "deadlock");
        assert!(is_retryable_contention(DbBackend::Postgres, &terse));
    }

    fn exec_err(msg: &str) -> DbErr {
        DbErr::Exec(RuntimeErr::Internal(msg.to_owned()))
    }

    fn query_err(msg: &str) -> DbErr {
        DbErr::Query(RuntimeErr::Internal(msg.to_owned()))
    }

    // ── MySQL ────────────────────────────────────────────────────────

    #[test]
    fn mysql_deadlock_detected() {
        let err = exec_err("MySqlError { ... SQLSTATE 40001: Deadlock found ... }");
        assert!(is_retryable_contention(DbBackend::MySql, &err));
    }

    #[test]
    fn mysql_wsrep_certification_conflict_detected() {
        let err =
            exec_err("MySqlError: WSREP detected deadlock/conflict and aborted the transaction");
        assert!(is_retryable_contention(DbBackend::MySql, &err));
    }

    #[test]
    fn mysql_wsrep_write_set_conflict_detected() {
        let err = exec_err("MySqlError: WSREP: Transaction failed due to write-set conflict");
        assert!(is_retryable_contention(DbBackend::MySql, &err));
    }

    #[test]
    fn unrelated_mysql_wsrep_message_not_retryable() {
        let err = exec_err(
            "MySqlError: WSREP provider failed certification metadata validation permanently",
        );
        assert!(!is_retryable_contention(DbBackend::MySql, &err));
    }

    #[test]
    fn mysql_restart_transaction_detected() {
        let err = exec_err("MySqlError: Lock wait timeout exceeded; try restarting transaction");
        assert!(is_retryable_contention(DbBackend::MySql, &err));
    }

    // ── PostgreSQL ────────────────────────────────────────────────────

    #[test]
    fn pg_serialization_failure_detected() {
        let err = exec_err("error returned from database: error with SQLSTATE 40001");
        assert!(is_retryable_contention(DbBackend::Postgres, &err));
    }

    #[test]
    fn pg_deadlock_detected() {
        let err = exec_err("error returned from database: error with SQLSTATE 40P01");
        assert!(is_retryable_contention(DbBackend::Postgres, &err));
    }

    #[test]
    fn pg_serialization_failure_by_message_detected() {
        // Real sea-orm/sqlx Display carries the message, not the numeric
        // SQLSTATE — this is the exact text Postgres emits for 40001 and the
        // concurrent-DELETE regression that the numeric-only match missed.
        let err = exec_err(
            "error returned from database: could not serialize access due to concurrent update",
        );
        assert!(is_retryable_contention(DbBackend::Postgres, &err));
    }

    #[test]
    fn pg_deadlock_by_message_detected() {
        let err = exec_err("error returned from database: deadlock detected");
        assert!(is_retryable_contention(DbBackend::Postgres, &err));
    }

    // ── RG-15 regression: DbErr::Custom-wrapped contention errors ────
    //
    // See `is_retryable_contention`'s doc comment for why `DbErr::Custom`
    // must be detected the same as `Exec`/`Query`.

    #[test]
    fn pg_serialization_failure_detected_through_custom_wrap() {
        let err = DbErr::Custom(
            "Query Error: error returned from database: could not serialize access due to \
             read/write dependencies among transactions"
                .to_owned(),
        );
        assert!(is_retryable_contention(DbBackend::Postgres, &err));
    }

    #[test]
    fn pg_deadlock_detected_through_custom_wrap() {
        let err = DbErr::Custom(
            "Query Error: error returned from database: deadlock detected".to_owned(),
        );
        assert!(is_retryable_contention(DbBackend::Postgres, &err));
    }

    #[test]
    fn mysql_deadlock_detected_through_custom_wrap() {
        let err = DbErr::Custom("Error 1213 (40001): Deadlock found".to_owned());
        assert!(is_retryable_contention(DbBackend::MySql, &err));
    }

    #[test]
    fn sqlite_busy_detected_through_custom_wrap() {
        let err =
            DbErr::Custom("error returned from database: (code: 5) database is locked".to_owned());
        assert!(is_retryable_contention(DbBackend::Sqlite, &err));
    }

    #[test]
    fn custom_wrap_non_contention_message_not_retryable() {
        let err = DbErr::Custom("UNIQUE constraint failed: gts_type.schema_id".to_owned());
        assert!(!is_retryable_contention(DbBackend::Postgres, &err));
        assert!(!is_retryable_contention(DbBackend::Sqlite, &err));
    }

    // ── Numeric SQLSTATE precision (UUID false-positive) ──────────────
    //
    // `DbErr::Custom` messages reaching this function are frequently
    // assembled by calling code with an interpolated id, not by a driver. A
    // bare `msg.contains("40001")` would call a "group not found" message a
    // serialization failure whenever the group's UUID happens to contain
    // that digit run in its hex -- unrelated to any real contention, and a
    // false positive that costs a few wasted retries rather than a
    // correctness bug, but a false positive all the same.

    #[test]
    fn custom_wrap_uuid_containing_sqlstate_digits_not_retryable() {
        let err = DbErr::Custom("group 40001abc-1234-5678-9abc-def012345678 not found".to_owned());
        assert!(
            !is_retryable_contention(DbBackend::Postgres, &err),
            "a UUID that happens to contain the digits `40001` must not be mistaken for \
             SQLSTATE 40001"
        );
    }

    #[test]
    fn custom_wrap_uuid_containing_deadlock_sqlstate_digits_not_retryable() {
        let err = DbErr::Custom("parent 40p01234-1234-5678-9abc-def012345678 not found".to_owned());
        assert!(
            !is_retryable_contention(DbBackend::Postgres, &err),
            "a UUID that happens to contain the digits `40p01` must not be mistaken for \
             SQLSTATE 40P01"
        );
    }

    #[test]
    fn real_pg_serialization_failure_message_still_retryable() {
        // The genuine shape sqlx/sea-orm surface for a real serialization
        // failure: "SQLSTATE 40001" as a substring of a longer driver
        // message, not the bare digits alone. Pinned separately from the
        // message-text tests above so a regression in `contains_sqlstate`
        // specifically (as opposed to the `could not serialize access`
        // match) turns this red.
        let err = DbErr::Custom(
            "error returned from database: error with SQLSTATE 40001: could not serialize \
             access due to concurrent update"
                .to_owned(),
        );
        assert!(is_retryable_contention(DbBackend::Postgres, &err));
    }

    // ── SQLite BUSY (code 5) ─────────────────────────────────────────

    #[test]
    fn sqlite_busy_exec_detected() {
        let err =
            exec_err("Execution Error: error returned from database: (code: 5) database is locked");
        assert!(is_retryable_contention(DbBackend::Sqlite, &err));
    }

    #[test]
    fn sqlite_busy_query_detected() {
        let err =
            query_err("Query Error: error returned from database: (code: 5) database is locked");
        assert!(is_retryable_contention(DbBackend::Sqlite, &err));
    }

    // ── SQLite BUSY_SNAPSHOT (code 517) ──────────────────────────────

    #[test]
    fn sqlite_busy_snapshot_detected() {
        let err = exec_err(
            "Execution Error: error returned from database: (code: 517) database is locked",
        );
        assert!(is_retryable_contention(DbBackend::Sqlite, &err));
    }

    // ── Cross-engine isolation ──────────────────────────────────────

    #[test]
    fn sqlstate_40001_not_retryable_on_sqlite() {
        let err = exec_err("SQLSTATE 40001");
        assert!(!is_retryable_contention(DbBackend::Sqlite, &err));
    }

    #[test]
    fn sqlite_busy_not_retryable_on_mysql() {
        let err =
            exec_err("Execution Error: error returned from database: (code: 5) database is locked");
        assert!(!is_retryable_contention(DbBackend::MySql, &err));
    }

    // ── Negative cases ───────────────────────────────────────────────

    #[test]
    fn sqlite_constraint_not_retryable() {
        let err = exec_err(
            "Execution Error: error returned from database: (code: 19) UNIQUE constraint failed",
        );
        assert!(!is_retryable_contention(DbBackend::Sqlite, &err));
    }

    #[test]
    fn unrelated_errors_not_retryable() {
        assert!(!is_retryable_contention(
            DbBackend::Sqlite,
            &DbErr::Custom("something".into()),
        ));
        assert!(!is_retryable_contention(
            DbBackend::Postgres,
            &DbErr::RecordNotFound("x".into()),
        ));
    }

    #[test]
    fn code_5_without_locked_msg_not_retryable() {
        let err = exec_err("error returned from database: (code: 5) something else");
        assert!(!is_retryable_contention(DbBackend::Sqlite, &err));
    }
}
