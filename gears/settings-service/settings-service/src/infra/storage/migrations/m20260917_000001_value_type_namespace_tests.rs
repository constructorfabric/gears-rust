// Created: 2026-09-17 by Constructor Tech
//! The namespace rewrite, run against a real database.

use std::collections::BTreeMap;

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

use super::super::Migrator;

const OLD_BOOL: &str = "gts.cf.toolkit.settings.type_bool_flag.v1~";
const NEW_BOOL: &str = "gts.cf.core.settings.type_bool_flag.v1~";

async fn exec(db: &DatabaseConnection, sql: &str) {
    db.execute_unprepared(sql).await.expect("statement runs");
}

/// Every declaration's value type, keyed by its slug so an assertion reads as
/// "this row points here" rather than depending on row order.
async fn value_types(db: &DatabaseConnection) -> BTreeMap<String, String> {
    db.query_all_raw(Statement::from_string(
        db.get_database_backend(),
        "SELECT leaf_slug, value_type_id FROM setting_declarations",
    ))
    .await
    .expect("readable")
    .iter()
    .map(|row| {
        (
            row.try_get::<String>("", "leaf_slug").expect("a slug"),
            row.try_get::<String>("", "value_type_id").expect("an id"),
        )
    })
    .collect()
}

/// A database migrated up to the point just before the rewrite, holding rows
/// written under the old namespace.
async fn with_rows_written_before_the_move() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, None).await.expect("migrations apply");

    exec(
        &db,
        "INSERT INTO categories (id, key, name, created_at, updated_at)
         VALUES ('cat', 'network', 'Network', '1970-01-01 00:00:00', '1970-01-01 00:00:00')",
    )
    .await;

    for (slug, value_type) in [
        ("old_one", OLD_BOOL),
        ("old_two", "gts.cf.toolkit.settings.type_port.v1~"),
        ("already_moved", NEW_BOOL),
        // A third-party value type that merely resembles ours: the rewrite is
        // anchored at the start of the id, so this must be left alone.
        ("foreign", "gts.acme.settings.type_bool_flag.v1~"),
    ] {
        exec(
            &db,
            &format!(
                "INSERT INTO setting_declarations
                   (id, key, leaf_slug, value_type_id, category_id, default_value,
                    scope_class, mode, status, data_classification, source,
                    last_change_at, created_at, updated_at, created_by)
                 VALUES ('{slug}-id',
                         'gts.cf.core.settings.setting_type.v1~acme.settings.network.{slug}.v1~',
                         '{slug}', '{value_type}', 'cat', 'true', 'local', 'standard',
                         'active', 'public', 'admin_authored',
                         '1970-01-01 00:00:00', '1970-01-01 00:00:00',
                         '1970-01-01 00:00:00', 'fixture')"
            ),
        )
        .await;
    }
    db
}

#[tokio::test]
async fn a_row_written_before_the_move_points_at_the_gears_namespace_after_it() {
    let db = with_rows_written_before_the_move().await;
    super::Migration
        .up(&SchemaManager::new(&db))
        .await
        .expect("the rewrite runs");

    let after = value_types(&db).await;
    assert_eq!(after["old_one"], NEW_BOOL, "moved");
    assert_eq!(
        after["old_two"], "gts.cf.core.settings.type_port.v1~",
        "moved"
    );
    assert_eq!(after["already_moved"], NEW_BOOL, "left as it was");
    assert_eq!(
        after["foreign"], "gts.acme.settings.type_bool_flag.v1~",
        "another vendor's catalogue is not ours to rename"
    );
}

#[tokio::test]
async fn rewriting_twice_changes_nothing_more() {
    // A migration is applied once, but the same rewrite also runs on a database
    // that was already moved -- a restored dump, a re-pointed stand. It has to
    // be idempotent rather than merely applied-once.
    let db = with_rows_written_before_the_move().await;
    let manager = SchemaManager::new(&db);

    super::Migration.up(&manager).await.expect("first rewrite");
    let once = value_types(&db).await;
    super::Migration.up(&manager).await.expect("second rewrite");

    assert_eq!(value_types(&db).await, once);
    assert!(
        once.values().all(|id| !id.starts_with("gts.cf.toolkit.")),
        "{once:?}"
    );
}

#[tokio::test]
async fn the_move_can_be_walked_back() {
    let db = with_rows_written_before_the_move().await;
    let manager = SchemaManager::new(&db);

    super::Migration.up(&manager).await.expect("forward");
    super::Migration.down(&manager).await.expect("back");

    let ids = value_types(&db).await;
    assert_eq!(ids["old_one"], OLD_BOOL);
    assert_eq!(
        ids["already_moved"], OLD_BOOL,
        "down moves every id in the namespace, including one that was already there"
    );
    assert_eq!(
        ids["foreign"], "gts.acme.settings.type_bool_flag.v1~",
        "a foreign id is untouched in both directions"
    );
}
