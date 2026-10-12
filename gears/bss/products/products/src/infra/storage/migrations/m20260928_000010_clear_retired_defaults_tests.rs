#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use sea_orm::{Database, DatabaseConnection, DbBackend};
use sea_orm_migration::MigratorTrait;

/// Every category as `id is_default status version changed`, where `changed` says whether
/// `updated_at` moved off the seeded instant.
async fn categories(db: &DatabaseConnection) -> Vec<String> {
    db.query_all_raw(Statement::from_string(
        DbBackend::Sqlite,
        "SELECT id || ' ' || is_default || ' ' || status || ' ' || version || ' ' || \
         (updated_at <> '2026-09-27T09:00:00Z') AS v FROM products_category ORDER BY id"
            .to_owned(),
    ))
    .await
    .unwrap()
    .iter()
    .map(|row| row.try_get::<String>("", "v").unwrap())
    .collect()
}

/// The chain up to this migration applies; 000010 clears the retired default once however often
/// it runs, keeps the active default and the retired category that is not the default, and its
/// `down` changes nothing. The upgrade through the real runner, with categories seeded by bound
/// values, is `tests/retired_default_migration.rs` and its Postgres twin.
#[tokio::test]
async fn clears_a_retired_default_once_and_reverts_to_nothing_on_sqlite() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let chain = super::super::Migrator::migrations();
    let at = chain
        .iter()
        .position(|m| m.name() == Migration.name())
        .expect("000010 is in the chain");
    for prior in &chain[..at] {
        prior.up(&manager).await.unwrap();
    }
    for (id, tenant, is_default, status) in [
        ("c1", "t1", 1, "retired"),
        ("c2", "t2", 1, "active"),
        ("c3", "t2", 0, "retired"),
    ] {
        db.execute_raw(Statement::from_string(
            DbBackend::Sqlite,
            format!(
                "INSERT INTO products_category (id, tenant_id, code, name, is_default, status, \
                 version, created_at, updated_at) VALUES ('{id}', '{tenant}', '{id}', '{id}', \
                 {is_default}, '{status}', 2, '2026-09-27T09:00:00Z', '2026-09-27T09:00:00Z')"
            ),
        ))
        .await
        .unwrap();
    }
    Migration.up(&manager).await.unwrap();
    Migration.up(&manager).await.unwrap();
    let cleared = categories(&db).await;
    assert_eq!(
        cleared,
        ["c1 0 retired 3 1", "c2 1 active 2 0", "c3 0 retired 2 0"]
    );
    Migration.down(&manager).await.unwrap();
    Migration.down(&manager).await.unwrap();
    assert_eq!(categories(&db).await, cleared);
}
