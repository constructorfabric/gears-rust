//! Cross-dialect checks for dropping the revision `content_hash` column.

use super::{
    MYSQL_DOWN_STATEMENTS, PG_DOWN_STATEMENTS, REVISION_TABLES as TABLES, SQLITE_DOWN_STATEMENTS,
    UP_STATEMENTS,
};

/// Up drops exactly the one column from each revision table, never a table.
#[test]
fn up_drops_the_column_from_both_revision_tables() {
    assert_eq!(UP_STATEMENTS.len(), TABLES.len());
    for (sql, table) in UP_STATEMENTS.iter().zip(TABLES) {
        assert_eq!(
            *sql,
            format!("ALTER TABLE {table} DROP COLUMN content_hash")
        );
    }
}

/// Every backend runs the same portable `DROP COLUMN`.
#[test]
fn every_supported_backend_dispatches_to_the_shared_up_list() {
    for backend in [
        sea_orm::DatabaseBackend::Postgres,
        sea_orm::DatabaseBackend::Sqlite,
        sea_orm::DatabaseBackend::MySql,
    ] {
        let got = super::up_statements(backend).expect("supported backend");
        assert_eq!(got, UP_STATEMENTS, "{backend:?}");
    }
}

/// Down re-adds the `NOT NULL` column only on empty tables. `SQLite` requires a
/// default when adding that column.
#[test]
fn down_restores_a_defaulted_not_null_column_per_backend() {
    for (backend, expected, ty) in [
        (
            sea_orm::DatabaseBackend::Postgres,
            PG_DOWN_STATEMENTS,
            "bytea",
        ),
        (
            sea_orm::DatabaseBackend::Sqlite,
            SQLITE_DOWN_STATEMENTS,
            "BLOB",
        ),
        (
            sea_orm::DatabaseBackend::MySql,
            MYSQL_DOWN_STATEMENTS,
            "VARBINARY(64)",
        ),
    ] {
        let got = super::down_statements(backend).expect("supported backend");
        assert_eq!(got, expected, "{backend:?}");
        assert_eq!(got.len(), TABLES.len(), "{backend:?}");
        for (sql, table) in got.iter().zip(TABLES) {
            assert!(sql.contains(&format!("ALTER TABLE {table}")), "{sql}");
            assert!(
                sql.contains(&format!("ADD COLUMN content_hash {ty} NOT NULL DEFAULT")),
                "{sql}"
            );
            assert!(!sql.contains("DROP TABLE"), "{sql}");
        }
    }
}
