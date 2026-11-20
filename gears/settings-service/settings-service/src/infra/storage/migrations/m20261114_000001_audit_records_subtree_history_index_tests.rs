//! The subtree history index exists after the chain, beside the one the
//! single-scope history keeps, and rolls back alone.

use sea_orm_migration::MigratorTrait;
use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};

use super::super::Migrator;

/// The indexes of `audit_records` by name, each with the statement that made
/// it.
async fn audit_indexes(db: &DatabaseConnection) -> Vec<(String, String)> {
    db.query_all_raw(Statement::from_string(
        db.get_database_backend(),
        "SELECT name, sql FROM sqlite_master \
         WHERE type = 'index' AND tbl_name = 'audit_records' AND sql IS NOT NULL;",
    ))
    .await
    .expect("index listing")
    .iter()
    .map(|r| {
        (
            r.try_get::<String>("", "name").expect("name"),
            r.try_get::<String>("", "sql").expect("sql"),
        )
    })
    .collect()
}

fn named<'a>(indexes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    indexes
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, sql)| sql.as_str())
}

/// The columns an index statement names, in order, spacing and case folded.
fn columns_of(sql: &str) -> String {
    let inside = sql
        .rsplit_once('(')
        .map_or("", |(_, tail)| tail)
        .trim_end_matches([')', ';', ' ', '\n']);
    inside
        .split(',')
        .map(|c| c.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join(", ")
        .to_lowercase()
}

#[tokio::test]
async fn the_index_orders_one_settings_records_newest_first_with_the_id_tiebreak() {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, None).await.expect("migrations apply");

    let indexes = audit_indexes(&db).await;
    let recent = named(&indexes, "idx_audit_key_recent")
        .unwrap_or_else(|| panic!("the subtree history index exists: {indexes:?}"));
    assert_eq!(
        columns_of(recent),
        "declaration_key, occurred_at desc, id desc",
        "{recent}"
    );
    // The single-scope history keeps the index it reads by.
    let scoped = named(&indexes, "idx_audit_scoped")
        .unwrap_or_else(|| panic!("the scoped index is kept: {indexes:?}"));
    assert_eq!(
        columns_of(scoped),
        "declaration_key, tenant_id, occurred_at desc",
        "{scoped}"
    );
}

#[tokio::test]
async fn rolling_the_index_back_drops_it_and_nothing_else() {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite connects");
    Migrator::up(&db, None).await.expect("migrations apply");
    let before: Vec<String> = audit_indexes(&db)
        .await
        .into_iter()
        .map(|(name, _)| name)
        .collect();

    // The index migration and everything after it, so a later migration
    // cannot break a test that is about this one.
    let migrations = Migrator::migrations();
    let at = migrations
        .iter()
        .position(|m| m.name() == "m20261114_000001_audit_records_subtree_history_index")
        .expect("the migration is in the list");
    let steps = u32::try_from(migrations.len() - at).expect("a small list");
    Migrator::down(&db, Some(steps))
        .await
        .expect("the rollback applies");

    let after: Vec<String> = audit_indexes(&db)
        .await
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(
        !after.contains(&"idx_audit_key_recent".to_owned()),
        "{after:?}"
    );
    for kept in before.iter().filter(|n| *n != "idx_audit_key_recent") {
        assert!(after.contains(kept), "`{kept}` is kept: {after:?}");
    }

    // Applying again brings it back: the statement is idempotent either way.
    Migrator::up(&db, None).await.expect("the chain reapplies");
    assert!(
        named(&audit_indexes(&db).await, "idx_audit_key_recent").is_some(),
        "the index is back"
    );
}
