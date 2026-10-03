//! `m0002_value_versions` has no `down`: it is irreversible by design.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbErr};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

use super::super::Migrator;

/// A fresh in-memory `SQLite` database with every credstore migration applied.
async fn migrated_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, None).await.expect("migrations apply");
    db
}

fn assert_irreversible(err: &DbErr) {
    let DbErr::Migration(message) = err else {
        panic!("expected DbErr::Migration, got {err:?}");
    };
    assert!(
        message.contains("m0002_value_versions is irreversible"),
        "{message}"
    );
    assert!(message.contains("snapshot"), "{message}");
}

/// Nothing was rolled back: the schema is still the post-`up` one.
async fn assert_schema_is_post_up(db: &DatabaseConnection) {
    let manager = SchemaManager::new(db);
    for table in ["credstore_secrets", "credstore_write_intents"] {
        assert!(
            manager.has_table(table).await.expect("has_table"),
            "{table} must still exist"
        );
    }
    assert!(
        manager
            .has_column("credstore_secrets", "value_version")
            .await
            .expect("has_column")
    );
    assert!(
        !manager
            .has_column("credstore_secrets", "value_fp")
            .await
            .expect("has_column")
    );
    db.execute_unprepared("SELECT 1 FROM credstore_write_intents")
        .await
        .expect("intent table is queryable");
}

#[tokio::test]
async fn down_is_irreversible_and_leaves_the_schema_untouched() {
    let db = migrated_db().await;

    let err = super::Migration
        .down(&SchemaManager::new(&db))
        .await
        .expect_err("m0002 cannot be rolled back");

    assert_irreversible(&err);
    assert_schema_is_post_up(&db).await;
}

#[tokio::test]
async fn migrator_down_one_step_fails_and_applies_nothing() {
    let db = migrated_db().await;

    let err = Migrator::down(&db, Some(1))
        .await
        .expect_err("rolling back m0002 must fail");

    assert_irreversible(&err);
    assert_schema_is_post_up(&db).await;
}
