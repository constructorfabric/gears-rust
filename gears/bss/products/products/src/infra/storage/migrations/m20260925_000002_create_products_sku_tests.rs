#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use sea_orm::Database;
#[tokio::test]
async fn applies_replays_and_reverts_on_sqlite() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    Migration.up(&manager).await.unwrap();
    Migration.up(&manager).await.unwrap();
    for table in TABLES {
        assert!(manager.has_table(*table).await.unwrap());
    }
    Migration.down(&manager).await.unwrap();
    Migration.down(&manager).await.unwrap();
    for table in TABLES {
        assert!(!manager.has_table(*table).await.unwrap());
    }
}

const TABLES: &[&str] = &["products_sku", "products_sku_version"];

#[tokio::test]
async fn sku_versions_refuse_update_and_delete_after_migration_replay() {
    use sea_orm::ConnectionTrait;
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    super::super::m20260925_000001_create_products_category::Migration
        .up(&manager)
        .await
        .unwrap();
    Migration.up(&manager).await.unwrap();
    db.execute_unprepared("INSERT INTO products_category (id,tenant_id,code,name,status,created_at,updated_at) VALUES ('c','t','C','Category','active','2026-09-25','2026-09-25')").await.unwrap();
    db.execute_unprepared("INSERT INTO products_sku (id,tenant_id,code,name,type,category_id,lifecycle,created_by,created_at,updated_at) VALUES ('s','t','S','SKU','recurring','c','published','a','2026-09-25','2026-09-25')").await.unwrap();
    db.execute_unprepared("INSERT INTO products_sku_version (sku_id,tenant_id,published_version,effective_from,content,created_at) VALUES ('s','t',1,'2026-09-25','{}','2026-09-25')").await.unwrap();
    Migration.up(&manager).await.unwrap();
    for statement in [
        "UPDATE products_sku_version SET content='[]'",
        "DELETE FROM products_sku_version",
    ] {
        let error = db
            .execute_unprepared(statement)
            .await
            .expect_err("version mutations must fail");
        assert!(
            error
                .to_string()
                .contains("products_sku_version is append-only"),
            "{error}"
        );
    }
    let row = db
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Sqlite,
            "SELECT content FROM products_sku_version",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<String>("", "content").unwrap(), "{}");
    db.close().await.unwrap();
}
