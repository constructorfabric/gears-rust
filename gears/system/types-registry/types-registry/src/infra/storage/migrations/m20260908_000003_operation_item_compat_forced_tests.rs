//! Cross-dialect checks for the `operation_item.compat_forced` migration.

use super::{
    MYSQL_DOWN_STATEMENTS, MYSQL_UP_STATEMENTS, PG_SQLITE_DOWN_STATEMENTS, PG_UP_STATEMENTS,
    SQLITE_UP_STATEMENTS,
};

const TABLE: &str = "types_registry__operation_item";
const COLUMN: &str = "compat_forced";

fn lists() -> [(&'static str, &'static [&'static str]); 3] {
    [
        ("postgres", PG_UP_STATEMENTS),
        ("sqlite", SQLITE_UP_STATEMENTS),
        ("mysql", MYSQL_UP_STATEMENTS),
    ]
}

fn only_statement(name: &str, statements: &'static [&'static str]) -> &'static str {
    assert_eq!(
        statements.len(),
        1,
        "{name} adds one column in one statement"
    );
    statements[0]
}

/// Every dialect alters the same table and adds the same column.
#[test]
fn every_backend_adds_the_column_to_the_operation_item_table() {
    for (name, statements) in lists() {
        let sql = only_statement(name, statements);
        assert!(
            sql.contains(&format!("ALTER TABLE {TABLE}")),
            "{name} must alter {TABLE}, got {sql}",
        );
        assert!(
            sql.contains(&format!("ADD COLUMN IF NOT EXISTS {COLUMN} "))
                || sql.contains(&format!("ADD COLUMN {COLUMN} ")),
            "{name} must add {COLUMN}, got {sql}",
        );
    }
}

/// `NOT NULL DEFAULT false` backfills existing items, all accepted before
/// waivers were supported (ceiling C9).
#[test]
fn every_backend_declares_the_column_not_null_and_defaulted_to_false() {
    for (name, statements) in lists() {
        let sql = only_statement(name, statements);
        assert!(
            sql.contains("NOT NULL"),
            "{name} must be NOT NULL, got {sql}"
        );
        let defaulted = sql.contains("DEFAULT false") || sql.contains("DEFAULT 0");
        assert!(
            defaulted,
            "{name} must default the column so existing rows are covered, got {sql}",
        );
    }
}

/// Use native Postgres boolean, and constrain the integer lowerings to 0/1.
#[test]
fn the_boolean_is_lowered_and_constrained_per_backend() {
    let pg = only_statement("postgres", PG_UP_STATEMENTS);
    assert!(pg.contains("boolean"), "got {pg}");

    let mysql = only_statement("mysql", MYSQL_UP_STATEMENTS);
    assert!(mysql.contains("TINYINT(1)"), "got {mysql}");
    assert!(
        mysql.contains(&format!("CHECK ({COLUMN} IN (0, 1))")),
        "MySQL must constrain the lowered boolean, got {mysql}",
    );

    let sqlite = only_statement("sqlite", SQLITE_UP_STATEMENTS);
    assert!(sqlite.contains("INTEGER"), "got {sqlite}");
    assert!(
        sqlite.contains(&format!("CHECK ({COLUMN} IN (0, 1))")),
        "SQLite must constrain the lowered boolean, got {sqlite}",
    );
}

/// `SQLite` does not support `IF NOT EXISTS` on `ADD COLUMN`.
#[test]
fn only_postgres_guards_the_add_column() {
    assert!(PG_UP_STATEMENTS[0].contains("ADD COLUMN IF NOT EXISTS"));
    assert!(!SQLITE_UP_STATEMENTS[0].contains("IF NOT EXISTS"));
    assert!(!MYSQL_UP_STATEMENTS[0].contains("IF NOT EXISTS"));
}

/// Verify backend dispatch as well as the SQL constants.
#[test]
fn each_supported_backend_dispatches_to_its_own_statement_list() {
    for (backend, expected) in [
        (sea_orm::DatabaseBackend::Postgres, PG_UP_STATEMENTS),
        (sea_orm::DatabaseBackend::Sqlite, SQLITE_UP_STATEMENTS),
        (sea_orm::DatabaseBackend::MySql, MYSQL_UP_STATEMENTS),
    ] {
        let got = super::up_statements(backend).expect("supported backend");
        assert_eq!(
            got, expected,
            "{backend:?} resolved to the wrong statement list"
        );
    }
}

/// Verify the backend-specific down path as well as the SQL constants.
#[test]
fn each_supported_backend_dispatches_to_its_down_statement_list() {
    for (backend, expected) in [
        (
            sea_orm::DatabaseBackend::Postgres,
            PG_SQLITE_DOWN_STATEMENTS,
        ),
        (sea_orm::DatabaseBackend::Sqlite, PG_SQLITE_DOWN_STATEMENTS),
        (sea_orm::DatabaseBackend::MySql, MYSQL_DOWN_STATEMENTS),
    ] {
        let got = super::down_statements(backend).expect("supported backend");
        assert_eq!(got, expected, "{backend:?} resolved to the wrong down list");
    }
}

/// Distinct SQL lists make misrouted backend dispatch detectable.
/// The wildcard refusal cannot be exercised: the non-exhaustive
/// `DatabaseBackend` enum has no fourth variant in the locked version.
#[test]
fn no_two_backends_share_a_statement_list() {
    let mut sql: Vec<&str> = lists()
        .into_iter()
        .map(|(name, statements)| only_statement(name, statements))
        .collect();
    let total = sql.len();
    sql.sort_unstable();
    sql.dedup();
    assert_eq!(
        sql.len(),
        total,
        "two backends lower to identical SQL, so a mis-wired dispatch arm is invisible",
    );
}

/// Down removes `MySQL`'s dependent CHECK before dropping the column.
#[test]
fn down_drops_the_constraint_then_the_column_and_never_the_table() {
    assert_eq!(
        PG_SQLITE_DOWN_STATEMENTS,
        [format!("ALTER TABLE {TABLE} DROP COLUMN {COLUMN}")]
    );
    assert_eq!(MYSQL_DOWN_STATEMENTS.len(), 2);
    assert!(MYSQL_DOWN_STATEMENTS[0].contains("DROP CHECK"));
    assert_eq!(
        MYSQL_DOWN_STATEMENTS[1],
        format!("ALTER TABLE {TABLE} DROP COLUMN {COLUMN}")
    );
    assert!(
        PG_SQLITE_DOWN_STATEMENTS
            .iter()
            .chain(MYSQL_DOWN_STATEMENTS)
            .all(|statement| !statement.contains("DROP TABLE")),
        "the table is the initial migration's to drop",
    );
}

/// `force` is reserved in `MySQL`; use `compat_forced` on every backend.
#[test]
fn no_backend_names_the_column_with_a_reserved_word() {
    for (name, statements) in lists() {
        let sql = only_statement(name, statements);
        assert!(
            !sql.contains(" force "),
            "{name} must not name the column `force`, which MySQL reserves; got {sql}",
        );
    }
}
