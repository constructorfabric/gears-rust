//! `m20261001_000011_sku_lifecycle_honesty` on `SQLite`: a stored `retiring` row becomes its prior
//! lifecycle with `retire_pending`, and the new CHECKs hold.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::super::Migrator;
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

async fn exec(db: &sea_orm::DatabaseConnection, sql: &str) {
    db.execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .await
        .unwrap();
}

async fn cell(db: &sea_orm::DatabaseConnection, sql: &str) -> Option<String> {
    let row = db
        .query_one_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .await
        .unwrap()
        .unwrap();
    row.try_get("", "v").ok()
}

fn chain_before() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
    Migrator::migrations()
        .into_iter()
        .filter(|m| m.name() != "m20261001_000011_sku_lifecycle_honesty")
        .collect()
}

#[tokio::test]
async fn a_retiring_row_keeps_its_prior_lifecycle_and_the_checks_hold() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    for migration in chain_before() {
        migration.up(&manager).await.unwrap();
    }
    exec(
        &db,
        "INSERT INTO products_category (id,tenant_id,code,name,status,version,created_at,updated_at) \
         VALUES ('c','t','storage','Storage','active',1,'2026-09-01','2026-09-01')",
    )
    .await;
    exec(
        &db,
        "INSERT INTO products_sku (id,tenant_id,code,name,type,category_id,lifecycle,\
         fence_prior_lifecycle,fenced_at,fence_op_id,created_by,created_at,updated_at) \
         VALUES ('retiring','t','RET','Retiring','recurring','c','retiring','published',\
         '2026-09-01T00:00:00Z','op','a','2026-09-01','2026-09-01')",
    )
    .await;
    exec(
        &db,
        "INSERT INTO products_sku (id,tenant_id,code,name,type,category_id,lifecycle,\
         created_by,created_at,updated_at) VALUES ('live','t','LIVE','Live','recurring','c',\
         'deprecated','a','2026-09-01','2026-09-01')",
    )
    .await;
    exec(
        &db,
        "INSERT INTO products_sku_version (sku_id,tenant_id,published_version,effective_from,content,created_at) \
         VALUES ('live','t',1,'2026-09-01','{\"name\":\"Live\"}','2026-09-01')",
    )
    .await;
    exec(
        &db,
        "INSERT INTO products_sku_reference (id,tenant_id,sku_id,owner_gear,ref_kind,ref_id,state,\
         reserved_by,reserved_at,forced) VALUES ('r','t','live','pricing','price_book_entry','ref',\
         'reserved','a','2026-09-01',0)",
    )
    .await;
    super::Migration.up(&manager).await.unwrap();

    assert_eq!(
        cell(
            &db,
            "SELECT lifecycle || ' ' || retire_pending AS v FROM products_sku WHERE id = 'retiring'"
        )
        .await
        .as_deref(),
        Some("published 1")
    );
    assert_eq!(
        cell(
            &db,
            "SELECT lifecycle || ' ' || retire_pending AS v FROM products_sku WHERE id = 'live'"
        )
        .await
        .as_deref(),
        Some("deprecated 0")
    );
    assert_eq!(
        cell(
            &db,
            "SELECT content AS v FROM products_sku_version WHERE sku_id = 'live'"
        )
        .await
        .as_deref(),
        Some("{\"name\":\"Live\"}")
    );
    assert_eq!(
        cell(
            &db,
            "SELECT state AS v FROM products_sku_reference WHERE id = 'r'"
        )
        .await
        .as_deref(),
        Some("reserved")
    );
    let names = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name AS v FROM sqlite_master WHERE type IN ('index','trigger') \
             AND tbl_name IN ('products_sku_version','products_sku_reference')"
                .to_owned(),
        ))
        .await
        .unwrap();
    let kept: Vec<String> = names
        .into_iter()
        .filter_map(|row| row.try_get("", "v").ok())
        .collect();
    for name in [
        "ix_products_sku_version_as_of",
        "products_sku_version_no_update",
        "products_sku_version_no_delete",
        "uq_products_sku_reference_live",
        "ix_products_sku_reference_live",
    ] {
        assert!(kept.iter().any(|kept| kept == name), "{name} in {kept:?}");
    }
    let columns = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name AS name FROM pragma_table_info('products_sku')".to_owned(),
        ))
        .await
        .unwrap();
    let names: Vec<String> = columns
        .iter()
        .map(|row| row.try_get::<String>("", "name").unwrap())
        .collect();
    assert!(names.contains(&"retire_pending".to_owned()));
    assert!(names.contains(&"lifecycle_next".to_owned()));
    assert!(names.contains(&"lifecycle_next_from".to_owned()));
    assert!(!names.iter().any(|n| n == "fence_prior_lifecycle"));

    for sql in [
        "UPDATE products_sku SET lifecycle = 'retiring' WHERE id = 'live'",
        "UPDATE products_sku SET retire_pending = 1 WHERE id = 'live'",
        "UPDATE products_sku SET lifecycle_next = 'retired', lifecycle_next_from = '2026-11-01' WHERE id = 'live'",
        "UPDATE products_sku SET lifecycle_next = 'deprecated' WHERE id = 'live'",
    ] {
        let err = db
            .execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await
            .expect_err("the CHECK refuses the write");
        let text = err.to_string();
        assert!(text.contains("CHECK constraint failed"), "{sql}: {text}");
    }
    exec(
        &db,
        "UPDATE products_sku SET lifecycle_next = 'deprecated', lifecycle_next_from = '2026-11-01' \
         WHERE id = 'live'",
    )
    .await;
    assert_eq!(
        cell(
            &db,
            "SELECT lifecycle_next AS v FROM products_sku WHERE id = 'live'"
        )
        .await
        .as_deref(),
        Some("deprecated")
    );
    assert!(
        super::Migration
            .down(&manager)
            .await
            .unwrap_err()
            .to_string()
            .contains("irreversible")
    );
}
