#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use sea_orm_migration::MigratorTrait;

async fn columns(db: &DatabaseConnection) -> Vec<String> {
    db.query_all_raw(Statement::from_string(
        DbBackend::Sqlite,
        "SELECT name AS v FROM pragma_table_info('products_approval_unit') ORDER BY cid".to_owned(),
    ))
    .await
    .unwrap()
    .iter()
    .map(|row| row.try_get::<String>("", "v").unwrap())
    .collect()
}

/// The chain up to this migration applies, then 000009 appends `submit_note` once however often
/// it runs, and its `down` removes it once however often it runs. The upgrade through the real
/// runner, with units seeded, is `tests/unit_submit_note_migration.rs` and its Postgres twin.
#[tokio::test]
async fn applies_replays_and_reverts_on_sqlite() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let chain = super::super::Migrator::migrations();
    let at = chain
        .iter()
        .position(|m| m.name() == Migration.name())
        .expect("000009 is in the chain");
    for prior in &chain[..at] {
        prior.up(&manager).await.unwrap();
    }
    let before = columns(&db).await;
    assert!(!before.iter().any(|c| c == "submit_note"), "{before:?}");
    Migration.up(&manager).await.unwrap();
    Migration.up(&manager).await.unwrap();
    let after = columns(&db).await;
    assert_eq!(
        after[..before.len()],
        before[..],
        "the existing columns keep their places"
    );
    assert_eq!(after[before.len()..], ["submit_note"]);
    Migration.down(&manager).await.unwrap();
    Migration.down(&manager).await.unwrap();
    assert_eq!(columns(&db).await, before);
}
