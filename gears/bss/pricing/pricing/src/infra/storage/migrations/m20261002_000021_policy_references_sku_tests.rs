//! D-514 on SQLite: 000021 reshapes a shared policy, leaves the old row, and refuses a frozen
//! acceptance that still embeds `quantity_semantics`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::Migration;
use crate::infra::storage::migrations::Migrator;
use crate::infra::usage_policy_wire::digest_text;
use bss_pricing_sdk::digest::policy_digest;
use bss_pricing_sdk::terms::{
    AggregationScope, Fold, PartialWindow, RatingWindow, Reset, Timezone, UsageRatingPolicyInput,
};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};
use uuid::Uuid;

const TENANT: Uuid = Uuid::from_u128(0x21);
const OLD_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONTENT: &str = r#"{"rating_window":{"kind":"calendar_hour","timezone":"UTC"},"aggregation_scope":"subscription_line","reset":"rating_window_start","quantity_semantics":{"meter":{"usage_type_id":"vm-hours","version":"v1"},"unit":"hour","fold":"SUM","accrual_policy_version":"integration-v1"},"partial_window":"actual_quantity_full_thresholds"}"#;

fn x(id: Uuid) -> String {
    format!("X'{}'", id.simple())
}

fn rules() -> UsageRatingPolicyInput {
    UsageRatingPolicyInput {
        rating_window: RatingWindow::CalendarHour {
            timezone: Timezone::Utc,
        },
        aggregation_scope: AggregationScope::SubscriptionLine,
        reset: Reset::RatingWindowStart,
        partial_window: PartialWindow::ActualQuantityFullThresholds,
        fold: Fold::Sum,
    }
}

async fn prior(db: &sea_orm::DatabaseConnection) {
    let manager = SchemaManager::new(db);
    for step in Migrator::migrations() {
        if step.name() == "m20261002_000021_policy_references_sku" {
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

async fn strings(db: &sea_orm::DatabaseConnection, sql: &str) -> Vec<String> {
    db.query_all_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect()
}

fn seed_catalog(book: Uuid) -> String {
    format!(
        "INSERT INTO pricing_price_book (id,tenant_id,code,name,currency,version,created_at,updated_at) VALUES ({},{},'eur','eur','EUR',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        x(book),
        x(TENANT)
    )
}

fn seed_policy(policy: Uuid) -> String {
    format!(
        "INSERT INTO pricing_usage_rating_policy (tenant_id,policy_id,version,digest,content,created_at,created_by) VALUES ({},{},1,'{OLD_DIGEST}','{CONTENT}','2026-01-01T00:00:00Z',{})",
        x(TENANT),
        x(policy),
        x(Uuid::from_u128(0x22))
    )
}

fn seed_entry(
    id: Uuid,
    book: Uuid,
    sku: Uuid,
    kind: &str,
    period: &str,
    policy: Option<Uuid>,
) -> String {
    let period_sql = if period.is_empty() {
        "NULL".to_owned()
    } else {
        format!("'{period}'")
    };
    let (pid, pver, pdig) = match policy {
        Some(policy) => (x(policy), "1".to_owned(), format!("'{OLD_DIGEST}'")),
        None => ("NULL".to_owned(), "NULL".to_owned(), "NULL".to_owned()),
    };
    format!(
        "INSERT INTO pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,period,reservation_id,reference_state,version,created_at,updated_at,model,usage_policy_id,usage_policy_version,usage_policy_digest) VALUES ({},{},{},{},'{kind}',{period_sql},{},'confirmed',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','per_unit',{pid},{pver},{pdig})",
        x(id),
        x(TENANT),
        x(book),
        x(sku),
        x(Uuid::from_u128(0x23))
    )
}

#[tokio::test]
async fn sqlite_reshapes_a_shared_policy_and_keeps_the_old_row() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    prior(&db).await;
    let book = Uuid::from_u128(0x30);
    let policy = Uuid::from_u128(0x31);
    let shared_a = Uuid::from_u128(0x32);
    let shared_b = Uuid::from_u128(0x33);
    let legacy = Uuid::from_u128(0x34);
    let recurring = Uuid::from_u128(0x35);
    exec(&db, &seed_catalog(book)).await;
    exec(&db, &seed_policy(policy)).await;
    exec(
        &db,
        &seed_entry(
            shared_a,
            book,
            Uuid::from_u128(0x41),
            "usage",
            "",
            Some(policy),
        ),
    )
    .await;
    exec(
        &db,
        &seed_entry(
            shared_b,
            book,
            Uuid::from_u128(0x42),
            "usage",
            "",
            Some(policy),
        ),
    )
    .await;
    exec(
        &db,
        &seed_entry(legacy, book, Uuid::from_u128(0x43), "usage", "", None),
    )
    .await;
    exec(
        &db,
        &seed_entry(
            recurring,
            book,
            Uuid::from_u128(0x44),
            "recurring",
            "month",
            None,
        ),
    )
    .await;
    let manager = SchemaManager::new(&db);
    Migration.up(&manager).await.unwrap();
    Migration.up(&manager).await.unwrap();
    let expected = digest_text(policy_digest(&rules()));
    let contents = strings(&db, "SELECT content AS v FROM pricing_usage_rating_policy").await;
    assert_eq!(contents.len(), 2, "{contents:?}");
    assert_eq!(
        contents
            .iter()
            .filter(|c| c.contains("quantity_semantics"))
            .count(),
        1
    );
    assert_eq!(
        contents
            .iter()
            .filter(|c| !c.contains("quantity_semantics"))
            .count(),
        1
    );
    let digests = strings(
        &db,
        "SELECT usage_policy_digest AS v FROM pricing_price_book_entry WHERE usage_policy_id IS NOT NULL ORDER BY id",
    )
    .await;
    assert_eq!(digests, vec![expected.clone(), expected.clone()]);
    let versions = strings(
        &db,
        "SELECT coalesce(usage_sku_version, 'null') AS v FROM pricing_price_book_entry ORDER BY id",
    )
    .await;
    assert_eq!(versions, vec!["null", "null", "null", "null"]);
    let index = strings(
        &db,
        "SELECT sql AS v FROM sqlite_master WHERE name = 'pricing_price_book_entry_key'",
    )
    .await;
    assert!(
        index[0].contains("usage_policy_digest"),
        "the entry key index still holds: {index:?}"
    );
    let refused = db
        .execute_raw(Statement::from_string(
            DbBackend::Sqlite,
            format!(
                "INSERT INTO pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,reservation_id,reference_state,version,created_at,updated_at,model,usage_sku_version) VALUES ({},{},{},{},'usage',{},'confirmed',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','per_unit',1)",
                x(Uuid::from_u128(0x36)),
                x(TENANT),
                x(book),
                x(Uuid::from_u128(0x45)),
                x(Uuid::from_u128(0x23))
            ),
        ))
        .await
        .expect_err("a version without a policy");
    assert!(
        refused.to_string().contains("pricing_entry_sku_version"),
        "{refused}"
    );
    let down = Migration.down(&manager).await.unwrap_err().to_string();
    assert!(
        down.contains("m20261002_000021_policy_references_sku"),
        "{down}"
    );
}

#[tokio::test]
async fn sqlite_refuses_a_frozen_acceptance_that_embeds_quantity_semantics() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    prior(&db).await;
    let digest = "b".repeat(64);
    exec(
        &db,
        &format!(
            "INSERT INTO pricing_acceptance (id,tenant_id,order_id,order_version,line_id,request_digest,terms_digest,receipt_json,accepted_at,hold_until,created_by) VALUES ({},{},{},'1',{},'{digest}','{digest}','{{\"usage_rating_policy\":{{\"content\":{{\"quantity_semantics\":{{\"fold\":\"SUM\"}}}}}}}}','2026-01-01T00:00:00Z','2026-01-02T00:00:00Z',{})",
            x(Uuid::from_u128(0x51)),
            x(TENANT),
            x(Uuid::from_u128(0x52)),
            x(Uuid::from_u128(0x53)),
            x(Uuid::from_u128(0x54))
        ),
    )
    .await;
    let manager = SchemaManager::new(&db);
    let error = Migration.up(&manager).await.unwrap_err().to_string();
    assert!(error.contains("pricing_acceptance"), "{error}");
    let column = strings(
        &db,
        "SELECT name AS v FROM pragma_table_info('pricing_price_book_entry') WHERE name = 'usage_sku_version'",
    )
    .await;
    assert!(column.is_empty(), "{column:?}");
}
