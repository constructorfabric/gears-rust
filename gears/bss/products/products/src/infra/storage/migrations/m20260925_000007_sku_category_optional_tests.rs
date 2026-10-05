//! The named exception to the house shape `applies_replays_and_reverts_on_sqlite`: this migration
//! applies and replays, but it does not revert (P-D-196; plan review L9). The upgrade through the
//! real runner, with rows seeded, is `tests/sku_category_migration.rs` and its Postgres twin.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use sea_orm_migration::MigratorTrait;

async fn category_notnull(db: &sea_orm::DatabaseConnection) -> i64 {
    db.query_one_raw(Statement::from_string(
        DbBackend::Sqlite,
        "SELECT \"notnull\" AS v FROM pragma_table_info('products_sku') WHERE name = 'category_id'",
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get::<i64>("", "v")
    .unwrap()
}

#[tokio::test]
async fn applies_and_replays_on_sqlite_and_refuses_to_revert() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let chain = super::super::Migrator::migrations();
    let at = chain
        .iter()
        .position(|m| m.name() == Migration.name())
        .expect("000007 is in the chain");
    for prior in &chain[..at] {
        prior.up(&manager).await.unwrap();
    }
    assert_eq!(category_notnull(&db).await, 1);
    Migration.up(&manager).await.unwrap();
    Migration.up(&manager).await.unwrap();
    assert_eq!(category_notnull(&db).await, 0);
    for _ in 0..2 {
        let error = Migration
            .down(&manager)
            .await
            .expect_err("000007 is irreversible");
        assert!(
            error
                .to_string()
                .contains("m20260925_000007_sku_category_optional: irreversible"),
            "{error}"
        );
    }
    assert_eq!(
        category_notnull(&db).await,
        0,
        "a refused down changes nothing"
    );
}
