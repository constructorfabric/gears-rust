// Created: 2026-09-25 by Virtuozzo International GmbH
//! The seed is the platform minimum, and applying it twice leaves one row.

use sea_orm_migration::MigratorTrait;
use sea_orm_migration::sea_orm::{ConnectionTrait, Database, Statement};

use super::super::Migrator;

/// The migration's own source, read at compile time.
const SOURCE: &str = include_str!("m20260925_000003_audit_retention_policy_seed.rs");

#[test]
fn the_seed_is_the_platform_minimum() {
    let up = SOURCE.split("async fn down").next().expect("the up half");
    let seed = format!("SELECT 1, {}, ", crate::audit::MIN_RETENTION_DAYS);
    assert!(up.contains(&seed), "the seeded retention is the minimum");
}

#[tokio::test]
async fn the_policy_row_is_there_once_after_the_migrations() {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, None).await.expect("migrations apply");
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT id, retention_days FROM settings_audit_policy;",
        ))
        .await
        .expect("readable");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].try_get::<i32>("", "id").expect("id"), 1);
    assert_eq!(
        rows[0].try_get::<i32>("", "retention_days").expect("days"),
        i32::try_from(crate::audit::MIN_RETENTION_DAYS).expect("fits")
    );
}

#[tokio::test]
async fn rolling_the_seed_back_leaves_a_configured_row_where_it_is() {
    // The row is configuration the gear has written, not schema: a rollback
    // of this migration must not delete it, since the writer that stays live
    // only ever updates it and would fail every pass with the row gone. The
    // table itself goes with the migration that made it.
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, None).await.expect("migrations apply");
    db.execute_unprepared("UPDATE settings_audit_policy SET retention_days = 730 WHERE id = 1;")
        .await
        .expect("the gear configured a longer retention");

    Migrator::down(&db, Some(1)).await.expect("one step back");

    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT retention_days FROM settings_audit_policy WHERE id = 1;",
        ))
        .await
        .expect("readable");
    assert_eq!(rows.len(), 1, "the row survived the rollback");
    assert_eq!(
        rows[0].try_get::<i32>("", "retention_days").expect("days"),
        730
    );

    // And applying the seed again over it is a no-op.
    Migrator::up(&db, None).await.expect("re-applied");
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT retention_days FROM settings_audit_policy;",
        ))
        .await
        .expect("readable");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].try_get::<i32>("", "retention_days").expect("days"),
        730
    );
}
