//! D-522 on SQLite: 000023 adds the book's archive mark, set and cleared as a pair, widens the
//! entry's `reference_state` with `released` and the reference op's `kind` with `release`,
//! rebuilding the entry's family and the op table; every row, key, index and trigger survives, and
//! the rebuilt price keeps 000022's pairings. `down` restores the previous shape, and refuses while
//! a row needs the wider sets.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::Migration;
use crate::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use uuid::Uuid;

const TENANT: Uuid = Uuid::from_u128(0x23);
const BOOK: Uuid = Uuid::from_u128(0x2301);
const ENTRY: Uuid = Uuid::from_u128(0x2302);
const LOST: Uuid = Uuid::from_u128(0x2303);
const PRICE: Uuid = Uuid::from_u128(0x2310);
const PAIR_A: Uuid = Uuid::from_u128(0x2311);
const PAIR_B: Uuid = Uuid::from_u128(0x2312);
const CHANGE: Uuid = Uuid::from_u128(0x2313);
const PLAN: Uuid = Uuid::from_u128(0x2320);
const REVISION: Uuid = Uuid::from_u128(0x2321);
const ITEM: Uuid = Uuid::from_u128(0x2322);
const OP: Uuid = Uuid::from_u128(0x2330);
const AUTHOR: Uuid = Uuid::from_u128(0x2340);
const SKU: Uuid = Uuid::from_u128(0x2341);
const TABLES: [&str; 5] = [
    "pricing_price_book",
    "pricing_price_book_entry",
    "pricing_price",
    "pricing_plan_item",
    "pricing_reference_op",
];

fn x(id: Uuid) -> String {
    format!("X'{}'", id.simple())
}

async fn prior(db: &sea_orm::DatabaseConnection) {
    let manager = SchemaManager::new(db);
    for step in Migrator::migrations() {
        if step.name() == "m20261003_000023_book_archive" {
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
    let now = "'2026-10-03T00:00:00Z'";
    let price = |id, version, from, state: &str| {
        format!(
            "INSERT INTO pricing_price (id,tenant_id,price_book_entry_id,version_no,price_json,eligibility,effective_from,state,created_by,version,created_at,updated_at) VALUES ({},{},{},{version},'{{}}','all','{from}','{state}',{},1,{now},{now})",
            x(id),
            x(TENANT),
            x(ENTRY),
            x(AUTHOR)
        )
    };
    let entry = |id, state: &str| {
        format!(
            "INSERT INTO pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,period,reservation_id,reference_state,version,created_at,updated_at,model) VALUES ({},{},{},{},'recurring','month',{},'{state}',1,{now},{now},'flat')",
            x(id),
            x(TENANT),
            x(BOOK),
            x(Uuid::from_u128(id.as_u128() + 0x100)),
            x(Uuid::from_u128(id.as_u128() + 0x200)),
        )
    };
    vec![
        format!(
            "INSERT INTO pricing_price_book (id,tenant_id,code,name,currency,version,created_at,updated_at) VALUES ({},{},'eur','eur','EUR',1,{now},{now})",
            x(BOOK),
            x(TENANT)
        ),
        entry(ENTRY, "confirmed"),
        entry(LOST, "lost"),
        price(PRICE, 1, "2026-01-01", "approved"),
        price(PAIR_A, 2, "2026-06-01", "draft"),
        price(PAIR_B, 3, "2026-07-01", "draft"),
        price(CHANGE, 4, "2026-01-01", "draft"),
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
        format!(
            "UPDATE pricing_price SET change_kind = 'cancel', target_price_id = {} WHERE id = {}",
            x(PRICE),
            x(CHANGE)
        ),
        format!(
            "INSERT INTO pricing_plan (id,tenant_id,code,name,version,created_by,created_at,updated_at) VALUES ({},{},'P','P',1,{},{now},{now})",
            x(PLAN),
            x(TENANT),
            x(AUTHOR)
        ),
        format!(
            "INSERT INTO pricing_plan_revision (id,tenant_id,plan_id,rev_no,book_id,state,version,created_by,created_at,updated_at) VALUES ({},{},{},1,{},'superseded',1,{},{now},{now})",
            x(REVISION),
            x(TENANT),
            x(PLAN),
            x(BOOK),
            x(AUTHOR)
        ),
        format!(
            "INSERT INTO pricing_plan_item (id,tenant_id,revision_id,sku_id,price_book_entry_id,treatment,reference_state,version,created_by,created_at,updated_at) VALUES ({},{},{},{},{},'paid','confirmed',1,{},{now},{now})",
            x(ITEM),
            x(TENANT),
            x(REVISION),
            x(SKU),
            x(ENTRY),
            x(AUTHOR)
        ),
        format!(
            "INSERT INTO pricing_reference_op (op_id,tenant_id,kind,ref_kind,ref_id,sku_id,state,attempts,next_attempt_at,created_by,created_at,updated_at) VALUES ({},{},'delete','price_book_entry',{},{},'done',0,{now},{},{now},{now})",
            x(OP),
            x(TENANT),
            x(ENTRY),
            x(SKU),
            x(AUTHOR)
        ),
    ]
}

/// Every index and trigger of the five tables, as SQLite stores their text.
async fn shape(db: &sea_orm::DatabaseConnection) -> Vec<String> {
    let names = TABLES.map(|t| format!("'{t}'")).join(",");
    strings(
        db,
        &format!(
            "SELECT type || ' ' || name || ': ' || sql AS v FROM sqlite_master \
             WHERE type IN ('index','trigger') AND tbl_name IN ({names}) AND sql IS NOT NULL \
             ORDER BY type, name"
        ),
    )
    .await
}

/// Every row of the five tables, as JSON text, in a stable order.
async fn rows(db: &sea_orm::DatabaseConnection) -> Vec<String> {
    let mut out = Vec::new();
    for table in TABLES {
        let columns = strings(
            db,
            &format!("SELECT name AS v FROM pragma_table_info('{table}') ORDER BY name"),
        )
        .await
        .into_iter()
        .filter(|c| c != "archived_at" && c != "archived_by")
        .map(|c| format!("'{c}', quote({c})"))
        .collect::<Vec<_>>()
        .join(", ");
        out.extend(
            strings(
                db,
                &format!(
                    "SELECT '{table} ' || json_object({columns}) AS v FROM {table} ORDER BY hex(rowid)"
                ),
            )
            .await,
        );
    }
    out.sort();
    out
}

async fn columns(db: &sea_orm::DatabaseConnection, table: &str) -> Vec<String> {
    strings(
        db,
        &format!("SELECT name AS v FROM pragma_table_info('{table}') ORDER BY name"),
    )
    .await
}

#[tokio::test]
async fn sqlite_widens_the_family_keeps_every_row_and_round_trips() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    prior(&db).await;
    for sql in seed() {
        exec(&db, &sql).await;
    }
    let shape_before = shape(&db).await;
    let rows_before = rows(&db).await;
    let manager = SchemaManager::new(&db);
    Migration.up(&manager).await.unwrap();

    let book = columns(&db, "pricing_price_book").await;
    for name in ["archived_at", "archived_by"] {
        assert!(book.iter().any(|c| c == name), "{book:?}");
    }
    assert_eq!(shape(&db).await, shape_before, "every index and trigger");
    assert_eq!(rows(&db).await, rows_before, "every row, every column");
    assert!(
        strings(&db, "SELECT 'x' AS v FROM pragma_foreign_key_check")
            .await
            .is_empty(),
        "every key still points at its row"
    );
    // The archive mark is a pair, and the rebuilt price keeps 000022's pairings.
    for (sql, what) in [
        (
            format!(
                "UPDATE pricing_price_book SET archived_at = '2026-10-03T01:00:00Z' WHERE id = {}",
                x(BOOK)
            ),
            "an archive time with no archiver",
        ),
        (
            format!(
                "UPDATE pricing_price_book SET archived_by = {} WHERE id = {}",
                x(AUTHOR),
                x(BOOK)
            ),
            "an archiver with no archive time",
        ),
        (
            format!(
                "UPDATE pricing_price SET target_price_id = NULL WHERE id = {}",
                x(CHANGE)
            ),
            "a change that names no price",
        ),
        (
            format!(
                "UPDATE pricing_price SET state = 'cancelled' WHERE id = {}",
                x(PRICE)
            ),
            "a cancelled price that names no unit",
        ),
    ] {
        assert!(try_exec(&db, &sql).await.is_err(), "{what}: {sql}");
    }
    // The wider sets.
    exec(
        &db,
        &format!(
            "UPDATE pricing_price_book_entry SET reference_state = 'released' WHERE id = {}",
            x(ENTRY)
        ),
    )
    .await;
    exec(
        &db,
        &format!(
            "UPDATE pricing_reference_op SET kind = 'release' WHERE op_id = {}",
            x(OP)
        ),
    )
    .await;
    exec(
        &db,
        &format!(
            "UPDATE pricing_price_book SET archived_at = '2026-10-03T01:00:00Z', archived_by = {} WHERE id = {}",
            x(AUTHOR),
            x(BOOK)
        ),
    )
    .await;
    for (sql, what) in [
        (
            format!(
                "UPDATE pricing_price_book_entry SET reference_state = 'gone' WHERE id = {}",
                x(LOST)
            ),
            "a reference state outside the widened check",
        ),
        (
            format!(
                "UPDATE pricing_reference_op SET kind = 'move' WHERE op_id = {}",
                x(OP)
            ),
            "an op kind outside the widened check",
        ),
        (
            format!(
                "INSERT INTO pricing_price (id,tenant_id,price_book_entry_id,version_no,price_json,eligibility,effective_from,state,created_by,version,created_at,updated_at) VALUES ({},{},{},9,'{{}}','all','2026-01-01','draft',{},1,'2026-10-03T00:00:00Z','2026-10-03T00:00:00Z')",
                x(Uuid::from_u128(0x2399)),
                x(TENANT),
                x(Uuid::from_u128(0x2398)),
                x(AUTHOR)
            ),
            "a price of no entry: the rebuilt key still holds",
        ),
    ] {
        assert!(try_exec(&db, &sql).await.is_err(), "{what}");
    }

    // Down refuses while a row needs the wider sets; the runner's transaction leaves the schema
    // as it was.
    {
        use sea_orm::TransactionTrait;
        let txn = db.begin().await.unwrap();
        let refused = Migration.down(&SchemaManager::new(&txn)).await;
        txn.rollback().await.unwrap();
        assert!(
            refused.is_err(),
            "a released entry has no state under the old check"
        );
    }
    assert_eq!(shape(&db).await, shape_before);
    exec(
        &db,
        &format!(
            "UPDATE pricing_price_book_entry SET reference_state = 'confirmed' WHERE id = {}",
            x(ENTRY)
        ),
    )
    .await;
    exec(
        &db,
        &format!(
            "UPDATE pricing_reference_op SET kind = 'delete' WHERE op_id = {}",
            x(OP)
        ),
    )
    .await;
    Migration.down(&manager).await.unwrap();
    let book = columns(&db, "pricing_price_book").await;
    assert!(!book.iter().any(|c| c == "archived_at"), "{book:?}");
    assert_eq!(shape(&db).await, shape_before);
    assert_eq!(rows(&db).await, rows_before);
    assert!(
        try_exec(
            &db,
            &format!(
                "UPDATE pricing_price_book_entry SET reference_state = 'released' WHERE id = {}",
                x(ENTRY)
            ),
        )
        .await
        .is_err(),
        "down restores the previous reference state check"
    );
    assert!(
        try_exec(
            &db,
            &format!(
                "UPDATE pricing_reference_op SET kind = 'release' WHERE op_id = {}",
                x(OP)
            ),
        )
        .await
        .is_err(),
        "down restores the previous op kind check"
    );
    Migration.up(&manager).await.unwrap();
    assert!(
        columns(&db, "pricing_price_book")
            .await
            .iter()
            .any(|c| c == "archived_by")
    );
}
