// Created: 2026-08-12 by Virtuozzo International GmbH
//! Tests for the migration harness.
//!
//! Acceptance criteria: FEATURE `gear-foundation.md` §6 — *the gear starts
//! against an empty database, runs all migrations to completion, and reports
//! ready*, and *a gear start with a failing migration does not serve traffic*.
//!
//! The second is `ToolKit`'s to enforce: it aborts startup when a migration
//! returns `Err`, so nothing here can make a partially migrated schema
//! reachable. What is testable in-crate is that the harness hands over a list
//! that actually applies.

use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DbBackend, Statement};
use sea_orm_migration::MigratorTrait;

use super::Migrator;

#[test]
fn the_harness_declares_its_migrations_in_order() {
    // Order is the vector's order and is what SeaORM records as applied. An
    // insertion rather than an append would renumber history against databases
    // that have already run it.
    let names: Vec<String> = Migrator::migrations()
        .iter()
        .map(|m| m.name().to_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            "m20260812_000001_initial".to_owned(),
            "m20260813_000001_categories".to_owned(),
            "m20260825_000001_setting_declarations".to_owned(),
            "m20260906_000001_setting_values".to_owned(),
            "m20260907_000001_audit_records".to_owned(),
            "m20260907_000002_tenant_permissions".to_owned(),
            "m20260915_000001_pending_secrets".to_owned(),
            "m20260917_000001_value_type_namespace".to_owned(),
            "m20260924_000001_audit_records_append_only".to_owned(),
            "m20260924_000002_audit_records_retention_floor".to_owned(),
            "m20260925_000001_audit_records_default_horizon_index".to_owned(),
            "m20260925_000002_audit_retention_policy".to_owned(),
            "m20260925_000003_audit_retention_policy_seed".to_owned(),
            "m20261014_000001_declaration_display_name".to_owned(),
        ]
    );
}

#[tokio::test]
async fn migrations_apply_to_an_empty_database() {
    // The acceptance criterion, run for real against an empty database rather
    // than asserted about. SQLite stands in for Postgres here; the migration is
    // a no-op on it by design, so what this proves is that the harness is wired
    // and applies cleanly, not that the Postgres DDL is correct.
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    assert_eq!(db.get_database_backend(), DbBackend::Sqlite);

    Migrator::up(&db, None).await.expect("migrations apply");

    let applied = Migrator::get_applied_migrations(&db)
        .await
        .expect("applied list is readable");
    assert_eq!(applied.len(), Migrator::migrations().len());
}

#[tokio::test]
async fn applying_twice_is_a_no_op() {
    // A restart re-runs the harness. The second pass must find nothing
    // outstanding rather than re-executing DDL.
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");

    Migrator::up(&db, None).await.expect("first pass applies");
    let after_first = Migrator::get_applied_migrations(&db)
        .await
        .expect("readable")
        .len();

    Migrator::up(&db, None).await.expect("second pass is clean");
    let after_second = Migrator::get_applied_migrations(&db)
        .await
        .expect("readable")
        .len();

    assert_eq!(after_first, after_second);
}

/// The columns of `setting_declarations` as SQLite reports them: name, and
/// whether the column refuses NULL.
async fn declaration_columns(db: &sea_orm::DatabaseConnection) -> Vec<(String, bool)> {
    db.query_all_raw(Statement::from_string(
        DbBackend::Sqlite,
        "PRAGMA table_info(setting_declarations)",
    ))
    .await
    .expect("table_info is readable")
    .iter()
    .map(|row| {
        (
            row.try_get::<String>("", "name").expect("a column name"),
            row.try_get::<i64>("", "notnull").expect("a notnull flag") != 0,
        )
    })
    .collect()
}

#[tokio::test]
async fn the_display_name_column_is_added_nullable_and_old_rows_read_null() {
    // A declaration written before the column existed has no label, and must
    // read as "unset" rather than as an empty string, or the surface would
    // start emitting a blank `display_name` for every pre-existing setting.
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    let total = u32::try_from(Migrator::migrations().len()).expect("a small list");
    Migrator::up(&db, Some(total - 1))
        .await
        .expect("everything before the display-name migration applies");
    assert!(
        !declaration_columns(&db)
            .await
            .iter()
            .any(|(name, _)| name == "display_name"),
        "the column must not exist before its migration"
    );

    db.execute_unprepared(
        "INSERT INTO categories (id, key, name, created_at, updated_at)
         VALUES ('cat', 'network', 'Network', '1970-01-01 00:00:00', '1970-01-01 00:00:00')",
    )
    .await
    .expect("category seeds");
    db.execute_unprepared(
        "INSERT INTO setting_declarations
           (id, key, leaf_slug, value_type_id, category_id, default_value,
            scope_class, mode, status, data_classification, source,
            last_change_at, created_at, updated_at, created_by)
         VALUES ('old-id',
                 'gts.cf.core.settings.setting_type.v1~acme.settings.network.old.v1~',
                 'old', 'gts.cf.core.settings.type_bool_flag.v1~', 'cat', 'true', 'local',
                 'standard', 'active', 'public', 'admin_authored',
                 '1970-01-01 00:00:00', '1970-01-01 00:00:00',
                 '1970-01-01 00:00:00', 'fixture')",
    )
    .await
    .expect("a row from before the column seeds");

    Migrator::up(&db, None)
        .await
        .expect("the display-name migration applies");

    let columns = declaration_columns(&db).await;
    let column = columns
        .iter()
        .find(|(name, _)| name == "display_name")
        .expect("the display_name column exists after the migration");
    assert!(!column.1, "display_name must be nullable");

    let row = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT display_name FROM setting_declarations WHERE id = 'old-id'",
        ))
        .await
        .expect("readable")
        .expect("the old row is still there");
    assert_eq!(
        row.try_get::<Option<String>>("", "display_name")
            .expect("a nullable column"),
        None,
        "a row from before the column reads NULL"
    );
}

#[tokio::test]
async fn sqlite_gets_no_trigram_index_for_display_name() {
    // The trigram index is a PostgreSQL extension feature; SQLite has no such
    // thing, and a migration that tried to create it there would fail to boot.
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, None).await.expect("migrations apply");

    let found = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_declarations_display_name_trgm'",
        ))
        .await
        .expect("readable");
    assert!(
        found.is_empty(),
        "SQLite must not carry the PostgreSQL-only trigram index"
    );
}

#[test]
fn no_domain_table_is_created_here() {
    // DECOMPOSITION entry 2.1: "Migration harness and shared schema
    // conventions; no domain tables in this feature." Scoped to the foundation's
    // own migration -- later migrations belong to the features that own their
    // tables, and `categories` arrives with entry 2.2.
    let statements = format!("{:?}", DatabaseBackend::Postgres);
    let _ = statements;
    for table in [
        "categories",
        "setting_declarations",
        "setting_values",
        "pending_changes",
        "apply_operations",
        "apply_change_results",
        "user_mode_preferences",
    ] {
        assert!(
            !MIGRATION_SOURCE.contains(&format!("CREATE TABLE IF NOT EXISTS {table}")),
            "`{table}` belongs to the feature that owns it, not to the foundation"
        );
    }
}

/// The initial migration's own source, so the assertion above reads what ships
/// rather than a copy that could drift from it.
const MIGRATION_SOURCE: &str = include_str!("m20260812_000001_initial.rs");

#[tokio::test]
async fn rolling_the_display_name_migration_back_drops_the_column_and_keeps_the_row() {
    // A rollback has to take only the label: the declaration it was written on
    // survives with every other column, and applying the migration again adds
    // the column back empty rather than resurrecting the dropped text.
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, None).await.expect("migrations apply");
    db.execute_unprepared(
        "INSERT INTO categories (id, key, name, created_at, updated_at)
         VALUES ('cat', 'network', 'Network', '1970-01-01 00:00:00', '1970-01-01 00:00:00')",
    )
    .await
    .expect("category seeds");
    db.execute_unprepared(
        "INSERT INTO setting_declarations
           (id, key, leaf_slug, value_type_id, category_id, default_value,
            scope_class, mode, status, data_classification, source,
            last_change_at, created_at, updated_at, created_by, display_name)
         VALUES ('row-id',
                 'gts.cf.core.settings.setting_type.v1~acme.settings.network.kept.v1~',
                 'kept', 'gts.cf.core.settings.type_bool_flag.v1~', 'cat', 'true', 'local',
                 'standard', 'active', 'public', 'admin_authored',
                 '1970-01-01 00:00:00', '1970-01-01 00:00:00',
                 '1970-01-01 00:00:00', 'fixture', 'Kept label')",
    )
    .await
    .expect("a labelled row seeds");

    Migrator::down(&db, Some(1)).await.expect("one step back");

    assert!(
        !declaration_columns(&db)
            .await
            .iter()
            .any(|(name, _)| name == "display_name"),
        "the rollback drops the column"
    );
    let row = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT leaf_slug, default_value FROM setting_declarations WHERE id = 'row-id'",
        ))
        .await
        .expect("readable")
        .expect("the declaration survived the rollback");
    assert_eq!(
        row.try_get::<String>("", "leaf_slug").expect("slug"),
        "kept",
        "the other columns are intact"
    );

    Migrator::up(&db, None).await.expect("re-applied");
    let row = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT display_name FROM setting_declarations WHERE id = 'row-id'",
        ))
        .await
        .expect("readable")
        .expect("the declaration is still there");
    assert_eq!(
        row.try_get::<Option<String>>("", "display_name")
            .expect("a nullable column"),
        None,
        "the column comes back empty"
    );
}
