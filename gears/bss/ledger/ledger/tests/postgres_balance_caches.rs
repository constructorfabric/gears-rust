//! Postgres-only integration tests for the balance-cache tables.
//! Ignored by default; run with `cargo test -p bss-ledger -- --ignored`.
//!
//! Asserts the conditional no-negative CHECK rejects a negative `AR`
//! `account_balance` row and allows a negative `tax_subbalance` (no
//! aggregate guard on tax sub-balances).

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use uuid::Uuid;

use bss_ledger::infra::storage::migrations::Migrator;

async fn boot() -> (
    testcontainers_modules::testcontainers::ContainerAsync<Postgres>,
    DatabaseConnection,
) {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = Database::connect(&url).await.unwrap();
    Migrator::up(&db, None).await.unwrap();
    (container, db)
}

fn exec(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn account_balance_rejects_negative_ar() {
    let (_c, db) = boot().await;
    let tenant_id = Uuid::new_v4();
    let account_id = Uuid::new_v4();

    let err = db
        .execute(exec(format!(
            "INSERT INTO bss.ledger_account_balance
                (tenant_id, account_id, currency, account_class, normal_side, balance_minor)
             VALUES ('{tenant_id}', '{account_id}', 'USD', 'AR', 'DR', -5)"
        )))
        .await
        .expect_err("negative AR balance must be rejected by CHECK");
    assert!(
        err.to_string().contains("chk_account_balance_no_negative"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn account_balance_allows_negative_revenue() {
    let (_c, db) = boot().await;
    let tenant_id = Uuid::new_v4();
    let account_id = Uuid::new_v4();

    // REVENUE is not in the no-negative class set; a credit-normal balance
    // may legitimately be negative on the cache row's signed convention.
    db.execute(exec(format!(
        "INSERT INTO bss.ledger_account_balance
            (tenant_id, account_id, currency, account_class, normal_side, balance_minor)
         VALUES ('{tenant_id}', '{account_id}', 'USD', 'REVENUE', 'CR', -100)"
    )))
    .await
    .expect("negative REVENUE balance must be allowed");
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn tax_subbalance_allows_negative() {
    let (_c, db) = boot().await;
    let tenant_id = Uuid::new_v4();
    let account_id = Uuid::new_v4();

    db.execute(exec(format!(
        "INSERT INTO bss.ledger_tax_subbalance
            (tenant_id, account_id, tax_jurisdiction, tax_filing_period, balance_minor)
         VALUES ('{tenant_id}', '{account_id}', 'US-CA', '2026Q2', -42)"
    )))
    .await
    .expect("tax_subbalance has no aggregate no-negative guard");
}
