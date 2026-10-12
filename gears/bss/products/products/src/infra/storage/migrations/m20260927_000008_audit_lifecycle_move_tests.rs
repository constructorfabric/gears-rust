//! The named exception to the house shape `applies_replays_and_reverts_on_sqlite`: this migration
//! applies and replays, but it does not revert (P-D-213). The upgrade through the real runner, with
//! rows seeded, is `tests/audit_lifecycle_migration.rs` and its Postgres twin.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use sea_orm::{Database, DatabaseConnection, DbBackend};
use sea_orm_migration::MigratorTrait;

async fn columns(db: &DatabaseConnection) -> Vec<String> {
    db.query_all_raw(Statement::from_string(
        DbBackend::Sqlite,
        "SELECT name AS v FROM pragma_table_info('products_audit_log') ORDER BY cid".to_owned(),
    ))
    .await
    .unwrap()
    .iter()
    .map(|row| row.try_get::<String>("", "v").unwrap())
    .collect()
}

/// The chain up to this migration, on an in-memory database.
async fn before_this_migration() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let chain = super::super::Migrator::migrations();
    let at = chain
        .iter()
        .position(|m| m.name() == Migration.name())
        .expect("000008 is in the chain");
    for prior in &chain[..at] {
        prior.up(&manager).await.unwrap();
    }
    db
}

#[tokio::test]
async fn applies_and_replays_on_sqlite_and_refuses_to_revert() {
    let db = before_this_migration().await;
    let manager = SchemaManager::new(&db);
    let before = columns(&db).await;
    assert!(
        !before.iter().any(|c| c.ends_with("_lifecycle")),
        "{before:?}"
    );
    Migration.up(&manager).await.unwrap();
    Migration.up(&manager).await.unwrap();
    let after = columns(&db).await;
    assert_eq!(
        after[..before.len()],
        before[..],
        "the existing columns keep their places"
    );
    assert_eq!(after[before.len()..], ["from_lifecycle", "to_lifecycle"]);
    for _ in 0..2 {
        let error = Migration
            .down(&manager)
            .await
            .expect_err("000008 is irreversible");
        assert!(
            error
                .to_string()
                .contains("m20260927_000008_audit_lifecycle_move: irreversible"),
            "{error}"
        );
    }
    assert_eq!(columns(&db).await, after, "a refused down changes nothing");
}

async fn exec(db: &DatabaseConnection, sql: &str) -> Result<(), String> {
    db.execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn row(id: &str) -> String {
    format!(
        "INSERT INTO products_audit_log (audit_id,tenant_id,actor_ref,action,subject_kind,subject_id,\
         written_at,seal_state,from_lifecycle,to_lifecycle) VALUES ('{id}','t1','a1','sku.create',\
         'sku','s1','2026-09-27T00:00:00Z','unsealed',NULL,'draft')"
    )
}

fn seal(id: &str, also: &str) -> String {
    format!(
        "UPDATE products_audit_log SET seal_state = 'sealed', chain_id = 'c1', seq = 0, \
         row_hash = x'00'{also} WHERE audit_id = '{id}'"
    )
}

/// The guard after the migration: the seal that keeps every record column passes; a seal that
/// also changes `from_lifecycle` or `to_lifecycle` is refused, as are a plain UPDATE of either and
/// every DELETE; a value outside the five lifecycles is refused at INSERT.
#[tokio::test]
async fn the_seal_still_keeps_every_record_column_the_two_new_ones_included() {
    const REFUSED: &str = "products_audit_log is append-only";
    let db = before_this_migration().await;
    Migration.up(&SchemaManager::new(&db)).await.unwrap();
    for id in ["clean", "from", "to", "plain"] {
        exec(&db, &row(id)).await.unwrap();
    }
    for (sql, refusal) in [
        (seal("from", ", from_lifecycle = 'published'"), REFUSED),
        (seal("to", ", to_lifecycle = 'retired'"), REFUSED),
        (seal("to", ", to_lifecycle = NULL"), REFUSED),
        (
            "UPDATE products_audit_log SET from_lifecycle = 'draft' WHERE audit_id = 'plain'"
                .to_owned(),
            REFUSED,
        ),
        (
            "UPDATE products_audit_log SET to_lifecycle = 'published' WHERE audit_id = 'plain'"
                .to_owned(),
            REFUSED,
        ),
        (
            "DELETE FROM products_audit_log WHERE audit_id = 'plain'".to_owned(),
            REFUSED,
        ),
        (
            row("bad").replace("NULL,'draft'", "'gone','draft'"),
            "chk_products_audit_log_from_lifecycle",
        ),
        (
            row("bad").replace("NULL,'draft'", "NULL,'gone'"),
            "chk_products_audit_log_to_lifecycle",
        ),
    ] {
        let error = exec(&db, &sql).await.expect_err(&sql);
        assert!(error.contains(refusal), "{sql}\n{error}");
    }
    exec(&db, &seal("clean", "")).await.unwrap();
    let states: Vec<String> = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT audit_id || ' ' || seal_state || ' ' || coalesce(from_lifecycle, '-') || ' ' || \
             to_lifecycle AS v FROM products_audit_log ORDER BY audit_id"
                .to_owned(),
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect();
    assert_eq!(
        states,
        [
            "clean sealed - draft",
            "from unsealed - draft",
            "plain unsealed - draft",
            "to unsealed - draft"
        ]
    );
}
