//! Postgres-only repo tests for `PayerStateRepo` (the payer-lifecycle row
//! `bss.ledger_payer_state` + the outstanding-balance check over
//! `bss.ledger_ar_payer_balance`, VHP-1852 Phase 2). Ignored by default; run with
//! `cargo test -p bss-ledger --test postgres_payer_state -- --ignored`.
//!
//! Covers: (a) `close` upserts the row to CLOSED, stamping the approver + the
//! open-balance marker, and `read` reads it back; (b) `close` is idempotent — a
//! re-close lands on the same PK and updates the marker; (c) SQL-level BOLA — a
//! foreign-tenant scope cannot read another tenant's payer-state row; (d)
//! `has_outstanding_balance` is false with no AR grain and true once a non-zero
//! `ledger_ar_payer_balance` grain exists (the closure dual-control trigger).

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic
)]

use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::PayerStateRepo;
use sea_orm::{ConnectionTrait, Database, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

fn pg(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

/// Spin up a Postgres container, migrate `bss`, return the raw connection + a
/// `bss`-search-path `DBProvider` (mirrors `postgres_payments.rs`).
async fn boot() -> (
    testcontainers_modules::testcontainers::ContainerAsync<Postgres>,
    sea_orm::DatabaseConnection,
    DBProvider<DbError>,
) {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let raw = Database::connect(&url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();

    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    let provider = DBProvider::<DbError>::new(tdb);
    (container, raw, provider)
}

/// `close` upserts the payer-lifecycle row to CLOSED (stamping the approver + the
/// open-balance marker); `read` reads it back. A re-close is idempotent and
/// updates the marker on the same PK.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn close_upserts_closed_and_is_idempotent() {
    let (_c, _raw, provider) = boot().await;
    let tenant = Uuid::now_v7();
    let payer = Uuid::now_v7();
    let approver = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let repo = PayerStateRepo::new(provider.clone());

    // Absence ⇒ OPEN (no row).
    assert!(
        repo.read(&scope, tenant, payer)
            .await
            .expect("read")
            .is_none(),
        "no row before close ⇒ OPEN"
    );

    // Close with an open balance.
    repo.close(&scope, tenant, payer, approver, true)
        .await
        .expect("close");
    let row = repo
        .read(&scope, tenant, payer)
        .await
        .expect("read")
        .expect("row present after close");
    assert_eq!(row.lifecycle_state, "CLOSED");
    assert!(row.closed_with_open_balance);
    assert_eq!(row.approved_by, Some(approver));

    // Re-close (idempotent): same PK, marker updated to false.
    let approver2 = Uuid::now_v7();
    repo.close(&scope, tenant, payer, approver2, false)
        .await
        .expect("re-close");
    let row = repo
        .read(&scope, tenant, payer)
        .await
        .expect("read")
        .expect("row present after re-close");
    assert_eq!(row.lifecycle_state, "CLOSED");
    assert!(
        !row.closed_with_open_balance,
        "the re-close updated the marker"
    );
    assert_eq!(row.approved_by, Some(approver2));
}

/// SQL-level BOLA: a payer-state row closed for tenant A is invisible to a
/// tenant-B `AccessScope` (own scope sees it ⇒ empty for B means scoped-out).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn payer_state_is_invisible_to_a_foreign_tenant_scope() {
    let (_c, _raw, provider) = boot().await;
    let tenant_a = Uuid::now_v7();
    let tenant_b = Uuid::now_v7();
    let payer = Uuid::now_v7();
    let own = AccessScope::for_tenant(tenant_a);
    let foreign = AccessScope::for_tenant(tenant_b);
    let repo = PayerStateRepo::new(provider.clone());

    repo.close(&own, tenant_a, payer, Uuid::now_v7(), false)
        .await
        .expect("close A");

    assert!(
        repo.read(&own, tenant_a, payer)
            .await
            .expect("read own")
            .is_some(),
        "tenant A's own scope must see its payer-state row"
    );
    assert!(
        repo.read(&foreign, tenant_a, payer)
            .await
            .expect("read foreign")
            .is_none(),
        "a tenant-B scope must NOT read tenant A's payer-state row (SQL-level BOLA)"
    );
}

/// `has_outstanding_balance` is false with no AR grain and true once a non-zero
/// `ledger_ar_payer_balance` grain exists — the trigger that routes a payer
/// closure through dual-control.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn has_outstanding_balance_reflects_the_ar_grain() {
    let (_c, raw, provider) = boot().await;
    let tenant = Uuid::now_v7();
    let payer = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let repo = PayerStateRepo::new(provider.clone());

    // No grain ⇒ nothing outstanding.
    assert!(
        !repo
            .has_outstanding_balance(&scope, tenant, payer)
            .await
            .expect("has_outstanding_balance"),
        "no AR grain ⇒ no outstanding balance"
    );

    // Seed a non-zero AR grain for the payer.
    let acct = Uuid::now_v7();
    raw.execute(pg(format!(
        "INSERT INTO bss.ledger_ar_payer_balance \
         (tenant_id, payer_tenant_id, account_id, currency, balance_minor, version) \
         VALUES ('{tenant}','{payer}','{acct}','USD',500,0)"
    )))
    .await
    .expect("seed ar payer balance");

    assert!(
        repo.has_outstanding_balance(&scope, tenant, payer)
            .await
            .expect("has_outstanding_balance"),
        "a non-zero AR grain ⇒ outstanding balance (closure needs dual-control)"
    );
}
