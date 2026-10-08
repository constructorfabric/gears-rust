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

const TABLES: &[&str] = &["products_sku_reference"];
