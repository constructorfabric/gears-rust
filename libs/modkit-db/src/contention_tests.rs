use sea_orm::RuntimeErr;

use super::*;

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

// ── SQLite BUSY (code 5) ─────────────────────────────────────────

#[test]
fn sqlite_busy_exec_detected() {
    let err =
        exec_err("Execution Error: error returned from database: (code: 5) database is locked");
    assert!(is_retryable_contention(DbBackend::Sqlite, &err));
}

#[test]
fn sqlite_busy_query_detected() {
    let err = query_err("Query Error: error returned from database: (code: 5) database is locked");
    assert!(is_retryable_contention(DbBackend::Sqlite, &err));
}

// ── SQLite BUSY_SNAPSHOT (code 517) ──────────────────────────────

#[test]
fn sqlite_busy_snapshot_detected() {
    let err =
        exec_err("Execution Error: error returned from database: (code: 517) database is locked");
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
