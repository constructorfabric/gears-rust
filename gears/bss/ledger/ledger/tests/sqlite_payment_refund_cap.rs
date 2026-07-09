//! Fast SQLite integration tests for the Slice-3 Phase-2 refund CAP repo
//! guarantees (Group C1), exercised at the repo layer — the load-bearing
//! `payment_settlement` / `payment_allocation_refund` cap CHECKs the refund
//! stage-1 reservation rides. The cheap half of the Phase-2 cap matrix that does
//! not need Docker/testcontainers; the full handler end-to-end (cap reservation +
//! the stage-1 reversal that releases it) is a Postgres-only test
//! (`postgres_refund_cap.rs`, `#[ignore]`).
//!
//! Covered:
//! - **`add_refunded` total money-out cap** (C1): a bump within `settled` succeeds;
//!   a bump past `refunded + clawed_back <= settled` trips
//!   `chk_payment_settlement_moneyout_le_settled` → `MoneyOutCapExceeded`.
//! - **`add_refunded_unallocated` spendable-headroom cap** (C1 / Pattern A): a bump
//!   within `settled − allocated` succeeds; one past `allocated +
//!   refunded_unallocated <= settled` trips
//!   `chk_payment_settlement_alloc_refu_le_settled` → `MoneyOutCapExceeded`.
//! - **`add_allocation_refund_refunded` per-`(payment, invoice)` cap** (C1 / Pattern
//!   B): a bump within the allocated amount succeeds; one past `refunded <=
//!   allocated` trips `chk_par_refunded_le_allocated` → `MoneyOutCapExceeded`.
//! - **decrement returns to norm** (C1 reversal): a negative Δ (the stage-1
//!   reversal release) backs the counter out and re-opens the cap for a later
//!   refund — proving the Group-C reversal frees what initiation reserved.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::similar_names,
    clippy::needless_pass_by_value
)]

use bss_ledger::domain::model::RepoError;
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::PaymentRepo;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

/// Connect an in-memory SQLite + run the migrator (the same harness as the other
/// repo tests).
async fn provider() -> DBProvider<DbError> {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect in-memory sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrator");
    DBProvider::<DbError>::new(db)
}

/// Map an in-txn `RepoError` into the `DbError` the `provider.transaction` closure
/// must return; the `RepoError` is preserved in `DbError::Other` so a test can
/// re-match it after the txn (`Debug`-stamped — matched by substring below).
fn repo_to_db(e: RepoError) -> DbError {
    // Stamp the variant name so the post-txn match can distinguish
    // `MoneyOutCapExceeded` from an infra `Db` error.
    DbError::Other(anyhow::Error::msg(format!("{e:?}")))
}

/// `true` iff the txn error surfaced a `MoneyOutCapExceeded` (the cap CHECK fired).
fn is_cap_exceeded(e: &DbError) -> bool {
    e.to_string().contains("MoneyOutCapExceeded")
}

/// Seed a `payment_settlement` row (`settled`, `allocated`) in a fresh txn. Every
/// other counter starts at 0.
async fn seed_settlement(
    provider: &DBProvider<DbError>,
    scope: &AccessScope,
    tenant: Uuid,
    payment_id: &str,
    settled: i64,
    allocated: i64,
) {
    let scope = scope.clone();
    let payment_id = payment_id.to_owned();
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                PaymentRepo::seed_settlement(tx, &scope, tenant, &payment_id, "USD", settled, 0)
                    .await
                    .map_err(repo_to_db)?;
                if allocated > 0 {
                    PaymentRepo::add_allocated(tx, &scope, tenant, &payment_id, allocated)
                        .await
                        .map_err(repo_to_db)?;
                }
                Ok::<(), DbError>(())
            })
        })
        .await
        .expect("seed settlement");
}

/// Seed a `payment_allocation_refund` row with `allocated_minor = allocated` (the
/// per-`(payment, invoice)` cap basis) via `bump_allocation_refund` in a fresh txn.
async fn seed_allocation_refund(
    provider: &DBProvider<DbError>,
    scope: &AccessScope,
    tenant: Uuid,
    payment_id: &str,
    invoice_id: &str,
    allocated: i64,
) {
    let scope = scope.clone();
    let payment_id = payment_id.to_owned();
    let invoice_id = invoice_id.to_owned();
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                PaymentRepo::bump_allocation_refund(
                    tx,
                    &scope,
                    tenant,
                    &payment_id,
                    &invoice_id,
                    allocated,
                )
                .await
                .map_err(repo_to_db)
            })
        })
        .await
        .expect("seed allocation_refund");
}

/// Run `PaymentRepo::add_refunded(payment_id, delta)` in its own txn, returning the
/// result.
async fn add_refunded(
    provider: &DBProvider<DbError>,
    scope: &AccessScope,
    tenant: Uuid,
    payment_id: &str,
    delta: i64,
) -> Result<(), DbError> {
    let scope = scope.clone();
    let payment_id = payment_id.to_owned();
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                PaymentRepo::add_refunded(tx, &scope, tenant, &payment_id, delta)
                    .await
                    .map_err(repo_to_db)
            })
        })
        .await
}

/// Run `add_refunded_unallocated` in its own txn.
async fn add_refunded_unallocated(
    provider: &DBProvider<DbError>,
    scope: &AccessScope,
    tenant: Uuid,
    payment_id: &str,
    delta: i64,
) -> Result<(), DbError> {
    let scope = scope.clone();
    let payment_id = payment_id.to_owned();
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                PaymentRepo::add_refunded_unallocated(tx, &scope, tenant, &payment_id, delta)
                    .await
                    .map_err(repo_to_db)
            })
        })
        .await
}

/// Run `add_allocation_refund_refunded` in its own txn.
async fn add_allocation_refund_refunded(
    provider: &DBProvider<DbError>,
    scope: &AccessScope,
    tenant: Uuid,
    payment_id: &str,
    invoice_id: &str,
    delta: i64,
) -> Result<(), DbError> {
    let scope = scope.clone();
    let payment_id = payment_id.to_owned();
    let invoice_id = invoice_id.to_owned();
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                PaymentRepo::add_allocation_refund_refunded(
                    tx,
                    &scope,
                    tenant,
                    &payment_id,
                    &invoice_id,
                    delta,
                )
                .await
                .map_err(repo_to_db)
            })
        })
        .await
}

#[tokio::test]
async fn add_refunded_total_moneyout_cap_blocks_over_settled() {
    let provider = provider().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    // Settle 1000, nothing allocated.
    seed_settlement(&provider, &scope, tenant, "pay-1", 1000, 0).await;

    // Within cap: refund 600 of 1000 settled.
    add_refunded(&provider, &scope, tenant, "pay-1", 600)
        .await
        .expect("refund within settled succeeds");

    // Over cap: a further 500 ⇒ refunded 1100 > 1000 settled ⇒ CHECK fires.
    let over = add_refunded(&provider, &scope, tenant, "pay-1", 500)
        .await
        .expect_err("over-settled refund must be rejected by the cap CHECK");
    assert!(
        is_cap_exceeded(&over),
        "expected MoneyOutCapExceeded, got {over}"
    );

    // The remaining headroom (400) still admits a refund up to the cap.
    add_refunded(&provider, &scope, tenant, "pay-1", 400)
        .await
        .expect("refund up to exactly settled succeeds");
    // And one more minor unit is now over.
    let over2 = add_refunded(&provider, &scope, tenant, "pay-1", 1)
        .await
        .expect_err("refunding past exactly-settled must be rejected");
    assert!(
        is_cap_exceeded(&over2),
        "expected MoneyOutCapExceeded, got {over2}"
    );
}

#[tokio::test]
async fn add_refunded_decrement_reopens_cap() {
    let provider = provider().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    seed_settlement(&provider, &scope, tenant, "pay-1", 1000, 0).await;

    // Reserve the whole settled amount.
    add_refunded(&provider, &scope, tenant, "pay-1", 1000)
        .await
        .expect("reserve full cap");
    // A further refund is over-cap.
    assert!(
        is_cap_exceeded(
            &add_refunded(&provider, &scope, tenant, "pay-1", 1)
                .await
                .expect_err("over cap")
        ),
        "cap is exhausted"
    );

    // Release 1000 (the stage-1 reversal decrement, negative Δ) — backs the counter
    // to 0; the nonneg CHECK is NOT tripped (we back out exactly what we reserved).
    add_refunded(&provider, &scope, tenant, "pay-1", -1000)
        .await
        .expect("decrement releases the cap (no underflow on the matched amount)");

    // The cap is fully re-opened: a fresh full refund succeeds again.
    add_refunded(&provider, &scope, tenant, "pay-1", 1000)
        .await
        .expect("cap re-opened after the release");
}

#[tokio::test]
async fn add_refunded_unallocated_headroom_cap_blocks_when_allocated() {
    let provider = provider().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    // Settle 1000, allocate 700 ⇒ spendable headroom for a Pattern-A refund is 300.
    seed_settlement(&provider, &scope, tenant, "pay-1", 1000, 700).await;

    // Within headroom: refund_unallocated 300 ⇒ allocated 700 + 300 = 1000 <= 1000.
    add_refunded_unallocated(&provider, &scope, tenant, "pay-1", 300)
        .await
        .expect("refund within spendable headroom succeeds");

    // Over headroom: a further 1 ⇒ 700 + 301 = 1001 > 1000 ⇒ CHECK fires (the
    // refunded on-account cash can no longer also be allocated).
    let over = add_refunded_unallocated(&provider, &scope, tenant, "pay-1", 1)
        .await
        .expect_err("over-headroom refund_unallocated must be rejected");
    assert!(
        is_cap_exceeded(&over),
        "expected MoneyOutCapExceeded, got {over}"
    );
}

#[tokio::test]
async fn add_allocation_refund_per_invoice_cap_blocks_over_allocated() {
    let provider = provider().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    seed_settlement(&provider, &scope, tenant, "pay-1", 1000, 800).await;
    // The (payment, invoice) pair was allocated 800.
    seed_allocation_refund(&provider, &scope, tenant, "pay-1", "inv-9", 800).await;

    // Within the per-invoice cap: refund 800.
    add_allocation_refund_refunded(&provider, &scope, tenant, "pay-1", "inv-9", 800)
        .await
        .expect("refund up to the allocated amount succeeds");

    // Over the per-invoice cap: a further 1 ⇒ refunded 801 > 800 allocated ⇒ CHECK.
    let over = add_allocation_refund_refunded(&provider, &scope, tenant, "pay-1", "inv-9", 1)
        .await
        .expect_err("per-(payment, invoice) over-refund must be rejected");
    assert!(
        is_cap_exceeded(&over),
        "expected MoneyOutCapExceeded, got {over}"
    );

    // Release (the stage-1 reversal decrement) re-opens the per-invoice cap.
    add_allocation_refund_refunded(&provider, &scope, tenant, "pay-1", "inv-9", -800)
        .await
        .expect("decrement releases the per-invoice cap");
    add_allocation_refund_refunded(&provider, &scope, tenant, "pay-1", "inv-9", 800)
        .await
        .expect("per-invoice cap re-opened after the release");
}

#[tokio::test]
async fn add_allocation_refund_absent_row_is_db_error() {
    let provider = provider().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    seed_settlement(&provider, &scope, tenant, "pay-1", 1000, 0).await;
    // No payment_allocation_refund row for (pay-1, inv-none) ⇒ a Pattern-B refund of
    // an unallocated receipt is an upstream contract violation (rows_affected == 0).
    let err = add_allocation_refund_refunded(&provider, &scope, tenant, "pay-1", "inv-none", 100)
        .await
        .expect_err("a Pattern-B refund of a never-allocated (payment, invoice) must fail");
    // Not a cap violation — a plain Db error (the row is absent, not over-cap).
    assert!(
        !is_cap_exceeded(&err),
        "absent allocation_refund row is a Db error, not a cap violation: {err}"
    );
}
