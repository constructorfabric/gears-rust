// Created: 2026-09-07 by Constructor Tech
//! The table's vocabularies are closed at the schema, not only in code.

use sea_orm_migration::MigratorTrait;
use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};

use super::super::Migrator;

async fn run(db: &DatabaseConnection, sql: &str) -> Result<(), sea_orm_migration::sea_orm::DbErr> {
    db.execute_unprepared(sql).await.map(|_| ())
}

async fn migrated() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, None).await.expect("migrations apply");
    db
}

fn insert(id: &str, operation: &str, classification: &str, outcome: &str) -> String {
    format!(
        "INSERT INTO audit_records (id, resource, declaration_key, tenant_id, operation, actor, \
         actor_classification, outcome, request_id, occurred_at)
         VALUES ('{id}', 'cf.settings:k@t', 'k', 't', '{operation}', 'who', '{classification}', \
         '{outcome}', 'req', 'now');"
    )
}

#[tokio::test]
async fn every_vocabulary_value_is_accepted_and_anything_else_refused() {
    let db = migrated().await;
    for (i, op) in [
        "create",
        "change",
        "revert",
        "remove",
        "clone",
        "secret_use",
    ]
    .iter()
    .enumerate()
    {
        run(&db, &insert(&format!("op-{i}"), op, "public", "success"))
            .await
            .unwrap_or_else(|e| panic!("`{op}` is in the vocabulary: {e}"));
    }
    assert!(
        run(&db, &insert("bad-op", "delete", "public", "success"))
            .await
            .is_err()
    );
    assert!(
        run(&db, &insert("bad-class", "create", "secret", "success"))
            .await
            .is_err()
    );
    assert!(
        run(&db, &insert("bad-outcome", "create", "pii", "partial"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_scoped_and_retention_indexes_exist() {
    let db = migrated().await;
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'audit_records';",
        ))
        .await
        .expect("index listing");
    let names: Vec<String> = rows
        .iter()
        .map(|r| r.try_get::<String>("", "name").expect("name"))
        .collect();
    assert!(names.contains(&"idx_audit_scoped".to_owned()), "{names:?}");
    assert!(
        names.contains(&"idx_audit_retention".to_owned()),
        "{names:?}"
    );
}
