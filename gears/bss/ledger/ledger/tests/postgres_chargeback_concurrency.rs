//! Postgres-only **concurrency** test for the chargeback slice (Group D): two
//! concurrent `ChargebackService::record_phase` posts on the SAME dispute
//! serialize under `SERIALIZABLE` (caller-retry, decision O). Ignored by default;
//! run with
//! `cargo test -p cf-gears-bss-ledger --test postgres_chargeback_concurrency -- --ignored`.
//!
//! Covers:
//! - #D-1: a racing pair recording the SAME `lost` phase (same
//!   `dispute_id:cycle:phase`) on one opened `CASH_HOLD` dispute must land EXACTLY
//!   ONE ledger effect — the `(tenant, CHARGEBACK, business_id)` dedup admits one
//!   winner; the loser replays the winner's finalized entry. The forfeiture posts
//!   once and `clawed_back` is NEVER double-counted (no double clawback).
//! - #D-2: a racing `won` and `lost` on one opened cycle: exactly one resolves the
//!   dispute; the other is refused as `InvalidDisputeTransition`.
//! - #D-3: #D-2 made deterministic with an advisory-lock gate: the outcome that
//!   posts after the other one committed is refused as `InvalidDisputeTransition`
//!   (not `NegativeBalance`) and raises no negative-balance alarm.
//! - #D-4: the in-txn dispute check passes a still-open dispute, so a real
//!   `NegativeBalance` on it still surfaces (with its alarm).
//! - #D-5: a chargeback drain that replays an outcome another drain applied
//!   counts it as `replayed`, not `applied`, with no second effect.
//! - #D-6: #D-1 made deterministic with two gates: a repeat of the same `lost`
//!   that starts before the first `lost` commits, and whose dispute read sees
//!   it committed, replays the first entry instead of being refused as
//!   `InvalidDisputeTransition`.
//!
//! Mirrors `postgres_payment_concurrency.rs`'s `retry_on_serialization` +
//! `tokio::join!` shape (and `postgres_payments::allocate_replay_makes_no_
//! duplicate_rows`, the same-key racing-claim pattern).
//!
//! Self-contained: re-declares the small `boot` / `setup_seller` / `settle`
//! helpers it needs (mirrors `postgres_payment_concurrency.rs`), so it does not
//! depend on `postgres_chargebacks.rs`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic
)]

use std::sync::Arc;
use std::time::Duration;

use bss_ledger::domain::error::DomainError;
use bss_ledger::domain::model::{AccountRow, CurrencyScaleRow};
use bss_ledger::domain::payment::chargeback::{DisputePhase, FundsAtOpen};
use bss_ledger::domain::payment::settlement::SettlementInput;
use bss_ledger::domain::ports::metrics::NoopLedgerMetrics;
use bss_ledger::infra::events::alarm_catalog;
use bss_ledger::infra::events::payloads::AlarmCategory;
use bss_ledger::infra::events::publisher::LedgerEventPublisher;
use bss_ledger::infra::metrics::test_harness::MetricsHarness;
use bss_ledger::infra::payment::chargeback::{
    ChargebackOutcome, ChargebackRequest, ChargebackService,
};
use bss_ledger::infra::payment::settle::SettlementService;
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::PaymentRepo;
use bss_ledger::infra::storage::repo::ReferenceRepo;
use bss_ledger_sdk::{AccountClass, CurrencySpec, PostedMoney, Side, canonical_decimal};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, Database, Statement, TransactionTrait};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use time::OffsetDateTime;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

fn pg(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

/// The USD@2 currency spec every fixture here posts in.
fn usd_spec() -> CurrencySpec {
    CurrencySpec::try_new("USD".to_owned(), 2).unwrap()
}

/// A USD scale-2 posting from a cent count (`1234` ⇒ `12.34`).
fn usd(cents: i64) -> PostedMoney {
    PostedMoney::try_new(Decimal::new(cents, 2), usd_spec()).unwrap()
}

/// The canonical stored text of a USD cent count (`970` ⇒ `"9.7"`).
fn text(cents: i64) -> String {
    canonical_decimal(Decimal::new(cents, 2))
}

/// Boot a container, migrate on a raw connection, and return a `bss`-search-path
/// `DBProvider`. Mirrors `postgres_payment_concurrency::boot`.
async fn boot() -> (
    testcontainers_modules::testcontainers::ContainerAsync<Postgres>,
    sea_orm::DatabaseConnection,
    DBProvider<DbError>,
) {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let raw = Database::connect(&url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();

    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    let provider = DBProvider::<DbError>::new(tdb);
    (container, raw, provider)
}

/// Provisioned seller for the chargeback concurrency test (the cash-hold dispute
/// classes + the settle pool).
struct Seller {
    tenant: Uuid,
    payer: Uuid,
    cash: Uuid,
    dispute_hold: Uuid,
    dispute_loss: Uuid,
    period_id: String,
}

fn account(tenant: Uuid, id: Uuid, class: AccountClass, normal: Side) -> AccountRow {
    AccountRow {
        account_id: id,
        tenant_id: tenant,
        legal_entity_id: tenant,
        account_class: class.as_str().to_owned(),
        currency: "USD".to_owned(),
        revenue_stream: None,
        normal_side: normal.as_str().to_owned(),
        may_go_negative: false,
        lifecycle_state: "OPEN".to_owned(),
    }
}

/// Provision a seller: USD@2 scale, an OPEN fiscal period for the current month,
/// and the cash-hold dispute chart accounts (CASH_CLEARING debit, UNALLOCATED
/// credit, DISPUTE_HOLD debit, DISPUTE_LOSS_EXPENSE debit). Mirrors
/// `postgres_payment_concurrency::setup_seller`.
async fn setup_seller(raw: &sea_orm::DatabaseConnection, provider: &DBProvider<DbError>) -> Seller {
    let now = OffsetDateTime::now_utc();
    let s = Seller {
        tenant: Uuid::now_v7(),
        payer: Uuid::now_v7(),
        cash: Uuid::now_v7(),
        dispute_hold: Uuid::now_v7(),
        dispute_loss: Uuid::now_v7(),
        period_id: bss_ledger::domain::instant::yyyymm(now),
    };

    let reference = ReferenceRepo::new(provider.clone());
    reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: s.tenant,
            currency: "USD".to_owned(),
            currency_scale: 2,
            source: "iso".to_owned(),
        })
        .await
        .unwrap();
    raw.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_fiscal_period (tenant_id, legal_entity_id, period_id, fiscal_tz, status)
         VALUES ('{}','{}','{}','UTC','OPEN')",
        s.tenant, s.tenant, s.period_id
    )))
    .await
    .unwrap();

    for row in [
        account(s.tenant, s.cash, AccountClass::CashClearing, Side::Debit),
        account(
            s.tenant,
            Uuid::now_v7(),
            AccountClass::Unallocated,
            Side::Credit,
        ),
        account(
            s.tenant,
            s.dispute_hold,
            AccountClass::DisputeHold,
            Side::Debit,
        ),
        account(
            s.tenant,
            s.dispute_loss,
            AccountClass::DisputeLossExpense,
            Side::Debit,
        ),
    ] {
        reference.insert_account(row).await.unwrap();
    }
    s
}

fn chargeback_svc(provider: &DBProvider<DbError>) -> ChargebackService {
    ChargebackService::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(NoopLedgerMetrics),
    )
}

/// A chargeback service whose publisher counts invariant alarms into `metrics`
/// (`ledger_alarm_total`).
fn recording_chargeback_svc(
    provider: &DBProvider<DbError>,
    metrics: &MetricsHarness,
) -> ChargebackService {
    ChargebackService::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::with_metrics(Arc::new(
            metrics.metrics(),
        ))),
        Arc::new(NoopLedgerMetrics),
    )
}

/// The `ledger_alarm_total` labels of a `NEGATIVE_BALANCE_VIOLATION` alarm.
fn negative_balance_alarm() -> [(&'static str, &'static str); 2] {
    let category = AlarmCategory::NegativeBalanceViolation;
    [
        ("category", category.as_str()),
        ("severity", alarm_catalog::severity(category).as_str()),
    ]
}

/// The longest the gate tests wait on one step while the gate is held. A
/// locking regression then fails the test instead of hanging it.
const GATE_WAIT: Duration = Duration::from_secs(60);

/// Poll `pg_locks` until a lock request that matches `filter` (an SQL predicate
/// over `pg_locks`) waits, for at most 30 s. Returns whether one did.
async fn lock_request_waits(raw: &sea_orm::DatabaseConnection, filter: &str) -> bool {
    for _ in 0..1500 {
        let waiting = raw
            .query_one_raw(pg(format!(
                "SELECT COUNT(*) FROM pg_locks WHERE NOT granted AND {filter}"
            )))
            .await
            .unwrap()
            .map_or(0, |r| r.try_get_by_index::<i64>(0).unwrap());
        if waiting > 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// A cash-hold dispute request for `PAY-<suffix>` / `DSP-<suffix>`, cycle 1,
/// disputing the full 10.00.
fn dispute_request(s: &Seller, suffix: &str, phase: DisputePhase) -> ChargebackRequest {
    ChargebackRequest {
        tenant_id: s.tenant,
        payer_tenant_id: s.payer,
        payment_id: format!("PAY-{suffix}"),
        dispute_id: format!("DSP-{suffix}"),
        invoice_id: None,
        cycle: 1,
        phase,
        funds_at_open: FundsAtOpen::Withheld,
        disputed_amount: usd(1000),
        effective_at: None,
    }
}

/// The dispute's stored `last_phase`.
async fn dispute_phase(
    raw: &sea_orm::DatabaseConnection,
    s: &Seller,
    dispute_id: &str,
) -> Option<String> {
    raw.query_one_raw(pg(format!(
        "SELECT last_phase FROM bss.ledger_dispute \
         WHERE tenant_id='{}' AND dispute_id='{dispute_id}'",
        s.tenant
    )))
    .await
    .unwrap()
    .map(|r| r.try_get_by_index::<String>(0).unwrap())
}

/// Settle `gross` (fee 0) for `payment_id` — funds CASH_CLEARING + seeds the
/// settlement counter (`settled = gross`) the clawback cap nets against.
async fn settle(provider: &DBProvider<DbError>, s: &Seller, payment_id: &str, gross: i64) {
    SettlementService::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(NoopLedgerMetrics),
    )
    .settle(
        &SecurityContext::anonymous(),
        &AccessScope::for_tenant(s.tenant),
        SettlementInput {
            tenant_id: s.tenant,
            payer_tenant_id: s.payer,
            payment_id: payment_id.to_owned(),
            gross: usd(gross),

            fee: usd(0),
            effective_at: None,
        },
    )
    .await
    .expect("settle must succeed");
}

/// Client-side retry of a service call on a projector-level serialization
/// conflict (40001 stringified into `DomainError::Internal("…serialize…")`).
/// Decision O defers recompute-on-retry to the CALLER, so the test models what
/// the SDK/client does: re-run the WHOLE operation until it commits or hits a
/// real business rejection. A genuine deadlock (40P01) is NOT a serialization
/// conflict and propagates — failing the test loudly (the deadlock-freedom
/// guarantee). Copied verbatim from `postgres_payment_concurrency.rs`.
async fn retry_on_serialization<F, Fut, T>(mut op: F) -> Result<T, DomainError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, DomainError>>,
{
    for _ in 0..20 {
        match op().await {
            Err(DomainError::Internal(m)) if m.contains("serialize") => {}
            other => return other,
        }
    }
    op().await
}

/// Read a chart account's cached canonical `balance` text (or `None` if never posted to).
async fn account_balance(
    raw: &sea_orm::DatabaseConnection,
    s: &Seller,
    account: Uuid,
) -> Option<String> {
    raw.query_one_raw(pg(format!(
        "SELECT balance FROM bss.ledger_account_balance \
         WHERE tenant_id='{}' AND account_id='{}' AND currency='USD'",
        s.tenant, account
    )))
    .await
    .unwrap()
    .map(|r| r.try_get_by_index::<String>(0).unwrap())
}

/// Chargeback #D-1: two concurrent records of the SAME `lost` phase on ONE
/// opened `CASH_HOLD` dispute (same `dispute_id:cycle:phase`, so the dedup key
/// COLLIDES) must land EXACTLY ONE ledger effect. Under `SERIALIZABLE` the two
/// posts serialize at the dispute row + the clawback counter; the
/// `(tenant, CHARGEBACK, business_id)` dedup admits one winner and the loser
/// replays the winner's finalized entry (the client retries a projector-level
/// 40001 via `retry_on_serialization`). The forfeiture posts once
/// (DISPUTE_LOSS_EXPENSE = 10.00, DISPUTE_HOLD = 0) and `clawed_back` is
/// NEVER double-counted (it lands at EXACTLY the disputed 1000 — no double
/// clawback). Mirrors `postgres_payments::allocate_replay_makes_no_duplicate_
/// rows`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn concurrent_lost_on_one_dispute_claws_back_once() {
    let (_c, raw, provider) = boot().await;
    let s = setup_seller(&raw, &provider).await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // Settle 1000 (funds CASH_CLEARING + the settlement counter) and open a
    // cash-hold dispute (moves the 1000 into DISPUTE_HOLD).
    settle(&provider, &s, "PAY-CC-1", 1000).await;
    chargeback_svc(&provider)
        .record_phase(
            &ctx,
            &scope,
            ChargebackRequest {
                tenant_id: s.tenant,
                payer_tenant_id: s.payer,
                payment_id: "PAY-CC-1".to_owned(),
                dispute_id: "DSP-CC-1".to_owned(),
                invoice_id: None,
                cycle: 1,
                phase: DisputePhase::Opened,
                funds_at_open: FundsAtOpen::Withheld,
                disputed_amount: usd(1000),
                effective_at: None,
            },
        )
        .await
        .expect("opened cash-hold");

    // The SAME `lost` phase on both racing requests ⇒ the dedup key collides; at
    // most one may post the forfeiture, the other replays it.
    let request = || ChargebackRequest {
        tenant_id: s.tenant,
        payer_tenant_id: s.payer,
        payment_id: "PAY-CC-1".to_owned(),
        dispute_id: "DSP-CC-1".to_owned(),
        invoice_id: None,
        cycle: 1,
        phase: DisputePhase::Lost,
        funds_at_open: FundsAtOpen::Withheld,
        disputed_amount: usd(1000),
        effective_at: None,
    };

    let svc_a = chargeback_svc(&provider);
    let svc_b = chargeback_svc(&provider);
    let scope_a = scope.clone();
    let scope_b = scope.clone();
    let ctx_a = SecurityContext::anonymous();
    let ctx_b = SecurityContext::anonymous();
    let (ra, rb) = tokio::join!(
        async move { retry_on_serialization(|| svc_a.record_phase(&ctx_a, &scope_a, request())).await },
        async move { retry_on_serialization(|| svc_b.record_phase(&ctx_b, &scope_b, request())).await },
    );

    // Both sides resolved cleanly (a deadlock would have propagated out of
    // `retry_on_serialization`); at least one made progress. A `lost` is an inline
    // post, so each Ok is a `Recorded` (one fresh, the other a replay).
    for r in [&ra, &rb] {
        match r {
            Ok(ChargebackOutcome::Recorded(_)) => {}
            Ok(ChargebackOutcome::Queued(q)) => {
                panic!("a lost on an opened dispute posts inline, got Queued: {q:?}")
            }
            Err(e) => panic!("a racing lost must succeed (post or replay), got: {e:?}"),
        }
    }
    let replays = i32::from(matches!(&ra, Ok(ChargebackOutcome::Recorded(r)) if r.replayed))
        + i32::from(matches!(&rb, Ok(ChargebackOutcome::Recorded(r)) if r.replayed));
    assert!(
        replays >= 1,
        "exactly one side wins; the other replays the finalized entry: {ra:?} / {rb:?}"
    );

    // INVARIANT: the forfeiture posted EXACTLY once — DISPUTE_LOSS_EXPENSE = 1000,
    // DISPUTE_HOLD emptied — and `clawed_back` was not double-counted.
    assert_eq!(
        account_balance(&raw, &s, s.dispute_loss).await,
        Some(text(1000)),
        "the forfeiture booked exactly once into DISPUTE_LOSS_EXPENSE"
    );
    assert_eq!(
        account_balance(&raw, &s, s.dispute_hold).await,
        Some(text(0)),
        "the hold is emptied exactly once"
    );

    let row = PaymentRepo::new(provider.clone())
        .read_settlement(&scope, s.tenant, "PAY-CC-1")
        .await
        .unwrap()
        .expect("settlement row present");
    assert_eq!(
        row.clawed_back,
        usd(1000),
        "clawed_back reflects exactly one clawback (10.00), never doubled"
    );
    assert!(
        row.refunded.amount() + row.clawed_back.amount() <= row.settled.amount(),
        "the total money-out cap held: refunded ({}) + clawed ({}) <= settled ({})",
        row.refunded.amount(),
        row.clawed_back.amount(),
        row.settled.amount()
    );

    // The dispute resolved to LOST (advanced once).
    let phase = raw
        .query_one_raw(pg(format!(
            "SELECT last_phase FROM bss.ledger_dispute \
             WHERE tenant_id='{}' AND dispute_id='DSP-CC-1'",
            s.tenant
        )))
        .await
        .unwrap()
        .map(|r| r.try_get_by_index::<String>(0).unwrap());
    assert_eq!(
        phase,
        Some("LOST".to_owned()),
        "the dispute advanced to LOST"
    );
}

/// Chargeback #D-2 (the won/lost outcome race — regression for the missing
/// `(last_phase, cycle)` predicate on `dispute_advance`): a `won` and a `lost` on
/// the SAME opened cycle carry DISTINCT dedup keys (`…:won` vs `…:lost`), so BOTH
/// clear the dedup gate AND the out-of-txn transition guard and reach the sidecar.
/// Exactly ONE may resolve the dispute; the loser MUST be rejected as a clean
/// `InvalidDisputeTransition` — never commit a second outcome entry over the
/// first, never surface as a 500. Under `SERIALIZABLE` one commits; the other
/// finds the row no longer `OPENED` at this cycle (its post's in-txn dispute
/// check before projection, or the out-of-txn guard on a later attempt) and is
/// rejected. #D-3 below forces the in-txn path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn concurrent_won_and_lost_resolves_one_outcome() {
    let (_c, raw, provider) = boot().await;
    let s = setup_seller(&raw, &provider).await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    settle(&provider, &s, "PAY-CC-2", 1000).await;
    chargeback_svc(&provider)
        .record_phase(
            &ctx,
            &scope,
            ChargebackRequest {
                tenant_id: s.tenant,
                payer_tenant_id: s.payer,
                payment_id: "PAY-CC-2".to_owned(),
                dispute_id: "DSP-CC-2".to_owned(),
                invoice_id: None,
                cycle: 1,
                phase: DisputePhase::Opened,
                funds_at_open: FundsAtOpen::Withheld,
                disputed_amount: usd(1000),
                effective_at: None,
            },
        )
        .await
        .expect("opened cash-hold");

    // Distinct dedup keys (won vs lost) — neither is deduped against the other.
    let won_req = || ChargebackRequest {
        tenant_id: s.tenant,
        payer_tenant_id: s.payer,
        payment_id: "PAY-CC-2".to_owned(),
        dispute_id: "DSP-CC-2".to_owned(),
        invoice_id: None,
        cycle: 1,
        phase: DisputePhase::Won,
        funds_at_open: FundsAtOpen::Withheld,
        disputed_amount: usd(1000),
        effective_at: None,
    };
    let lost_req = || ChargebackRequest {
        tenant_id: s.tenant,
        payer_tenant_id: s.payer,
        payment_id: "PAY-CC-2".to_owned(),
        dispute_id: "DSP-CC-2".to_owned(),
        invoice_id: None,
        cycle: 1,
        phase: DisputePhase::Lost,
        funds_at_open: FundsAtOpen::Withheld,
        disputed_amount: usd(1000),
        effective_at: None,
    };

    let svc_a = chargeback_svc(&provider);
    let svc_b = chargeback_svc(&provider);
    let scope_a = scope.clone();
    let scope_b = scope.clone();
    let ctx_a = SecurityContext::anonymous();
    let ctx_b = SecurityContext::anonymous();
    let (won, lost) = tokio::join!(
        async move { retry_on_serialization(|| svc_a.record_phase(&ctx_a, &scope_a, won_req())).await },
        async move { retry_on_serialization(|| svc_b.record_phase(&ctx_b, &scope_b, lost_req())).await },
    );

    // EXACTLY ONE outcome resolved; the other is a clean invalid-transition
    // rejection (NOT a second commit, NOT an Internal/500).
    let oks = i32::from(won.is_ok()) + i32::from(lost.is_ok());
    assert_eq!(
        oks, 1,
        "exactly one outcome may win: won={won:?} lost={lost:?}"
    );
    for r in [&won, &lost] {
        if let Err(e) = r {
            assert!(
                matches!(e, DomainError::InvalidDisputeTransition(_)),
                "the losing outcome must be a clean InvalidDisputeTransition, got {e:?}"
            );
        }
    }

    // The dispute sits at EXACTLY ONE terminal phase, matching the winner; the
    // hold is emptied once either way (won releases it, lost forfeits it).
    let winner_phase = if won.is_ok() { "WON" } else { "LOST" };
    let phase = raw
        .query_one_raw(pg(format!(
            "SELECT last_phase FROM bss.ledger_dispute \
             WHERE tenant_id='{}' AND dispute_id='DSP-CC-2'",
            s.tenant
        )))
        .await
        .unwrap()
        .map(|r| r.try_get_by_index::<String>(0).unwrap());
    assert_eq!(
        phase.as_deref(),
        Some(winner_phase),
        "the dispute resolved to the winning outcome exactly once"
    );
    assert_eq!(
        account_balance(&raw, &s, s.dispute_hold).await,
        Some(text(0)),
        "DISPUTE_HOLD drained by the single applied outcome"
    );
    // The clawback counter reflects the winner: 0 on a won, the full 1000 on a lost.
    let want_clawed = if won.is_ok() { 0 } else { 1000 };
    let row = PaymentRepo::new(provider.clone())
        .read_settlement(&scope, s.tenant, "PAY-CC-2")
        .await
        .unwrap()
        .expect("settlement row present");
    assert_eq!(
        row.clawed_back,
        usd(want_clawed),
        "clawed_back reflects the single winning outcome (won=>0, lost=>10.00)"
    );
}

/// Chargeback #D-3 (the #D-2 race, made deterministic): an outcome that passes
/// its out-of-txn transition guard while the dispute is still `OPENED`, and whose
/// post then runs after the other outcome has committed, MUST be refused as
/// `InvalidDisputeTransition` — not as the projector's `NegativeBalance` on the
/// `DISPUTE_HOLD` the winner already emptied — and must raise no
/// `NEGATIVE_BALANCE_VIOLATION` alarm. A test-only trigger holds the `won`'s
/// journal insert on an advisory lock this test owns: the `won` passes its guard
/// and opens its post transaction, the `lost` resolves the dispute and commits,
/// then the lock is released and the `won` post runs on the resolved state.
/// Every wait while the gate is held is bounded (`GATE_WAIT`, and a
/// `lock_timeout` in the trigger), so a locking regression fails the test
/// instead of hanging it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn an_outcome_posted_after_the_other_resolves_is_a_transition_refusal() {
    let (_c, raw, provider) = boot().await;
    let s = setup_seller(&raw, &provider).await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    settle(&provider, &s, "PAY-CC-3", 1000).await;
    chargeback_svc(&provider)
        .record_phase(
            &ctx,
            &scope,
            dispute_request(&s, "CC-3", DisputePhase::Opened),
        )
        .await
        .expect("opened cash-hold");

    // The gate: a `won` journal insert waits on advisory lock (4242, 3). The
    // `lock_timeout` turns a wait that never ends into an error.
    raw.execute_raw(pg(
        "CREATE FUNCTION bss.test_gate_won_insert() RETURNS trigger \
         LANGUAGE plpgsql AS $$ BEGIN SET LOCAL lock_timeout = '60s'; \
         PERFORM pg_advisory_xact_lock(4242, 3); RETURN NEW; END $$",
    ))
    .await
    .expect("create the gate function");
    raw.execute_raw(pg(
        "CREATE TRIGGER test_gate_won_insert BEFORE INSERT ON bss.ledger_journal_entry \
         FOR EACH ROW WHEN (NEW.source_business_id = 'DSP-CC-3:1:WON') \
         EXECUTE FUNCTION bss.test_gate_won_insert()",
    ))
    .await
    .expect("create the gate trigger");
    let gate = raw.begin().await.expect("open the gate transaction");
    gate.execute_raw(pg("SELECT pg_advisory_xact_lock(4242, 3)"))
        .await
        .expect("take the gate lock");

    // The `won` passes its guard (the dispute is OPENED) and waits at its insert.
    // Its service counts invariant alarms.
    let metrics = MetricsHarness::new();
    let won_svc = recording_chargeback_svc(&provider, &metrics);
    let won_scope = scope.clone();
    let won_req = dispute_request(&s, "CC-3", DisputePhase::Won);
    let won = tokio::spawn(async move {
        won_svc
            .record_phase(&SecurityContext::anonymous(), &won_scope, won_req)
            .await
    });
    let mut gated = false;
    for _ in 0..1500 {
        if won.is_finished() {
            break;
        }
        let waiting = raw
            .query_one_raw(pg("SELECT COUNT(*) FROM pg_locks \
                 WHERE locktype = 'advisory' AND classid = 4242 AND objid = 3 AND NOT granted"))
            .await
            .unwrap()
            .map_or(0, |r| r.try_get_by_index::<i64>(0).unwrap());
        if waiting > 0 {
            gated = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gated, "the won post reaches the gated journal insert");

    // The `lost` resolves the dispute and commits while the `won` waits.
    let lost = tokio::time::timeout(
        GATE_WAIT,
        chargeback_svc(&provider).record_phase(
            &ctx,
            &scope,
            dispute_request(&s, "CC-3", DisputePhase::Lost),
        ),
    )
    .await
    .expect("the lost finishes while the won waits at the gate")
    .expect("the lost resolves the opened dispute");
    assert!(
        matches!(&lost, ChargebackOutcome::Recorded(p) if !p.replayed),
        "the lost posts fresh"
    );

    // Release the gate: the `won` post now runs on the resolved dispute.
    gate.commit().await.expect("release the gate lock");
    let won = tokio::time::timeout(GATE_WAIT, won)
        .await
        .expect("the won finishes after the gate opens")
        .expect("the won task must not panic");
    assert!(
        matches!(won, Err(DomainError::InvalidDisputeTransition(_))),
        "the outcome that lost the race is refused as an invalid transition"
    );
    metrics.force_flush();
    assert_eq!(
        metrics.counter_value("ledger_alarm_total", &negative_balance_alarm()),
        0,
        "the race loser raises no negative-balance alarm"
    );

    // Money: only the lost landed.
    assert_eq!(
        dispute_phase(&raw, &s, "DSP-CC-3").await.as_deref(),
        Some("LOST"),
        "the dispute resolved to LOST"
    );
    assert_eq!(
        account_balance(&raw, &s, s.dispute_hold).await,
        Some(text(0)),
        "DISPUTE_HOLD drained once, by the lost"
    );
    assert_eq!(
        account_balance(&raw, &s, s.dispute_loss).await,
        Some(text(1000)),
        "the forfeiture booked once"
    );
    let row = PaymentRepo::new(provider.clone())
        .read_settlement(&scope, s.tenant, "PAY-CC-3")
        .await
        .unwrap()
        .expect("settlement row present");
    assert_eq!(
        row.clawed_back,
        usd(1000),
        "clawed_back reflects the lost alone (10.00)"
    );
}

/// Chargeback #D-4: the in-txn dispute check refuses only a dispute that is no
/// longer `OPENED` at the cycle. On a still-open dispute it passes, so a real
/// refusal keeps its own class. Here `DISPUTE_HOLD` is emptied with a raw write
/// while the dispute stays `OPENED`: the `won` is refused by the projector as
/// `NegativeBalance`, and the `NEGATIVE_BALANCE_VIOLATION` alarm is counted (this
/// also shows that #D-3's zero alarm count can see an alarm).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn an_outcome_on_a_still_open_dispute_keeps_its_negative_balance_refusal() {
    let (_c, raw, provider) = boot().await;
    let s = setup_seller(&raw, &provider).await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    settle(&provider, &s, "PAY-CC-4", 1000).await;
    chargeback_svc(&provider)
        .record_phase(
            &ctx,
            &scope,
            dispute_request(&s, "CC-4", DisputePhase::Opened),
        )
        .await
        .expect("opened cash-hold");
    raw.execute_raw(pg(format!(
        "UPDATE bss.ledger_account_balance SET balance = '{}' \
         WHERE tenant_id='{}' AND account_id='{}'",
        text(0),
        s.tenant,
        s.dispute_hold
    )))
    .await
    .expect("empty DISPUTE_HOLD under the open dispute");

    let metrics = MetricsHarness::new();
    let won = recording_chargeback_svc(&provider, &metrics)
        .record_phase(&ctx, &scope, dispute_request(&s, "CC-4", DisputePhase::Won))
        .await;
    assert!(
        matches!(won, Err(DomainError::NegativeBalance(_))),
        "a won on a still-open dispute keeps the projector's NegativeBalance"
    );
    metrics.force_flush();
    assert_eq!(
        metrics.counter_value("ledger_alarm_total", &negative_balance_alarm()),
        1,
        "the real refusal raises its negative-balance alarm"
    );
    assert_eq!(
        dispute_phase(&raw, &s, "DSP-CC-4").await.as_deref(),
        Some("OPENED"),
        "the refused won leaves the dispute open"
    );
}

/// Chargeback #D-5: a chargeback drain that claimed a row another drain has
/// since applied counts that apply as `replayed`, not `applied`. A deterministic
/// stand-in for two racing drains: a `won` that arrives before its `opened` is
/// queued; the `opened`'s drain applies it (the winner). The test then puts back
/// the state a losing drain sees after its own claim — the queue row `QUEUED`,
/// and the dispute `OPENED` so the out-of-txn guard passes — and drains again.
/// The post finds the dedup `POSTED` and replays it: no second effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn a_chargeback_drain_that_replays_an_applied_outcome_counts_a_replay() {
    let (_c, raw, provider) = boot().await;
    let s = setup_seller(&raw, &provider).await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);
    let svc = chargeback_svc(&provider);

    settle(&provider, &s, "PAY-CC-5", 1000).await;
    let queued = svc
        .record_phase(&ctx, &scope, dispute_request(&s, "CC-5", DisputePhase::Won))
        .await
        .expect("a won before its opened is accepted");
    assert!(
        matches!(queued, ChargebackOutcome::Queued(_)),
        "a won before its opened is queued"
    );
    svc.record_phase(
        &ctx,
        &scope,
        dispute_request(&s, "CC-5", DisputePhase::Opened),
    )
    .await
    .expect("opened cash-hold");

    let queue_status = || async {
        raw.query_one_raw(pg(format!(
            "SELECT status FROM bss.ledger_pending_event_queue \
             WHERE tenant_id='{}' AND flow='CHARGEBACK' AND business_id='DSP-CC-5:1:WON'",
            s.tenant
        )))
        .await
        .unwrap()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
    };
    let dedup_entry = || async {
        raw.query_one_raw(pg(format!(
            "SELECT result_entry_id FROM bss.ledger_idempotency_dedup \
             WHERE tenant_id='{}' AND flow='CHARGEBACK' AND business_id='DSP-CC-5:1:WON' \
             AND status='POSTED'",
            s.tenant
        )))
        .await
        .unwrap()
        .map(|r| r.try_get_by_index::<Option<Uuid>>(0).unwrap())
    };
    let won_entries = || async {
        raw.query_one_raw(pg(format!(
            "SELECT COUNT(*) FROM bss.ledger_journal_entry \
             WHERE tenant_id='{}' AND source_business_id='DSP-CC-5:1:WON'",
            s.tenant
        )))
        .await
        .unwrap()
        .map_or(0, |r| r.try_get_by_index::<i64>(0).unwrap())
    };
    assert_eq!(
        queue_status().await.as_deref(),
        Some("APPLIED"),
        "the opened's drain applied the queued won"
    );
    assert_eq!(
        dispute_phase(&raw, &s, "DSP-CC-5").await.as_deref(),
        Some("WON"),
        "the applied won resolved the dispute"
    );
    let first_entry = dedup_entry()
        .await
        .expect("the applied won's dedup row is POSTED");

    raw.execute_raw(pg(format!(
        "UPDATE bss.ledger_pending_event_queue SET status = 'QUEUED' \
         WHERE tenant_id='{}' AND flow='CHARGEBACK' AND business_id='DSP-CC-5:1:WON'",
        s.tenant
    )))
    .await
    .expect("put the queue row back to QUEUED");
    raw.execute_raw(pg(format!(
        "UPDATE bss.ledger_dispute SET last_phase = 'OPENED' \
         WHERE tenant_id='{}' AND dispute_id='DSP-CC-5'",
        s.tenant
    )))
    .await
    .expect("put the dispute back to OPENED");

    let report = svc
        .drain(&ctx, &scope, s.tenant, 16)
        .await
        .expect("drain ok");
    assert_eq!(
        report.applied, 0,
        "a replayed apply is not counted as applied"
    );
    assert_eq!(
        report.replayed, 1,
        "the apply of an already-applied row is counted as replayed"
    );
    assert_eq!(report.blocked, 0, "the replay reached the post unblocked");

    // No second effect: one `won` entry, the dedup keeps it, and the hold
    // released once (CASH_CLEARING back at 10.00, DISPUTE_HOLD at 0).
    assert_eq!(
        won_entries().await,
        1,
        "the replay posts no second won entry"
    );
    assert_eq!(
        dedup_entry().await,
        Some(first_entry),
        "the dedup keeps the first won entry"
    );
    assert_eq!(
        account_balance(&raw, &s, s.dispute_hold).await,
        Some(text(0)),
        "DISPUTE_HOLD released once"
    );
    assert_eq!(
        account_balance(&raw, &s, s.cash).await,
        Some(text(1000)),
        "CASH_CLEARING got the hold back once"
    );
}

/// Chargeback #D-6 (the #D-1 race, made deterministic): a repeat of the SAME
/// `lost` (same `dispute_id:cycle:phase` key) that starts before the first
/// `lost` commits, and whose dispute read returns after that commit, MUST replay
/// the first entry. It must not be refused as `InvalidDisputeTransition` because
/// its dispute read already shows `LOST` (the #D-1 failure: a dedup lookup made
/// before the commit missed the first `lost`). Two gates fix the order:
/// - a test-only trigger holds the first `lost` at its dedup finalize (after its
///   dispute write, before its commit) on an advisory lock this test owns;
/// - a gate transaction queues an `ACCESS EXCLUSIVE` lock on `ledger_dispute`
///   behind the held post, so the repeat's out-of-txn dispute read waits.
///
/// The repeat starts while both gates are held, and its dispute read waits. The
/// test then releases the first `lost` (it commits; the table gate gets its
/// lock) and then the table gate: the repeat reads the dispute as `LOST`. Every
/// wait while a gate is held is bounded (`GATE_WAIT`, and a `lock_timeout` in
/// both gates).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn a_lost_repeated_after_the_first_commits_replays() {
    let (_c, raw, provider) = boot().await;
    let s = setup_seller(&raw, &provider).await;
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    settle(&provider, &s, "PAY-CC-6", 1000).await;
    chargeback_svc(&provider)
        .record_phase(
            &ctx,
            &scope,
            dispute_request(&s, "CC-6", DisputePhase::Opened),
        )
        .await
        .expect("opened cash-hold");

    // Gate 1: the `lost` dedup finalize (`CLAIMED` -> `POSTED`) waits on advisory
    // lock (4242, 6). Only the first `lost` finalizes; a replay never does.
    raw.execute_raw(pg(
        "CREATE FUNCTION bss.test_gate_lost_finalize() RETURNS trigger \
         LANGUAGE plpgsql AS $$ BEGIN SET LOCAL lock_timeout = '60s'; \
         PERFORM pg_advisory_xact_lock(4242, 6); RETURN NEW; END $$",
    ))
    .await
    .expect("create the finalize gate function");
    raw.execute_raw(pg(
        "CREATE TRIGGER test_gate_lost_finalize BEFORE UPDATE ON bss.ledger_idempotency_dedup \
         FOR EACH ROW WHEN (NEW.business_id = 'DSP-CC-6:1:LOST' AND NEW.status = 'POSTED') \
         EXECUTE FUNCTION bss.test_gate_lost_finalize()",
    ))
    .await
    .expect("create the finalize gate trigger");
    let finalize_gate = raw.begin().await.expect("open the finalize gate");
    finalize_gate
        .execute_raw(pg("SELECT pg_advisory_xact_lock(4242, 6)"))
        .await
        .expect("take the finalize gate lock");

    // The first `lost` writes the dispute and waits at its finalize.
    let first_svc = chargeback_svc(&provider);
    let first_scope = scope.clone();
    let first_req = dispute_request(&s, "CC-6", DisputePhase::Lost);
    let first = tokio::spawn(async move {
        first_svc
            .record_phase(&SecurityContext::anonymous(), &first_scope, first_req)
            .await
    });
    assert!(
        lock_request_waits(
            &raw,
            "locktype = 'advisory' AND classid = 4242 AND objid = 6"
        )
        .await,
        "the first lost reaches the gated finalize"
    );

    // Gate 2: an ACCESS EXCLUSIVE lock on `ledger_dispute` queues behind the
    // held first `lost` (it holds the dispute row it wrote). Every later read of
    // the table queues behind this request.
    let table_gate = raw.begin().await.expect("open the table gate");
    let (release_table, table_released) = tokio::sync::oneshot::channel::<()>();
    let table_gate = tokio::spawn(async move {
        table_gate
            .execute_raw(pg("SET LOCAL lock_timeout = '60s'"))
            .await?;
        table_gate
            .execute_raw(pg("LOCK TABLE bss.ledger_dispute IN ACCESS EXCLUSIVE MODE"))
            .await?;
        let _ = table_released.await;
        table_gate.commit().await
    });
    let dispute_lock = "locktype = 'relation' AND relation = 'bss.ledger_dispute'::regclass";
    assert!(
        lock_request_waits(
            &raw,
            &format!("{dispute_lock} AND mode = 'AccessExclusiveLock'")
        )
        .await,
        "the table gate queues behind the held first lost"
    );

    // The repeat starts while the first `lost` is not committed; its dispute
    // read waits behind the table gate. A dedup lookup made before that read
    // cannot see the first `lost`.
    let repeat_svc = chargeback_svc(&provider);
    let repeat_scope = scope.clone();
    let repeat_req = dispute_request(&s, "CC-6", DisputePhase::Lost);
    let repeat = tokio::spawn(async move {
        repeat_svc
            .record_phase(&SecurityContext::anonymous(), &repeat_scope, repeat_req)
            .await
    });
    assert!(
        lock_request_waits(
            &raw,
            &format!("{dispute_lock} AND mode = 'AccessShareLock'")
        )
        .await,
        "the repeat's dispute read waits behind the table gate"
    );

    // Release gate 1: the first `lost` commits. The table gate then gets its lock.
    finalize_gate
        .commit()
        .await
        .expect("release the finalize gate");
    let first = tokio::time::timeout(GATE_WAIT, first)
        .await
        .expect("the first lost finishes after the finalize gate opens")
        .expect("the first lost task must not panic");
    let first = match first {
        Ok(ChargebackOutcome::Recorded(first)) => first,
        other => panic!("the first lost posts inline, got: {other:?}"),
    };
    assert!(!first.replayed, "the first lost posts fresh");

    // Release gate 2: the repeat reads the dispute as `LOST`.
    release_table.send(()).expect("the table gate is waiting");
    tokio::time::timeout(GATE_WAIT, table_gate)
        .await
        .expect("the table gate finishes")
        .expect("the table gate task must not panic")
        .expect("the table gate takes and releases its lock");
    let repeat = tokio::time::timeout(GATE_WAIT, repeat)
        .await
        .expect("the repeat finishes after the table gate opens")
        .expect("the repeat task must not panic");
    let replay = match repeat {
        Ok(ChargebackOutcome::Recorded(replay)) => replay,
        other => panic!("the repeated lost must replay the first entry, got: {other:?}"),
    };
    assert!(replay.replayed, "the repeated lost is a replay");
    assert_eq!(
        replay.entry_id, first.entry_id,
        "the replay returns the first lost's entry"
    );

    // Money: the forfeiture booked once.
    let lost_entries = raw
        .query_one_raw(pg(format!(
            "SELECT COUNT(*) FROM bss.ledger_journal_entry \
             WHERE tenant_id='{}' AND source_business_id='DSP-CC-6:1:LOST'",
            s.tenant
        )))
        .await
        .unwrap()
        .map_or(0, |r| r.try_get_by_index::<i64>(0).unwrap());
    assert_eq!(lost_entries, 1, "one lost entry");
    assert_eq!(
        dispute_phase(&raw, &s, "DSP-CC-6").await.as_deref(),
        Some("LOST"),
        "the dispute resolved to LOST"
    );
    assert_eq!(
        account_balance(&raw, &s, s.dispute_hold).await,
        Some(text(0)),
        "DISPUTE_HOLD emptied once"
    );
    assert_eq!(
        account_balance(&raw, &s, s.dispute_loss).await,
        Some(text(1000)),
        "the forfeiture booked once"
    );
    let row = PaymentRepo::new(provider.clone())
        .read_settlement(&scope, s.tenant, "PAY-CC-6")
        .await
        .unwrap()
        .expect("settlement row present");
    assert_eq!(
        row.clawed_back,
        usd(1000),
        "clawed_back reflects one clawback (10.00)"
    );
}
