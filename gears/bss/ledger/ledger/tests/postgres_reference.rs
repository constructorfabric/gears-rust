//! Postgres-only integration tests for the idempotency + reference tables.
//! Ignored by default; run with `cargo test -p bss-ledger -- --ignored`.
//!
//! Covers: (a) `idempotency_dedup` PK rejects a duplicate
//! `(tenant, flow, business_id)`; (b) `currency_scale_registry` round-trips;
//! (c) `tenant_account` expression-unique index rejects a duplicate CoA row.

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
async fn idempotency_dedup_rejects_duplicate_key() {
    let (_c, db) = boot().await;
    let tenant_id = Uuid::new_v4();

    let insert = |hash: &str| {
        exec(format!(
            "INSERT INTO bss.ledger_idempotency_dedup
                (tenant_id, flow, business_id, payload_hash, status)
             VALUES ('{tenant_id}', 'INVOICE_POST', 'biz-1', '{hash}', 'COMMITTED')"
        ))
    };

    db.execute(insert("h1")).await.expect("first insert ok");
    let err = db
        .execute(insert("h2"))
        .await
        .expect_err("duplicate (tenant, flow, business_id) must be rejected");
    assert!(
        err.to_string().to_lowercase().contains("duplicate")
            || err.to_string().contains("idempotency_dedup_pkey"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn currency_scale_registry_round_trips() {
    let (_c, db) = boot().await;
    let tenant_id = Uuid::new_v4();

    db.execute(exec(format!(
        "INSERT INTO bss.ledger_currency_scale_registry (tenant_id, currency, minor_units, source)
         VALUES ('{tenant_id}', 'USD', 2, 'ISO4217')"
    )))
    .await
    .unwrap();

    let row = db
        .query_one(exec(format!(
            "SELECT minor_units FROM bss.ledger_currency_scale_registry
             WHERE tenant_id = '{tenant_id}' AND currency = 'USD'"
        )))
        .await
        .unwrap()
        .expect("row must exist");
    let minor_units: i16 = row.try_get("", "minor_units").unwrap();
    assert_eq!(minor_units, 2);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn tenant_account_coa_unique_rejects_duplicate() {
    let (_c, db) = boot().await;
    let tenant_id = Uuid::new_v4();
    let legal_entity_id = Uuid::new_v4();

    let insert = || {
        let account_id = Uuid::new_v4();
        exec(format!(
            "INSERT INTO bss.ledger_tenant_account
                (account_id, tenant_id, legal_entity_id, account_class, currency, normal_side)
             VALUES ('{account_id}', '{tenant_id}', '{legal_entity_id}', 'AR', 'USD', 'DR')"
        ))
    };

    db.execute(insert()).await.expect("first CoA row ok");
    let err = db
        .execute(insert())
        .await
        .expect_err("duplicate CoA grain must be rejected by the expression-unique index");
    assert!(
        err.to_string().to_lowercase().contains("duplicate")
            || err.to_string().contains("uq_tenant_account_coa"),
        "unexpected error: {err}"
    );
}
