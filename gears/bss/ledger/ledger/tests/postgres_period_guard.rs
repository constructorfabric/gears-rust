//! Postgres-only: the fiscal-period guard. An `OPEN` period pins clean; a
//! `CLOSED` period and a missing period both yield `PeriodError::Closed`.
//! Ignored by default; run with `cargo test -p bss-ledger -- --ignored`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use bss_ledger::infra::posting::period::{FiscalPeriodGuard, PeriodError};
use bss_ledger::infra::storage::migrations::Migrator;
use sea_orm::{ConnectionTrait, Database, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

fn pg(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn pin_open_admits_open_rejects_closed_and_missing() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    let raw = Database::connect(&url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();

    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    let provider = DBProvider::<DbError>::new(tdb);

    let tenant = Uuid::now_v7();
    let legal_entity = tenant;
    let period_id = "202606";
    let guard = FiscalPeriodGuard::new();

    // Seed an OPEN period.
    raw.execute(pg(format!(
        "INSERT INTO bss.ledger_fiscal_period (tenant_id, legal_entity_id, period_id, fiscal_tz, status)
         VALUES ('{tenant}','{legal_entity}','{period_id}','UTC','OPEN')"
    )))
    .await
    .unwrap();

    // OPEN → Ok.
    let open = run_pin(&provider, &guard, tenant, legal_entity, period_id).await;
    assert!(open.is_ok(), "open period must pin: {open:?}");

    // Flip to CLOSED → Closed.
    raw.execute(pg(format!(
        "UPDATE bss.ledger_fiscal_period SET status='CLOSED'
         WHERE tenant_id='{tenant}' AND legal_entity_id='{legal_entity}' AND period_id='{period_id}'"
    )))
    .await
    .unwrap();
    let closed = run_pin(&provider, &guard, tenant, legal_entity, period_id).await;
    assert_eq!(closed, Err(PeriodError::Closed));

    // Missing period → Closed.
    let missing = run_pin(&provider, &guard, tenant, legal_entity, "209912").await;
    assert_eq!(missing, Err(PeriodError::Closed));
}

/// Pin a period inside one transaction, returning the guard's typed result
/// (carried out as the transaction's success value so it survives COMMIT).
async fn run_pin(
    provider: &DBProvider<DbError>,
    guard: &FiscalPeriodGuard,
    tenant: Uuid,
    legal_entity: Uuid,
    period_id: &str,
) -> Result<(), PeriodError> {
    let guard = guard.clone();
    let period_id = period_id.to_owned();
    provider
        .transaction(move |txn| {
            Box::pin(async move {
                Ok::<_, DbError>(guard.pin_open(txn, tenant, legal_entity, &period_id).await)
            })
        })
        .await
        .unwrap()
}
