//! The `change_ref` index migration on `SQLite`: it builds the partial index
//! on its own, after the activation migration, and its rollback drops the
//! index alone.

use sea_orm_migration::MigratorTrait;
use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};

use super::super::Migrator;
use super::super::migrations_tests::{before, from};

/// This migration's name in the list.
const INDEX: &str = "m20261031_000002_audit_change_ref_index";

async fn index_sql(db: &DatabaseConnection) -> Option<String> {
    db.query_one_raw(Statement::from_string(
        db.get_database_backend(),
        "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'idx_audit_change_ref';",
    ))
    .await
    .expect("readable")
    .map(|row| row.try_get::<String>("", "sql").expect("the index's sql"))
}

#[tokio::test]
async fn the_index_is_built_here_partial_and_dropped_alone_on_rollback() {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, Some(before(INDEX)))
        .await
        .expect("everything up to the activation migration applies");
    assert_eq!(
        index_sql(&db).await,
        None,
        "the activation migration does not build the index"
    );

    Migrator::up(&db, None)
        .await
        .expect("the migration applies");
    let sql = index_sql(&db).await.expect("the index exists");
    assert!(
        sql.contains("(change_ref)") && sql.contains("WHERE change_ref IS NOT NULL"),
        "partial over the rows that carry a change_ref: {sql}"
    );

    Migrator::down(&db, Some(from(INDEX)))
        .await
        .expect("the migration rolls back");
    assert_eq!(index_sql(&db).await, None, "the rollback drops the index");
    db.execute_unprepared(
        "INSERT INTO audit_records (id, resource, declaration_key, tenant_id, operation, actor, \
         actor_classification, outcome, request_id, occurred_at, change_ref)
         VALUES ('act', 'cf.settings:k@t', 'k', 't', 'activation', 'gear', 'public', \
         'success', 'req', 'now', 'ref');",
    )
    .await
    .expect("the activation migration's column and vocabulary stay");
}
