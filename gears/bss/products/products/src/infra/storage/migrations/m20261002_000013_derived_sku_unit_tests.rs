#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use sea_orm::{Database, DbBackend};
use sea_orm_migration::MigratorTrait;

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

async fn prior(db: &sea_orm::DatabaseConnection) -> SchemaManager<'_> {
    let manager = SchemaManager::new(db);
    let chain = super::super::Migrator::migrations();
    let at = chain
        .iter()
        .position(|migration| migration.name() == Migration.name())
        .expect("000013 is in the chain");
    for migration in &chain[..at] {
        migration.up(&manager).await.unwrap();
    }
    manager
}

fn sku_row(id: &str, reference: &str, unit: &str) -> String {
    format!(
        "INSERT INTO products_sku (id, tenant_id, code, name, type, description, sellable, \
         lifecycle, revision, published_version, usage_type_ref, unit, type_change_pending, \
         retire_pending, created_by, created_at, updated_at) VALUES \
         ('{id}','t1','{id}','{id}','usage','',1,'published',1,1,'{reference}','{unit}',0,0, \
         'a1','2026-10-02T00:00:00Z','2026-10-02T00:00:00Z')"
    )
}

fn type_row(output: &str) -> String {
    format!(
        "INSERT INTO products_derived_usage_type (tenant_id, id, code, name, created_by, created_at) \
         VALUES ('t1','type1','meter','Meter','a1','2026-10-02T00:00:00Z'); \
         INSERT INTO products_derived_usage_type_version \
         (tenant_id, type_id, version, declaration_json, digest, created_by, created_at) VALUES \
         ('t1','type1',1,'{{\"output_unit\":\"{output}\"}}','{DIGEST}','a1','2026-10-02T00:00:00Z')"
    )
}

async fn exec(db: &sea_orm::DatabaseConnection, sql: &str) {
    for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        db.execute_raw(Statement::from_string(
            DbBackend::Sqlite,
            statement.to_owned(),
        ))
        .await
        .unwrap();
    }
}

async fn unit_of(db: &sea_orm::DatabaseConnection, id: &str) -> Option<String> {
    db.query_all_raw(Statement::from_string(
        DbBackend::Sqlite,
        format!("SELECT unit AS v FROM products_sku WHERE id = '{id}'"),
    ))
    .await
    .unwrap()
    .first()
    .unwrap()
    .try_get("", "v")
    .unwrap()
}

/// An agreeing stored unit is nulled, and a later write of a unit on a derived ref is refused.
#[tokio::test]
async fn an_agreeing_derived_sku_loses_its_stored_unit_and_the_check_holds() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = prior(&db).await;
    exec(&db, &type_row("GB")).await;
    exec(&db, &sku_row("s1", "products.derived/meter@1", "GB")).await;
    Migration.up(&manager).await.unwrap();
    assert_eq!(unit_of(&db, "s1").await, None);
    let refused = db
        .execute_raw(Statement::from_string(
            DbBackend::Sqlite,
            sku_row("s2", "products.derived/meter@1", "GB"),
        ))
        .await;
    assert!(refused.is_err(), "a derived SKU stores no unit");
}

/// A stored unit that is not the version's output unit refuses the migration and stays.
#[tokio::test]
async fn a_disagreeing_derived_sku_refuses_the_migration_and_keeps_its_unit() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = prior(&db).await;
    exec(&db, &type_row("GB")).await;
    exec(
        &db,
        &sku_row("s-disagree", "products.derived/meter@1", "MB"),
    )
    .await;
    let error = Migration.up(&manager).await.unwrap_err().to_string();
    assert!(
        error.contains("s-disagree"),
        "the refusal names the SKU: {error}"
    );
    assert_eq!(unit_of(&db, "s-disagree").await.as_deref(), Some("MB"));
}
