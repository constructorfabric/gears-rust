//! D-546 on SQLite: 000024 creates the quota set and rule tables with their CHECKs, keys and
//! index; a rule names a revision and an item that exist, an item that a rule names cannot be
//! deleted, and `down` refuses while a rule exists. The Postgres twin of the CHECKs is
//! `tests/postgres_plan_quota_rules.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::Migration;
use crate::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, DbErr, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use uuid::Uuid;

const TENANT: Uuid = Uuid::from_u128(0x24);
const BOOK: Uuid = Uuid::from_u128(0x2401);
const PLAN: Uuid = Uuid::from_u128(0x2402);
const REVISION: Uuid = Uuid::from_u128(0x2403);
const ITEM: Uuid = Uuid::from_u128(0x2404);
const AUTHOR: Uuid = Uuid::from_u128(0x2405);
const SKU: Uuid = Uuid::from_u128(0x2406);
const NOW: &str = "'2026-11-21T00:00:00Z'";

fn x(id: Uuid) -> String {
    format!("X'{}'", id.simple())
}

/// A database the whole chain migrated, 000024 last.
async fn migrated() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let chain = Migrator::migrations();
    assert_eq!(
        chain.last().map(|m| m.name().to_owned()).as_deref(),
        Some("m20261121_000024_plan_quota_rules")
    );
    for step in chain {
        step.up(&manager).await.unwrap();
    }
    db
}

async fn exec(db: &DatabaseConnection, sql: &str) -> Result<(), DbErr> {
    db.execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .await
        .map(|_| ())
}

/// A book, a plan, its draft revision and one item without an entry, by raw SQL.
async fn draft_with_item(db: &DatabaseConnection) -> (Uuid, Uuid) {
    for sql in [
        format!(
            "INSERT INTO pricing_price_book \
             (id,tenant_id,code,name,currency,version,created_at,updated_at) \
             VALUES ({},{},'eur','eur','EUR',1,{NOW},{NOW})",
            x(BOOK),
            x(TENANT)
        ),
        format!(
            "INSERT INTO pricing_plan \
             (id,tenant_id,code,name,version,created_by,created_at,updated_at) \
             VALUES ({},{},'P','P',1,{},{NOW},{NOW})",
            x(PLAN),
            x(TENANT),
            x(AUTHOR)
        ),
        format!(
            "INSERT INTO pricing_plan_revision \
             (id,tenant_id,plan_id,rev_no,book_id,state,version,created_by,created_at,updated_at) \
             VALUES ({},{},{},1,{},'draft',1,{},{NOW},{NOW})",
            x(REVISION),
            x(TENANT),
            x(PLAN),
            x(BOOK),
            x(AUTHOR)
        ),
        format!(
            "INSERT INTO pricing_plan_item \
             (id,tenant_id,revision_id,sku_id,treatment,reference_state,version,created_by,\
             created_at,updated_at) \
             VALUES ({},{},{},{},'included','unreserved',1,{},{NOW},{NOW})",
            x(ITEM),
            x(TENANT),
            x(REVISION),
            x(SKU),
            x(AUTHOR)
        ),
    ] {
        exec(db, &sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    (REVISION, ITEM)
}

/// One valid rule row, every column written, with each `(column, SQL literal)` of `overrides` in
/// place of its default.
async fn insert_rule(
    db: &DatabaseConnection,
    revision: Uuid,
    item: Uuid,
    overrides: &[(&str, &str)],
) -> Result<(), DbErr> {
    let digest = format!("'{}'", "a".repeat(64));
    let mut columns: Vec<(&str, String)> = vec![
        ("revision_id", x(revision)),
        ("rule_id", x(Uuid::new_v4())),
        ("tenant_id", x(TENANT)),
        ("item_id", x(item)),
        ("definition_id", x(Uuid::from_u128(0xd1))),
        ("definition_version", "'1'".to_owned()),
        ("definition_digest", digest),
        ("dimension_value", "NULL".to_owned()),
        ("logical_binding_key", "'compute'".to_owned()),
        ("subject_selector", "'resource_owner'".to_owned()),
        (
            "scope_selection_json",
            r#"'{"region":"region-1"}'"#.to_owned(),
        ),
        ("amount", "'10'".to_owned()),
        (
            "unit_ref",
            "'gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.vcpu.v1'".to_owned(),
        ),
        ("basis", "'per_plan_unit'".to_owned()),
        ("evidence_json", "'{}'".to_owned()),
        ("availability_json", "'{}'".to_owned()),
        ("created_at", NOW.to_owned()),
    ];
    for (column, value) in overrides {
        let slot = columns
            .iter_mut()
            .find(|(name, _)| name == column)
            .unwrap_or_else(|| panic!("no column {column}"));
        (*value).clone_into(&mut slot.1);
    }
    let names = columns
        .iter()
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join(",");
    let values = columns
        .iter()
        .map(|(_, v)| v.as_str())
        .collect::<Vec<_>>()
        .join(",");
    exec(
        db,
        &format!("INSERT INTO pricing_plan_quota_rule ({names}) VALUES ({values})"),
    )
    .await
}

/// 000024's own `down`, through its schema manager.
async fn down_000024(db: &DatabaseConnection) -> Result<(), DbErr> {
    Migration.down(&SchemaManager::new(db)).await
}

#[tokio::test]
async fn the_rule_checks_hold() {
    let db = migrated().await;
    let (revision, item) = draft_with_item(&db).await;
    // Each refusal names its own constraint, so every CHECK is proven on its own.
    for (column, value, check) in [
        ("basis", "'PerPlanUnit'", "basis"),
        ("subject_selector", "'tenant'", "subject"),
        ("logical_binding_key", "''", "key"),
        (
            "logical_binding_key",
            &format!("'{}'", "k".repeat(65)),
            "key",
        ),
        ("definition_digest", "'abc'", "digest"),
        ("amount", "''", "amount"),
        ("amount", &format!("'{}'", "9".repeat(40)), "amount"),
        ("definition_version", "''", "version"),
    ] {
        let error = insert_rule(&db, revision, item, &[(column, value)])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("CHECK"), "{column}: {error}");
        assert!(
            error.contains(&format!("chk_pricing_plan_quota_rule_{check}")),
            "{column}: {error}"
        );
    }
    insert_rule(&db, revision, item, &[]).await.unwrap();
}

#[tokio::test]
async fn the_set_checks_hold() {
    let db = migrated().await;
    let (revision, _) = draft_with_item(&db).await;
    let set = |count: &str, digest: &str| {
        format!(
            "INSERT INTO pricing_plan_quota_set \
             (revision_id,tenant_id,quota_rules_digest,rule_count,updated_by,updated_at) \
             VALUES ({},{},'{digest}',{count},{},{NOW})",
            x(revision),
            x(TENANT),
            x(AUTHOR)
        )
    };
    let full = "a".repeat(64);
    for (sql, check) in [
        (set("201", &full), "count"),
        (set("0", &full), "count"),
        (set("1", "abc"), "digest"),
    ] {
        let error = exec(&db, &sql).await.unwrap_err().to_string();
        assert!(
            error.contains(&format!("chk_pricing_plan_quota_set_{check}")),
            "{sql}: {error}"
        );
    }
    // SQLite lets a `text PRIMARY KEY` hold NULL unless the column says `NOT NULL`, and checks no
    // foreign key on NULL: the set's key says it.
    let error = exec(
        &db,
        &format!(
            "INSERT INTO pricing_plan_quota_set \
             (revision_id,tenant_id,quota_rules_digest,rule_count,updated_by,updated_at) \
             VALUES (NULL,{},'{full}',1,{},{NOW})",
            x(TENANT),
            x(AUTHOR)
        ),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("NOT NULL constraint failed: pricing_plan_quota_set.revision_id"),
        "{error}"
    );
    exec(&db, &set("200", &full)).await.unwrap();
}

#[tokio::test]
async fn a_rule_names_an_item_and_a_revision_that_exist() {
    let db = migrated().await;
    let (revision, item) = draft_with_item(&db).await;
    let error = insert_rule(&db, revision, Uuid::new_v4(), &[])
        .await
        .unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("foreign key"),
        "{error}"
    );
    let error = insert_rule(&db, Uuid::new_v4(), item, &[])
        .await
        .unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("foreign key"),
        "{error}"
    );
}

#[tokio::test]
async fn an_item_with_rules_cannot_be_deleted() {
    let db = migrated().await;
    let (revision, item) = draft_with_item(&db).await;
    insert_rule(&db, revision, item, &[]).await.unwrap();
    let error = exec(
        &db,
        &format!("DELETE FROM pricing_plan_item WHERE id = {}", x(item)),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("foreign key"),
        "{error}"
    );
}

#[tokio::test]
async fn down_refuses_while_a_rule_exists() {
    let db = migrated().await;
    let (revision, item) = draft_with_item(&db).await;
    insert_rule(&db, revision, item, &[]).await.unwrap();
    let error = down_000024(&db).await.unwrap_err();
    assert!(error.to_string().contains("holds rows"), "{error}");
    assert!(
        error.to_string().contains("pricing_plan_quota_rule"),
        "{error}"
    );
    exec(&db, "DELETE FROM pricing_plan_quota_rule")
        .await
        .unwrap();
    down_000024(&db).await.unwrap();
    down_000024(&db).await.unwrap();
}
