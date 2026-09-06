// Created: 2026-08-12 by Constructor Tech
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

use sea_orm::{Database, DatabaseBackend, DbBackend};
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
