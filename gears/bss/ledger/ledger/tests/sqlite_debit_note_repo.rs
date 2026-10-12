//! Fast SQLite integration tests for the Slice-3 debit-note repo guarantees
//! (Group D3), exercised at the repo layer — the cheap half of the Phase-1
//! integration matrix that does not need Docker/testcontainers. The full handler
//! end-to-end (chart provisioning + projector + post engine + schedule build) is a
//! Postgres-only test (`postgres_debit_note.rs`, `#[ignore]`).
//!
//! Covered:
//! - **`add_debit_note_total` raises the headroom** (D3 / AC #24): a debit note
//!   bumps `debit_note_total`, which is the RHS of the headroom check
//!   (`credit_note_total <= original_total + debit_note_total`),
//!   so the cap for *later credit notes* grows — a credit note that would have been
//!   over-cap before the debit note now fits, and the one-unit-over is still
//!   rejected (proving the raise is exactly the debit-note amount).
//! - **`add_debit_note_total` requires a seeded row** (invariant): a bump before
//!   the first-touch seed is a `Db` error.
//! - **`insert_debit_note` round-trips** (D3): a `debit_note` row persists and a
//!   duplicate `(tenant, debit_note_id)` collides on the PK.

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
use bss_ledger::infra::storage::repo::AdjustmentRepo;
use bss_ledger::infra::storage::repo::adjustment_repo::NewDebitNote;
use bss_ledger_sdk::{CurrencySpec, PostedMoney, parse_decimal};
use sea_orm_migration::MigratorTrait;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

/// USD at scale 2 — the only currency these repo-level tests use.
fn usd() -> CurrencySpec {
    CurrencySpec::try_new("USD".to_owned(), 2).expect("USD spec")
}

/// Validated USD money from canonical major-unit text (`"10"` = ten dollars).
fn money(text: &str) -> PostedMoney {
    PostedMoney::try_new(parse_decimal(text).expect("decimal"), usd()).expect("posted money")
}

/// Connect an in-memory SQLite + run the migrator (the same harness as the
/// credit-note repo test).
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
/// must return (it fixes the closure error to `DbError` and rolls back on `Err`);
/// the `RepoError` `Debug` is stamped into `DbError::Other` so a caller can assert
/// the variant by a contains-check.
fn repo_to_db(e: RepoError) -> DbError {
    DbError::Other(anyhow::Error::msg(format!("{e:?}")))
}

/// Assert a rolled-back `provider.transaction` result IS the headroom cap CHECK
/// (`RepoError::MoneyOutCapExceeded`).
fn assert_cap_exceeded(res: &Result<(), DbError>) {
    let err = res
        .as_ref()
        .expect_err("expected a cap-CHECK rejection")
        .to_string();
    assert!(
        err.contains("MoneyOutCapExceeded"),
        "expected MoneyOutCapExceeded, got: {err}"
    );
}

#[tokio::test]
async fn debit_note_total_raises_headroom_for_credit_notes() {
    let provider = provider().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let invoice = "inv-dn-headroom";

    // Seed original_total = 10 (no debit notes ⇒ headroom = 10).
    let scope_a = scope.clone();
    let repo = AdjustmentRepo::new(provider.clone());
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                repo.seed_exposure_first_touch(tx, &scope_a, tenant, invoice, &money("10"))
                    .await
                    .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))
            })
        })
        .await
        .expect("seed exposure");

    // Raise the headroom by a 5 debit note ⇒ headroom = 10 + 5 = 15.
    let scope_b = scope.clone();
    let repo = AdjustmentRepo::new(provider.clone());
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                repo.add_debit_note_total(tx, &scope_b, tenant, invoice, &money("5"))
                    .await
                    .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))
            })
        })
        .await
        .expect("debit note raises headroom");

    // A 15 credit note now fits exactly to the raised cap (10 original + 5
    // debit) — it WOULD have been over-cap (15 > 10) before the debit note.
    let scope_c = scope.clone();
    let repo = AdjustmentRepo::new(provider.clone());
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                repo.add_credit_note_total(tx, &scope_c, tenant, invoice, &money("15"))
                    .await
                    .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))
            })
        })
        .await
        .expect("credit note fits the debit-note-raised headroom");

    // One cent over the raised cap (running credit 15.01 > 15) is rejected —
    // confirms the headroom was raised by exactly the debit-note amount.
    let scope_d = scope.clone();
    let repo = AdjustmentRepo::new(provider.clone());
    let res = provider
        .transaction(move |tx| {
            Box::pin(async move {
                repo.add_credit_note_total(tx, &scope_d, tenant, invoice, &money("0.01"))
                    .await
                    .map_err(repo_to_db)
            })
        })
        .await;
    assert_cap_exceeded(&res);
}

#[tokio::test]
async fn add_debit_note_total_requires_a_seeded_row() {
    let provider = provider().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);

    // No seed first ⇒ the bump matches no row and is a Db invariant error.
    let scope_a = scope.clone();
    let repo = AdjustmentRepo::new(provider.clone());
    let res = provider
        .transaction(move |tx| {
            Box::pin(async move {
                repo.add_debit_note_total(tx, &scope_a, tenant, "inv-unseeded", &money("1"))
                    .await
                    .map_err(repo_to_db)
            })
        })
        .await;
    let err = res
        .as_ref()
        .expect_err("expected a not-seeded error")
        .to_string();
    assert!(
        err.contains("not seeded"),
        "expected not-seeded Db error, got: {err}"
    );
}

#[tokio::test]
async fn debit_note_row_round_trips() {
    let provider = provider().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);

    let note = NewDebitNote {
        tenant_id: tenant,
        debit_note_id: "dn-rt".to_owned(),
        origin_invoice_id: "inv-1".to_owned(),
        amount: money("11"),
        recognized_part: money("6"),
        deferred_part: money("4"),
        created_at_utc: OffsetDateTime::now_utc(),
    };
    let scope_a = scope.clone();
    let repo = AdjustmentRepo::new(provider.clone());
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                repo.insert_debit_note(tx, &scope_a, &note)
                    .await
                    .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))
            })
        })
        .await
        .expect("insert debit_note");

    // The insert committed without error — the round-trip success. Field-level
    // persistence (incl-tax amount + ex-tax split parts) is asserted by the engine
    // end-to-end PG test (`postgres_debit_note`); here the composite-PK guard is
    // asserted by the duplicate-insert collision below (mirrors the credit_note
    // round-trip in `sqlite_adjustment_repo`).

    // A duplicate PK insert collides (the engine's idempotency claim normally
    // short-circuits this before the sidecar; here we assert the PK guard holds).
    let note2 = NewDebitNote {
        tenant_id: tenant,
        debit_note_id: "dn-rt".to_owned(),
        origin_invoice_id: "inv-1".to_owned(),
        amount: money("0.01"),
        recognized_part: money("0.01"),
        deferred_part: money("0"),
        created_at_utc: OffsetDateTime::now_utc(),
    };
    let scope_b = scope.clone();
    let repo = AdjustmentRepo::new(provider.clone());
    let dup = provider
        .transaction(move |tx| {
            Box::pin(async move {
                repo.insert_debit_note(tx, &scope_b, &note2)
                    .await
                    .map_err(repo_to_db)
            })
        })
        .await;
    assert!(
        dup.is_err(),
        "duplicate debit_note_id must collide on the PK"
    );
}
