//! D-520 on SQLite: 000022 rebuilds `pricing_price`, keeps its indexes and a mutual pair, widens
//! `state` with `cancelled`, and pairs a change with the price it names and a cancelled price with
//! the unit that cancelled it. `down` restores the previous shape.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::Migration;
use crate::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use uuid::Uuid;

const TENANT: Uuid = Uuid::from_u128(0x22);
const BOOK: Uuid = Uuid::from_u128(0x2201);
const ENTRY: Uuid = Uuid::from_u128(0x2202);
const PRICE_A: Uuid = Uuid::from_u128(0x2210);
const PRICE_B: Uuid = Uuid::from_u128(0x2211);
const PAIR_A: Uuid = Uuid::from_u128(0x2220);
const PAIR_B: Uuid = Uuid::from_u128(0x2221);
const AUTHOR: Uuid = Uuid::from_u128(0x2230);
const UNIT: Uuid = Uuid::from_u128(0x2250);
const CHANGE: Uuid = Uuid::from_u128(0x2260);

fn x(id: Uuid) -> String {
    format!("X'{}'", id.simple())
}

async fn prior(db: &sea_orm::DatabaseConnection) {
    let manager = SchemaManager::new(db);
    for step in Migrator::migrations() {
        if step.name() == "m20261003_000022_price_cancel_and_end" {
            break;
        }
        step.up(&manager).await.unwrap();
    }
}

async fn exec(db: &sea_orm::DatabaseConnection, sql: &str) {
    db.execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn try_exec(db: &sea_orm::DatabaseConnection, sql: &str) -> Result<(), sea_orm::DbErr> {
    db.execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .await
        .map(|_| ())
}

async fn strings(db: &sea_orm::DatabaseConnection, sql: &str) -> Vec<String> {
    db.query_all_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect()
}

fn seed() -> Vec<String> {
    let price = |id, entry, version, from, state| {
        format!(
            "INSERT INTO pricing_price (id,tenant_id,price_book_entry_id,version_no,price_json,eligibility,effective_from,state,created_by,version,created_at,updated_at) VALUES ({},{},{},{version},'{{}}','all','{from}','{state}',{},1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            x(id),
            x(TENANT),
            x(entry),
            x(AUTHOR)
        )
    };
    vec![
        format!(
            "INSERT INTO pricing_price_book (id,tenant_id,code,name,currency,version,created_at,updated_at) VALUES ({},{},'eur','eur','EUR',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            x(BOOK),
            x(TENANT)
        ),
        format!(
            "INSERT INTO pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,period,reservation_id,reference_state,version,created_at,updated_at,model) VALUES ({},{},{},{},'recurring','month',{},'confirmed',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','flat')",
            x(ENTRY),
            x(TENANT),
            x(BOOK),
            x(Uuid::from_u128(0x2240)),
            x(Uuid::from_u128(0x2241))
        ),
        price(PRICE_A, ENTRY, 1, "2026-01-01", "approved"),
        price(PRICE_B, ENTRY, 2, "2026-03-01", "approved"),
        price(PAIR_A, ENTRY, 3, "2026-06-01", "draft"),
        price(PAIR_B, ENTRY, 4, "2026-07-01", "draft"),
        format!(
            "UPDATE pricing_price SET paired_price_id = {} WHERE id = {}",
            x(PAIR_B),
            x(PAIR_A)
        ),
        format!(
            "UPDATE pricing_price SET paired_price_id = {} WHERE id = {}",
            x(PAIR_A),
            x(PAIR_B)
        ),
    ]
}

/// A pending prices unit of the book: the unit a cancelled price names.
fn unit() -> String {
    format!(
        "INSERT INTO pricing_approval_unit (id, tenant_id, kind, ref_type, ref_id, state, \
         common_effective_date, quorum_required, generation, submitted_by, submitted_at, \
         decided_at, decided_note, snapshot, snapshot_hash, version) VALUES ({}, {}, 'prices', \
         'price_book', {}, 'pending', NULL, 1, 1, {}, '2026-01-01T00:00:00Z', NULL, NULL, '{{}}', \
         'h', 1)",
        x(UNIT),
        x(TENANT),
        x(BOOK),
        x(AUTHOR)
    )
}

async fn index_sql(db: &sea_orm::DatabaseConnection) -> Vec<String> {
    strings(
        db,
        "SELECT sql AS v FROM sqlite_master WHERE type = 'index' AND tbl_name = 'pricing_price' AND sql IS NOT NULL ORDER BY name",
    )
    .await
}

async fn trigger_sql(db: &sea_orm::DatabaseConnection) -> Vec<String> {
    strings(
        db,
        "SELECT sql AS v FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'pricing_price' ORDER BY name",
    )
    .await
}

async fn columns(db: &sea_orm::DatabaseConnection) -> Vec<String> {
    strings(
        db,
        "SELECT name AS v FROM pragma_table_info('pricing_price') ORDER BY name",
    )
    .await
}

#[tokio::test]
async fn sqlite_rebuilds_the_price_keeps_the_chain_and_round_trips() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    prior(&db).await;
    for sql in seed() {
        exec(&db, &sql).await;
    }
    let indexes = index_sql(&db).await;
    assert_eq!(indexes.len(), 2, "{indexes:?}");
    let triggers = trigger_sql(&db).await;
    let manager = SchemaManager::new(&db);
    Migration.up(&manager).await.unwrap();
    let cols = columns(&db).await;
    for name in ["change_kind", "target_price_id", "cancelled_by_unit_id"] {
        assert!(cols.iter().any(|c| c == name), "{cols:?}");
    }
    // The rebuild keeps both indexes; only the approved start narrows to the `set` rows.
    let narrowed: Vec<String> = indexes
        .iter()
        .map(|sql| {
            if sql.contains("pricing_price_approved_start") {
                format!("{sql} AND change_kind = 'set'")
            } else {
                sql.clone()
            }
        })
        .collect();
    assert_eq!(index_sql(&db).await, narrowed);
    assert_eq!(
        trigger_sql(&db).await,
        triggers,
        "the rebuild keeps every trigger"
    );
    let kinds = strings(
        &db,
        "SELECT change_kind AS v FROM pricing_price ORDER BY version_no",
    )
    .await;
    assert_eq!(kinds, vec!["set", "set", "set", "set"]);
    let chain = strings(
        &db,
        "SELECT effective_from AS v FROM pricing_price WHERE state = 'approved' ORDER BY version_no",
    )
    .await;
    assert_eq!(chain, vec!["2026-01-01", "2026-03-01"]);
    let pair = strings(
        &db,
        &format!(
            "SELECT hex(paired_price_id) AS v FROM pricing_price WHERE id = {}",
            x(PAIR_A)
        ),
    )
    .await;
    assert_eq!(pair, vec![PAIR_B.simple().to_string().to_uppercase()]);
    // The pairings: a change names its price and a price names none; a cancelled price names the
    // unit that cancelled it, and only a cancelled price names one.
    exec(&db, &unit()).await;
    for (sql, what) in [
        (
            format!(
                "UPDATE pricing_price SET change_kind = 'cancel' WHERE id = {}",
                x(PRICE_B)
            ),
            "a change that names no price",
        ),
        (
            format!(
                "UPDATE pricing_price SET target_price_id = {} WHERE id = {}",
                x(PRICE_A),
                x(PRICE_B)
            ),
            "a price that names another",
        ),
        (
            format!(
                "UPDATE pricing_price SET state = 'cancelled' WHERE id = {}",
                x(PRICE_B)
            ),
            "a cancelled price that names no unit",
        ),
        (
            format!(
                "UPDATE pricing_price SET cancelled_by_unit_id = {} WHERE id = {}",
                x(UNIT),
                x(PRICE_B)
            ),
            "a unit on a price that is not cancelled",
        ),
    ] {
        assert!(try_exec(&db, &sql).await.is_err(), "{what}: {sql}");
    }
    exec(
        &db,
        &format!(
            "UPDATE pricing_price SET state = 'cancelled', cancelled_by_unit_id = {} WHERE id = {}",
            x(UNIT),
            x(PRICE_B)
        ),
    )
    .await;
    let refused = try_exec(
        &db,
        &format!(
            "UPDATE pricing_price SET state = 'retired' WHERE id = {}",
            x(PRICE_A)
        ),
    )
    .await;
    assert!(refused.is_err(), "a state outside the widened check");
    let bad_kind = try_exec(
        &db,
        &format!(
            "UPDATE pricing_price SET change_kind = 'move' WHERE id = {}",
            x(PRICE_A)
        ),
    )
    .await;
    assert!(bad_kind.is_err(), "a change kind outside the check");
    exec(
        &db,
        &format!(
            "UPDATE pricing_price SET state = 'approved', cancelled_by_unit_id = NULL WHERE id = {}",
            x(PRICE_B)
        ),
    )
    .await;
    // An applied change keeps the start of the price it names (review RF-P item 9): `down` is
    // refused on the approved-start index, and the runner's transaction leaves the schema as it was.
    exec(
        &db,
        &format!(
            "INSERT INTO pricing_price (id,tenant_id,price_book_entry_id,version_no,price_json,eligibility,effective_from,effective_to,state,change_kind,target_price_id,created_by,version,created_at,updated_at) VALUES ({},{},{},5,'{{}}','all','2026-01-01','2026-02-01','approved','end',{},{},1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            x(CHANGE),
            x(TENANT),
            x(ENTRY),
            x(PRICE_A),
            x(AUTHOR)
        ),
    )
    .await;
    {
        use sea_orm::TransactionTrait;
        let txn = db.begin().await.unwrap();
        let refused = Migration.down(&SchemaManager::new(&txn)).await;
        txn.rollback().await.unwrap();
        let refused = refused.expect_err("an applied change on its price's start refuses down");
        assert!(
            refused.to_string().contains("UNIQUE"),
            "the approved-start index refuses it: {refused}"
        );
    }
    assert_eq!(index_sql(&db).await, narrowed, "the schema is as it was");
    assert!(columns(&db).await.iter().any(|c| c == "change_kind"));
    // A draft change goes back as a plain row.
    exec(
        &db,
        &format!(
            "UPDATE pricing_price SET state = 'draft' WHERE id = {}",
            x(CHANGE)
        ),
    )
    .await;
    Migration.down(&manager).await.unwrap();
    let restored = columns(&db).await;
    for name in ["change_kind", "target_price_id", "cancelled_by_unit_id"] {
        assert!(!restored.iter().any(|c| c == name), "{restored:?}");
    }
    assert_eq!(index_sql(&db).await, indexes);
    assert_eq!(trigger_sql(&db).await, triggers);
    let still = strings(
        &db,
        "SELECT effective_from AS v FROM pricing_price WHERE state = 'approved' ORDER BY version_no",
    )
    .await;
    assert_eq!(still, vec!["2026-01-01", "2026-03-01"]);
    let plain = strings(
        &db,
        &format!(
            "SELECT state || ' ' || effective_from AS v FROM pricing_price WHERE id = {}",
            x(CHANGE)
        ),
    )
    .await;
    assert_eq!(plain, ["draft 2026-01-01"]);
    let cancelled = try_exec(
        &db,
        &format!(
            "UPDATE pricing_price SET state = 'cancelled' WHERE id = {}",
            x(PRICE_B)
        ),
    )
    .await;
    assert!(cancelled.is_err(), "down restores the previous state check");
    Migration.up(&manager).await.unwrap();
    assert!(
        columns(&db)
            .await
            .iter()
            .any(|c| c == "cancelled_by_unit_id")
    );
}
