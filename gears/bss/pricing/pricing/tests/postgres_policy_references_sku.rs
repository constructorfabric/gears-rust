//! D-514 on Postgres: the same seeded shapes as the SQLite migration test, in schema `bss`.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod pg_support;

use bss_pricing::infra::usage_policy_wire::digest_text;
use bss_pricing::module::BssPricingGear;
use bss_pricing_sdk::digest::policy_digest;
use bss_pricing_sdk::terms::{
    AggregationScope, Fold, PartialWindow, RatingWindow, Reset, Timezone, UsageRatingPolicyInput,
};
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use uuid::Uuid;

const MIGRATION: &str = "m20261002_000021_policy_references_sku";
const OLD_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONTENT: &str = r#"{"rating_window":{"kind":"calendar_hour","timezone":"UTC"},"aggregation_scope":"subscription_line","reset":"rating_window_start","quantity_semantics":{"meter":{"usage_type_id":"vm-hours","version":"v1"},"unit":"hour","fold":"SUM","accrual_policy_version":"integration-v1"},"partial_window":"actual_quantity_full_thresholds"}"#;

fn q(id: Uuid) -> String {
    format!("'{id}'")
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

async fn exec(pg: &Pg, sql: &str) {
    pg.raw()
        .await
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            sql.to_owned(),
        ))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn strings(pg: &Pg, sql: &str) -> Vec<String> {
    pg.raw()
        .await
        .query_all_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            sql.to_owned(),
        ))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect()
}

async fn prior(pg: &Pg) {
    let chain = BssPricingGear::default()
        .migrations()
        .into_iter()
        .filter(|m| m.name() != MIGRATION)
        .collect();
    toolkit_db::migration_runner::run_migrations_for_testing(&pg.db().await, chain)
        .await
        .unwrap();
}

fn seed(book: Uuid, policy: Uuid) -> Vec<String> {
    let tenant = Uuid::from_u128(0x21);
    let author = Uuid::from_u128(0x22);
    vec![
        format!(
            "INSERT INTO bss.pricing_price_book (id,tenant_id,code,name,currency,version,created_at,updated_at) VALUES ({},{},'eur','eur','EUR',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            q(book),
            q(tenant)
        ),
        format!(
            "INSERT INTO bss.pricing_usage_rating_policy (tenant_id,policy_id,version,digest,content,created_at,created_by) VALUES ({},{},1,'{OLD_DIGEST}','{CONTENT}'::jsonb,'2026-01-01T00:00:00Z',{})",
            q(tenant),
            q(policy),
            q(author)
        ),
    ]
}

fn entry(
    id: Uuid,
    book: Uuid,
    sku: Uuid,
    kind: &str,
    period: &str,
    policy: Option<Uuid>,
) -> String {
    let tenant = Uuid::from_u128(0x21);
    let period_sql = if period.is_empty() {
        "NULL".to_owned()
    } else {
        format!("'{period}'")
    };
    let (pid, pver, pdig) = match policy {
        Some(policy) => (q(policy), "1".to_owned(), format!("'{OLD_DIGEST}'")),
        None => ("NULL".to_owned(), "NULL".to_owned(), "NULL".to_owned()),
    };
    format!(
        "INSERT INTO bss.pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,period,reservation_id,reference_state,version,created_at,updated_at,model,usage_policy_id,usage_policy_version,usage_policy_digest) VALUES ({},{},{},{},'{kind}',{period_sql},{},'confirmed',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','per_unit',{pid},{pver},{pdig})",
        q(id),
        q(tenant),
        q(book),
        q(sku),
        q(Uuid::from_u128(0x23))
    )
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_reshapes_a_shared_policy_and_keeps_the_old_row() {
    let pg = Pg::empty().await;
    prior(&pg).await;
    let book = Uuid::from_u128(0x30);
    let policy = Uuid::from_u128(0x31);
    for sql in seed(book, policy) {
        exec(&pg, &sql).await;
    }
    exec(
        &pg,
        &entry(
            Uuid::from_u128(0x32),
            book,
            Uuid::from_u128(0x41),
            "usage",
            "",
            Some(policy),
        ),
    )
    .await;
    exec(
        &pg,
        &entry(
            Uuid::from_u128(0x33),
            book,
            Uuid::from_u128(0x42),
            "usage",
            "",
            Some(policy),
        ),
    )
    .await;
    exec(
        &pg,
        &entry(
            Uuid::from_u128(0x34),
            book,
            Uuid::from_u128(0x43),
            "usage",
            "",
            None,
        ),
    )
    .await;
    exec(
        &pg,
        &entry(
            Uuid::from_u128(0x35),
            book,
            Uuid::from_u128(0x44),
            "recurring",
            "month",
            None,
        ),
    )
    .await;
    let applied = toolkit_db::migration_runner::run_migrations_for_testing(
        &pg.db().await,
        BssPricingGear::default()
            .migrations()
            .into_iter()
            .filter(|m| m.name() == MIGRATION)
            .collect(),
    )
    .await
    .unwrap();
    assert_eq!(applied.applied_names, [MIGRATION.to_owned()]);
    let expected = digest_text(policy_digest(&rules()));
    let contents = strings(
        &pg,
        "SELECT content::text AS v FROM bss.pricing_usage_rating_policy",
    )
    .await;
    assert_eq!(contents.len(), 2, "{contents:?}");
    assert_eq!(
        contents
            .iter()
            .filter(|c| c.contains("quantity_semantics"))
            .count(),
        1
    );
    let digests = strings(
        &pg,
        "SELECT usage_policy_digest AS v FROM bss.pricing_price_book_entry WHERE usage_policy_id IS NOT NULL ORDER BY id",
    )
    .await;
    assert_eq!(digests, vec![expected.clone(), expected]);
    let versions = strings(
        &pg,
        "SELECT coalesce(usage_sku_version::text, 'null') AS v FROM bss.pricing_price_book_entry",
    )
    .await;
    assert!(versions.iter().all(|v| v == "null"), "{versions:?}");
    let index = strings(
        &pg,
        "SELECT indexdef AS v FROM pg_indexes WHERE indexname = 'pricing_price_book_entry_key'",
    )
    .await;
    assert!(
        index.iter().any(|sql| sql.contains("usage_policy_digest")),
        "{index:?}"
    );
    let refused = pg
        .raw()
        .await
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            format!(
                "INSERT INTO bss.pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,reservation_id,reference_state,version,created_at,updated_at,model,usage_sku_version) VALUES ({},{},{},{},'usage',{},'confirmed',1,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','per_unit',1)",
                q(Uuid::from_u128(0x36)),
                q(Uuid::from_u128(0x21)),
                q(book),
                q(Uuid::from_u128(0x45)),
                q(Uuid::from_u128(0x23))
            ),
        ))
        .await;
    let error = refused.expect_err("a version without a policy").to_string();
    assert!(error.contains("pricing_entry_sku_version"), "{error}");
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_refuses_a_frozen_acceptance_that_embeds_quantity_semantics() {
    let pg = Pg::empty().await;
    prior(&pg).await;
    let digest = "b".repeat(64);
    let tenant = Uuid::from_u128(0x21);
    exec(
        &pg,
        &format!(
            "INSERT INTO bss.pricing_acceptance (id,tenant_id,order_id,order_version,line_id,request_digest,terms_digest,receipt_json,accepted_at,hold_until,created_by) VALUES ({},{},{},'1',{},'{digest}','{digest}','{{\"usage_rating_policy\":{{\"content\":{{\"quantity_semantics\":{{\"fold\":\"SUM\"}}}}}}}}','2026-01-01T00:00:00Z','2026-01-02T00:00:00Z',{})",
            q(Uuid::from_u128(0x51)),
            q(tenant),
            q(Uuid::from_u128(0x52)),
            q(Uuid::from_u128(0x53)),
            q(Uuid::from_u128(0x54))
        ),
    )
    .await;
    let error = toolkit_db::migration_runner::run_migrations_for_testing(
        &pg.db().await,
        BssPricingGear::default()
            .migrations()
            .into_iter()
            .filter(|m| m.name() == MIGRATION)
            .collect(),
    )
    .await
    .expect_err("the receipt guard")
    .to_string();
    assert!(error.contains("pricing_acceptance"), "{error}");
}
