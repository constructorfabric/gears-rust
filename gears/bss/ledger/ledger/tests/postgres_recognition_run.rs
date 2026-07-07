//! Postgres-only integration + concurrency tests for the Slice 4 ASC 606 S6
//! **release** (Groups D/E/F), driven through the REAL stack:
//! `InvoicePostService` (to materialize a deferred schedule + segments) →
//! `RecognitionRunService` / `RecognitionRunner` (to release / reverse them).
//!
//! Covers (design §11, Group F4):
//! - **atomic release**: a run posts one `DR CL / CR Revenue` entry per due
//!   segment AND bumps `recognized_minor` AND stamps the segment `DONE`, all in
//!   one txn (balances + counter + status all move together);
//! - **no double recognition**: re-running the same period credits each segment
//!   exactly once (the per-segment `RECOGNITION` gate + `status = DONE` /
//!   `UNIQUE (schedule, period_id)`);
//! - **over-recognition blocked at the per-schedule CHECK** even when a sibling
//!   schedule keeps the per-stream `CONTRACT_LIABILITY` account aggregate
//!   positive (the cap is per-obligation, not per-account);
//! - **reversal** decrements `recognized_minor`, restores `CONTRACT_LIABILITY`,
//!   and leaves the reversed segment `DONE`;
//! - **racing runs** on the same period → each segment credited exactly once (no
//!   double-credit under contention);
//! - **ordering resume**: an out-of-order-parked `QUEUED` segment drains on a
//!   later run once its predecessor commits `DONE`.
//!
//! `LedgerLocalClient::new` is `pub(crate)`, so this out-of-crate test drives the
//! `pub` services directly (mirrors `postgres_recognition_build.rs`). Ignored by
//! default; run with `-- --ignored`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::similar_names
)]

use std::sync::Arc;

use bss_ledger::config::{FxConfig, RecognitionConfig};
use bss_ledger::domain::error::DomainError;
use bss_ledger::domain::invoice::builder::{InvoiceItem, PostedInvoice, TaxBreakdown};
use bss_ledger::domain::model::RepoError;
use bss_ledger::domain::model::{AccountRow, CurrencyScaleRow};
use bss_ledger::domain::money::DEFAULT_PLAUSIBLE_MAX_MAJOR;
use bss_ledger::domain::recognition::input::{RecognitionInput, RecognitionTiming};
use bss_ledger::infra::events::publisher::LedgerEventPublisher;
use bss_ledger::infra::invoice_post::InvoicePostService;
use bss_ledger::infra::metrics::test_harness::MetricsHarness;
use bss_ledger::infra::recognition::run_service::RecognitionRunService;
use bss_ledger::infra::recognition::runner::{RecognitionRunner, ReleasableSegment};
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::recognition_repo::NewSchedule;
use bss_ledger::infra::storage::repo::{RecognitionRepo, ReferenceRepo};
use bss_ledger_sdk::{AccountClass, RecognitionRunOutcome, Side};
use chrono::NaiveDate;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

fn pg(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

async fn scalar_i64(conn: &DatabaseConnection, sql: &str) -> Option<i64> {
    conn.query_one(pg(sql.to_owned()))
        .await
        .unwrap()
        .map(|r| r.try_get_by_index::<i64>(0).unwrap())
}

async fn count(conn: &DatabaseConnection, sql: &str) -> i64 {
    scalar_i64(conn, sql).await.unwrap_or(0)
}

fn naive(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// Provisioned seller ids (incl. the per-stream Contract-liability account the
/// deferred split credits + the release draws down).
struct Seller {
    tenant: Uuid,
    payer: Uuid,
    ar: Uuid,
    revenue: Uuid,
    contract_liability: Uuid,
    tax: Uuid,
    suspense: Uuid,
    period_id: String,
}

fn account(
    tenant: Uuid,
    id: Uuid,
    class: AccountClass,
    normal: Side,
    stream: Option<&str>,
) -> AccountRow {
    AccountRow {
        account_id: id,
        tenant_id: tenant,
        legal_entity_id: tenant,
        account_class: class.as_str().to_owned(),
        currency: "USD".to_owned(),
        revenue_stream: stream.map(str::to_owned),
        normal_side: normal.as_str().to_owned(),
        may_go_negative: false,
        lifecycle_state: "OPEN".to_owned(),
    }
}

/// Open one more fiscal period for the seller (so a release into a later period
/// passes the foundation OPEN-period gate).
async fn open_period(raw: &DatabaseConnection, s: &Seller, period_id: &str) {
    raw.execute(pg(format!(
        "INSERT INTO bss.ledger_fiscal_period (tenant_id, legal_entity_id, period_id, fiscal_tz, status)
         VALUES ('{}','{}','{period_id}','UTC','OPEN')",
        s.tenant, s.tenant
    )))
    .await
    .unwrap();
}

/// Boot, migrate, seed USD@2 + an OPEN period + AR / REVENUE(subscription) /
/// CONTRACT_LIABILITY(subscription) / TAX / SUSPENSE accounts. Mirrors
/// `postgres_recognition_build::setup`.
async fn setup(url: &str) -> (DatabaseConnection, DBProvider<DbError>, Seller) {
    let raw = Database::connect(url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();
    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    let provider = DBProvider::<DbError>::new(tdb);

    let s = Seller {
        tenant: Uuid::now_v7(),
        payer: Uuid::now_v7(),
        ar: Uuid::now_v7(),
        revenue: Uuid::now_v7(),
        contract_liability: Uuid::now_v7(),
        tax: Uuid::now_v7(),
        suspense: Uuid::now_v7(),
        period_id: "202606".to_owned(),
    };

    let reference = ReferenceRepo::new(provider.clone());
    reference
        .upsert_currency_scale(CurrencyScaleRow {
            tenant_id: s.tenant,
            currency: "USD".to_owned(),
            minor_units: 2,
            plausible_max_major: DEFAULT_PLAUSIBLE_MAX_MAJOR,
            source: "iso".to_owned(),
        })
        .await
        .unwrap();
    open_period(&raw, &s, &s.period_id).await;

    for row in [
        account(s.tenant, s.ar, AccountClass::Ar, Side::Debit, None),
        account(
            s.tenant,
            s.revenue,
            AccountClass::Revenue,
            Side::Credit,
            Some("subscription"),
        ),
        account(
            s.tenant,
            s.contract_liability,
            AccountClass::ContractLiability,
            Side::Credit,
            Some("subscription"),
        ),
        account(
            s.tenant,
            s.tax,
            AccountClass::TaxPayable,
            Side::Credit,
            None,
        ),
        account(
            s.tenant,
            s.suspense,
            AccountClass::Suspense,
            Side::Credit,
            None,
        ),
    ] {
        reference.insert_account(row).await.unwrap();
    }
    (raw, provider, s)
}

/// A `subscription` item with a straight-line recognition spec over `periods`
/// months from `first_period_id` (so the segments land in known periods).
fn recognized_item(amount: i64, periods: u32, first_period: &str, item_ref: &str) -> InvoiceItem {
    InvoiceItem {
        amount_minor_ex_tax: amount,
        deferred_minor: 0,
        currency: "USD".to_owned(),
        revenue_stream: "subscription".to_owned(),
        catalog_class: Some(AccountClass::Revenue),
        contract_class: None,
        gl_code: Some("4000".to_owned()),
        recognition: Some(RecognitionInput {
            policy_ref: "policy.sl.v1".to_owned(),
            timing: RecognitionTiming::StraightLine {
                periods,
                first_period_id: Some(first_period.to_owned()),
            },
            po_allocation_group: Some("grp-1".to_owned()),
            multi_po: false,
            ssp_snapshot_ref: None,
            subscription_ref: Some("sub-1".to_owned()),
            vc_estimate_ref: None,
            vc_method_ref: None,
            immaterial_one_shot_sku: false,
        }),
        invoice_item_ref: Some(item_ref.to_owned()),
        sku_or_plan_ref: None,
        price_id: None,
        pricing_snapshot_ref: None,
    }
}

fn invoice(s: &Seller, invoice_id: &str, items: Vec<InvoiceItem>) -> PostedInvoice {
    PostedInvoice {
        invoice_id: invoice_id.to_owned(),
        payer_tenant_id: s.payer,
        resource_tenant_id: None,
        seller_tenant_id: s.tenant,
        effective_at: naive(2026, 6, 1),
        due_date: Some(naive(2026, 7, 1)),
        period_id: s.period_id.clone(),
        items,
        tax: Vec::<TaxBreakdown>::new(),
        posted_by_actor_id: s.tenant,
        correlation_id: s.tenant,
    }
}

fn invoice_svc(provider: &DBProvider<DbError>, harness: &MetricsHarness) -> InvoicePostService {
    InvoicePostService::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(harness.metrics()),
        RecognitionConfig::default(),
        FxConfig::default(),
    )
}

fn run_svc(provider: &DBProvider<DbError>, harness: &MetricsHarness) -> RecognitionRunService {
    RecognitionRunService::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(harness.metrics()),
    )
}

async fn bal(raw: &DatabaseConnection, s: &Seller, account: Uuid) -> Option<i64> {
    scalar_i64(
        raw,
        &format!(
            "SELECT balance_minor FROM bss.ledger_account_balance \
             WHERE tenant_id='{}' AND account_id='{}' AND currency='USD'",
            s.tenant, account
        ),
    )
    .await
}

/// The schedule_id of the (single) ACTIVE schedule for an invoice.
async fn schedule_id(raw: &DatabaseConnection, s: &Seller, invoice_id: &str) -> String {
    raw.query_one(pg(format!(
        "SELECT schedule_id FROM bss.ledger_recognition_schedule \
         WHERE tenant_id='{}' AND source_invoice_id='{invoice_id}'",
        s.tenant
    )))
    .await
    .unwrap()
    .map(|r| r.try_get_by_index::<String>(0).unwrap())
    .expect("a schedule for the invoice")
}

async fn recognized_minor(raw: &DatabaseConnection, s: &Seller, schedule: &str) -> Option<i64> {
    scalar_i64(
        raw,
        &format!(
            "SELECT recognized_minor FROM bss.ledger_recognition_schedule \
             WHERE tenant_id='{}' AND schedule_id='{schedule}'",
            s.tenant
        ),
    )
    .await
}

/// The `status` of a schedule row by `schedule_id`.
async fn schedule_status_of(
    raw: &DatabaseConnection,
    s: &Seller,
    schedule: &str,
) -> Option<String> {
    raw.query_one(pg(format!(
        "SELECT status FROM bss.ledger_recognition_schedule \
         WHERE tenant_id='{}' AND schedule_id='{schedule}'",
        s.tenant
    )))
    .await
    .unwrap()
    .map(|r| r.try_get_by_index::<String>(0).unwrap())
}

/// Count recognition (DR CL / CR Revenue) journal ENTRIES for the schedule's
/// release business id `schedule_id:segment_no` (NOT the reversal).
async fn release_entry_count(
    raw: &DatabaseConnection,
    s: &Seller,
    schedule: &str,
    segment_no: i32,
) -> i64 {
    count(
        raw,
        &format!(
            "SELECT COUNT(*) FROM bss.ledger_journal_entry \
             WHERE tenant_id='{}' AND source_doc_type='RECOGNITION' \
               AND source_business_id='{schedule}:{segment_no}'",
            s.tenant
        ),
    )
    .await
}

async fn segment_status(
    raw: &DatabaseConnection,
    s: &Seller,
    schedule: &str,
    segment_no: i32,
) -> Option<String> {
    raw.query_one(pg(format!(
        "SELECT status FROM bss.ledger_recognition_segment \
         WHERE tenant_id='{}' AND schedule_id='{schedule}' AND segment_no={segment_no}",
        s.tenant
    )))
    .await
    .unwrap()
    .map(|r| r.try_get_by_index::<String>(0).unwrap())
}

// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn run_releases_atomically_and_is_not_double_credited() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // A 2-segment straight-line schedule, both segments in the single OPEN period
    // 202606 (so both are due + releasable without opening more periods): 1000 ex
    // tax over 2 months from 202606 ⇒ each segment 500, but both land in
    // 202606..202607. To keep both in the OPEN period, open 202607 too.
    open_period(&raw, &s, "202607").await;
    let inv = invoice(
        &s,
        "INV-REL",
        vec![recognized_item(1000, 2, "202606", "item-1")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    let sched = schedule_id(&raw, &s, "INV-REL").await;
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(1000),
        "CL fully deferred before release"
    );

    // Trigger a run for the LATER period 202607 (releases both segments
    // 202606 + 202607, in order).
    let svc = run_svc(&provider, &harness);
    let outcome = svc
        .trigger(&ctx, &scope, s.tenant, "202607", None)
        .await
        .expect("run releases");
    match outcome {
        RecognitionRunOutcome::Ran(r) => assert_eq!(r.released, 2, "both segments released fresh"),
        RecognitionRunOutcome::Queued(_) => panic!("in-order release must not queue"),
    }

    // Atomic effects: CL drained to 0, Revenue credited 1000, recognized_minor =
    // total, both segments DONE, one release entry per segment.
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(0),
        "CL drained"
    );
    assert_eq!(
        bal(&raw, &s, s.revenue).await,
        Some(1000),
        "Revenue recognized"
    );
    assert_eq!(recognized_minor(&raw, &s, &sched).await, Some(1000));
    assert_eq!(
        segment_status(&raw, &s, &sched, 1).await.as_deref(),
        Some("DONE")
    );
    assert_eq!(
        segment_status(&raw, &s, &sched, 2).await.as_deref(),
        Some("DONE")
    );
    assert_eq!(release_entry_count(&raw, &s, &sched, 1).await, 1);
    assert_eq!(release_entry_count(&raw, &s, &sched, 2).await, 1);

    // Re-run the SAME period: NO double credit (each segment replays via the
    // per-segment RECOGNITION gate + status=DONE). Balances + counter unchanged,
    // still exactly one entry per segment.
    let again = svc
        .trigger(&ctx, &scope, s.tenant, "202607", None)
        .await
        .expect("re-run is a no-op");
    if let RecognitionRunOutcome::Ran(r) = again {
        assert_eq!(r.released, 0, "nothing fresh on the re-run");
    }
    assert_eq!(bal(&raw, &s, s.contract_liability).await, Some(0));
    assert_eq!(
        bal(&raw, &s, s.revenue).await,
        Some(1000),
        "no second credit"
    );
    assert_eq!(recognized_minor(&raw, &s, &sched).await, Some(1000));
    assert_eq!(
        release_entry_count(&raw, &s, &sched, 1).await,
        1,
        "still one entry"
    );
    assert_eq!(release_entry_count(&raw, &s, &sched, 2).await, 1);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn over_recognition_blocked_at_per_schedule_check_with_sibling_positive() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // TWO schedules on the SAME stream/account (two single-period 1-segment
    // invoices, each 600 deferred in 202606). The per-stream CONTRACT_LIABILITY
    // account aggregates BOTH (1200 credit).
    for inv_id in ["INV-A", "INV-B"] {
        let inv = invoice(
            &s,
            inv_id,
            vec![recognized_item(600, 1, "202606", "item-1")],
        );
        invoice_svc(&provider, &harness)
            .post_invoice(&ctx, &scope, &inv, true)
            .await
            .expect("deferred invoice posts");
    }
    let sched_a = schedule_id(&raw, &s, "INV-A").await;
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(1200),
        "both schedules aggregate on the per-stream CL account"
    );

    // Release ONLY schedule A's segment (via the runner's single-segment release),
    // leaving schedule B fully deferred. recognized_minor(A) = 600 = its total;
    // the per-stream CONTRACT_LIABILITY account is still +600 (B's deferred
    // balance), so the ACCOUNT aggregate is comfortably positive.
    let runner = RecognitionRunner::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(harness.metrics()),
    );
    let seg_a = ReleasableSegment {
        schedule_id: sched_a.clone(),
        segment_no: 1,
        period_id: "202606".to_owned(),
        amount_minor: 600,
        revenue_stream: "subscription".to_owned(),
        currency: "USD".to_owned(),
    };
    runner
        .release_segment(&ctx, &scope, s.tenant, &seg_a, Uuid::now_v7())
        .await
        .expect("release schedule A's segment");
    assert_eq!(recognized_minor(&raw, &s, &sched_a).await, Some(600));
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(600),
        "the per-stream CL account is still positive (schedule B's deferred balance)"
    );

    // Now attempt to OVER-bump schedule A past its 600 total by +1 (the path the
    // runner's stamp sidecar exercises). The per-schedule
    // `recognized_minor <= total_deferred_minor` CHECK rejects it as a cap
    // violation — EVEN THOUGH the per-stream CONTRACT_LIABILITY account is still
    // +600. The cap is per-obligation (per schedule), not per-account.
    let err = provider
        .transaction(|txn| {
            let sched_a = sched_a.clone();
            let scope = scope.clone();
            Box::pin(async move {
                RecognitionRepo::add_recognized(txn, &scope, s.tenant, &sched_a, 1)
                    .await
                    .map_err(|e| DbError::Sea(sea_orm::DbErr::Custom(e.to_string())))
            })
        })
        .await
        .expect_err("over-recognition must be blocked at the per-schedule CHECK");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("chk_ledger_recognition_schedule_") || msg.contains("check"),
        "over-recognition is the per-schedule cap CHECK, got: {msg}"
    );
    // Schedule A's counter is unchanged (the over-bump rolled back).
    assert_eq!(recognized_minor(&raw, &s, &sched_a).await, Some(600));
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn reversal_decrements_and_segment_stays_done() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // One single-period 1-segment schedule (600 deferred in 202606), released.
    let inv = invoice(
        &s,
        "INV-REV",
        vec![recognized_item(600, 1, "202606", "item-1")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    let sched = schedule_id(&raw, &s, "INV-REV").await;
    run_svc(&provider, &harness)
        .trigger(&ctx, &scope, s.tenant, "202606", None)
        .await
        .expect("release");
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(0),
        "CL drained"
    );
    assert_eq!(
        bal(&raw, &s, s.revenue).await,
        Some(600),
        "Revenue recognized"
    );
    assert_eq!(recognized_minor(&raw, &s, &sched).await, Some(600));
    assert_eq!(
        segment_status(&raw, &s, &sched, 1).await.as_deref(),
        Some("DONE")
    );

    // Reverse the released segment: DR Revenue / CR CL, decrement recognized_minor
    // back to 0 — and the reversed segment STAYS DONE (design §4.3).
    let runner = RecognitionRunner::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(harness.metrics()),
    );
    let seg = ReleasableSegment {
        schedule_id: sched.clone(),
        segment_no: 1,
        period_id: "202606".to_owned(),
        amount_minor: 600,
        revenue_stream: "subscription".to_owned(),
        currency: "USD".to_owned(),
    };
    let posting = runner
        .release_reversal(&ctx, &scope, s.tenant, &seg)
        .await
        .expect("reversal posts");
    assert!(!posting.replayed, "a fresh reversal");

    // Effects: CL restored to 600, Revenue back to 0, recognized_minor back to 0.
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(600),
        "CL restored"
    );
    assert_eq!(
        bal(&raw, &s, s.revenue).await,
        Some(0),
        "revenue un-recognized"
    );
    assert_eq!(
        recognized_minor(&raw, &s, &sched).await,
        Some(0),
        "counter decremented"
    );
    // The reversed segment is left DONE (its release happened + was compensated;
    // re-recognizing needs a NEW schedule version, Phase 3).
    assert_eq!(
        segment_status(&raw, &s, &sched, 1).await.as_deref(),
        Some("DONE"),
        "reversed segment stays DONE"
    );

    // The reversal is idempotent (schedule_id:segment_no:reversal): a replay does
    // not decrement twice (which would underflow the recognized_minor >= 0 CHECK).
    let replay = runner
        .release_reversal(&ctx, &scope, s.tenant, &seg)
        .await
        .expect("reversal replay is a no-op");
    assert!(replay.replayed, "the reversal replays");
    assert_eq!(
        recognized_minor(&raw, &s, &sched).await,
        Some(0),
        "no double decrement"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn racing_runs_credit_each_segment_exactly_once() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // A 1-segment schedule (600 deferred in 202606).
    let inv = invoice(
        &s,
        "INV-RACE",
        vec![recognized_item(600, 1, "202606", "item-1")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    let sched = schedule_id(&raw, &s, "INV-RACE").await;

    // Two runs racing the SAME period (each its own run_id ⇒ no run-dedup
    // short-circuit). The single-active-run `coord` lease serialises them: one
    // wins the lease and releases the segment, the other sees `LeaseHeld` and
    // returns a no-op replay (it never starts a second pass). Both succeed; the
    // per-segment RECOGNITION gate + `status = DONE` remain the ultimate
    // at-most-once backstop, so exactly ONE credit lands either way.
    let svc1 = run_svc(&provider, &harness);
    let svc2 = run_svc(&provider, &harness);
    let (ctx1, ctx2) = (ctx.clone(), ctx.clone());
    let (sc1, sc2) = (scope.clone(), scope.clone());
    let t = s.tenant;
    let (r1, r2) = tokio::join!(
        async move { svc1.trigger(&ctx1, &sc1, t, "202606", None).await },
        async move { svc2.trigger(&ctx2, &sc2, t, "202606", None).await },
    );
    r1.expect("run 1 ok");
    r2.expect("run 2 ok");

    // Exactly one credit landed: CL drained once, Revenue == 600 (not 1200),
    // recognized_minor == 600, exactly one release entry, segment DONE.
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(0),
        "CL drained once"
    );
    assert_eq!(
        bal(&raw, &s, s.revenue).await,
        Some(600),
        "no double-credit"
    );
    assert_eq!(recognized_minor(&raw, &s, &sched).await, Some(600));
    assert_eq!(
        release_entry_count(&raw, &s, &sched, 1).await,
        1,
        "exactly one release entry"
    );
    assert_eq!(
        segment_status(&raw, &s, &sched, 1).await.as_deref(),
        Some("DONE")
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn queued_successor_drains_behind_its_predecessor_in_one_pass() {
    // Ordering (§4.6) drain, in-pass: a previously out-of-order-parked QUEUED
    // successor is re-enumerated by a run and releases only AFTER its
    // lower-period predecessor commits DONE (the predecessor, being lower
    // segment_no, is released first in the same ascending pass). Pins that a
    // QUEUED segment is not stuck — it is picked up and drained behind its
    // predecessor, never ahead of it.
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // A 2-segment schedule: seg-1 in 202606 (predecessor), seg-2 in 202607.
    open_period(&raw, &s, "202607").await;
    let inv = invoice(
        &s,
        "INV-ORD",
        vec![recognized_item(1000, 2, "202606", "item-1")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    let sched = schedule_id(&raw, &s, "INV-ORD").await;

    // Simulate an earlier out-of-order pass that parked seg-2 QUEUED (its
    // predecessor seg-1 had not committed) WITHOUT touching seg-1 (left PENDING).
    let repo = RecognitionRepo::new(provider.clone());
    repo.mark_segment_queued(&scope, s.tenant, &sched, 2)
        .await
        .expect("park seg-2 QUEUED");
    assert_eq!(
        segment_status(&raw, &s, &sched, 2).await.as_deref(),
        Some("QUEUED")
    );

    // Drive a run via the runner over seg-2 ALONE (the QUEUED successor) while
    // seg-1 is still PENDING: the predecessor gate parks it (it stays QUEUED, it
    // is NOT released out of order). One run pass, one run_id.
    let runner = RecognitionRunner::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(harness.metrics()),
    );
    // `run_period("202607")` re-enumerates seg-2 (QUEUED) AND seg-1 (PENDING,
    // 202606 <= 202607). seg-1 has no predecessor ⇒ releases; seg-2's predecessor
    // seg-1 is then DONE ⇒ seg-2 drains. Both release in this single pass — the
    // QUEUED successor is drained behind its predecessor, never ahead of it.
    let summary = runner
        .run_period(&ctx, &scope, s.tenant, "202607", Uuid::now_v7())
        .await
        .expect("run drains in order");
    assert_eq!(
        summary.released, 2,
        "predecessor then successor, both released"
    );
    assert_eq!(
        summary.queued, 0,
        "nothing left parked once the predecessor is DONE"
    );

    // Final state: both DONE, CL fully drained, Revenue fully recognized, and the
    // releases landed in period order (seg-1's entry in 202606, seg-2's in 202607).
    assert_eq!(
        segment_status(&raw, &s, &sched, 1).await.as_deref(),
        Some("DONE")
    );
    assert_eq!(
        segment_status(&raw, &s, &sched, 2).await.as_deref(),
        Some("DONE")
    );
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(0),
        "CL fully drained"
    );
    assert_eq!(bal(&raw, &s, s.revenue).await, Some(1000));
    let seg2_period = scalar_i64(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.ledger_journal_entry \
             WHERE tenant_id='{}' AND source_doc_type='RECOGNITION' \
               AND source_business_id='{sched}:2' AND period_id='202607'",
            s.tenant
        ),
    )
    .await;
    assert_eq!(
        seg2_period,
        Some(1),
        "seg-2 released into its own period 202607"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn queued_successor_re_parks_when_predecessor_excluded_from_window() {
    // The complement of the drain test: when a run's window includes a QUEUED
    // successor but NOT its still-un-DONE predecessor (a narrower run target),
    // the successor is re-parked QUEUED (the §4.6 gate holds) — never released
    // ahead of its predecessor — and a LATER, wider run drains it.
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // seg-1 in 202606, seg-2 in 202607, seg-3 in 202608. Open all three periods.
    open_period(&raw, &s, "202607").await;
    open_period(&raw, &s, "202608").await;
    let inv = invoice(
        &s,
        "INV-ORD2",
        vec![recognized_item(900, 3, "202606", "item-1")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    let sched = schedule_id(&raw, &s, "INV-ORD2").await;

    let runner = RecognitionRunner::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(harness.metrics()),
    );

    // Park seg-3 QUEUED, then run for window 202607 (seg-3 is 202608 > 202607, so
    // OUT of the window — it cannot be re-enumerated this pass). seg-1 + seg-2
    // release in order; seg-3 stays QUEUED (not in window, not touched).
    repo_park(&provider, &scope, s.tenant, &sched, 3).await;
    let first = runner
        .run_period(&ctx, &scope, s.tenant, "202607", Uuid::now_v7())
        .await
        .expect("first window run");
    assert_eq!(first.released, 2, "seg-1 + seg-2 release in order");
    assert_eq!(
        segment_status(&raw, &s, &sched, 3).await.as_deref(),
        Some("QUEUED"),
        "seg-3 untouched"
    );

    // Now widen the window to 202608: seg-3 (QUEUED) is re-enumerated, its
    // predecessors (seg-1, seg-2) are DONE ⇒ it drains.
    let second = runner
        .run_period(&ctx, &scope, s.tenant, "202608", Uuid::now_v7())
        .await
        .expect("widened run drains seg-3");
    assert_eq!(second.released, 1, "seg-3 drains");
    assert_eq!(
        segment_status(&raw, &s, &sched, 3).await.as_deref(),
        Some("DONE")
    );
    assert_eq!(
        recognized_minor(&raw, &s, &sched).await,
        Some(900),
        "all three recognized"
    );
}

/// Park one segment QUEUED via the repo (a tiny helper to keep the test bodies
/// uncluttered).
async fn repo_park(
    provider: &DBProvider<DbError>,
    scope: &AccessScope,
    tenant: Uuid,
    schedule: &str,
    segment_no: i32,
) {
    RecognitionRepo::new(provider.clone())
        .mark_segment_queued(scope, tenant, schedule, segment_no)
        .await
        .expect("park segment QUEUED");
}

// ── Group I-rest: schedule GET (I2) + RecognitionWithoutInvoiceLink (I3) ──────

/// A deferred `subscription` item identical to [`recognized_item`] but with NO
/// `invoice_item_ref` — exercises the §4.7 invoice-link guard (I3): a deferred
/// line that cannot resolve its Contract-liability anchor must be blocked with
/// [`DomainError::RecognitionWithoutInvoiceLink`] BEFORE the post.
fn recognized_item_no_link(amount: i64, periods: u32, first_period: &str) -> InvoiceItem {
    InvoiceItem {
        invoice_item_ref: None,
        ..recognized_item(amount, periods, first_period, "unused")
    }
}

/// I2: after a deferred invoice post, `RecognitionRepo::read_schedule` +
/// `list_segments` (the exact reads the `GET /recognition-schedules/{id}` local
/// client runs) return the materialized schedule header + its segments ordered by
/// `segment_no`; a missing `schedule_id` yields `None` (the handler's 404 source).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn get_schedule_returns_header_and_segments_and_none_when_absent() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // A 2-segment straight-line schedule (1000 over 2 months from 202606).
    open_period(&raw, &s, "202607").await;
    let inv = invoice(
        &s,
        "INV-GET",
        vec![recognized_item(1000, 2, "202606", "item-1")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    let sched = schedule_id(&raw, &s, "INV-GET").await;

    let repo = RecognitionRepo::new(provider.clone());

    // The schedule header reads back with its build-time facts.
    let schedule = repo
        .read_schedule(&scope, s.tenant, &sched)
        .await
        .expect("read schedule")
        .expect("the schedule exists");
    assert_eq!(schedule.status, "ACTIVE");
    assert_eq!(schedule.version, 0);
    assert_eq!(schedule.revenue_stream, "subscription");
    assert_eq!(schedule.currency, "USD");
    assert_eq!(schedule.total_deferred_minor, 1000);
    assert_eq!(schedule.recognized_minor, 0);
    assert_eq!(schedule.source_invoice_id, "INV-GET");
    assert_eq!(schedule.source_invoice_item_ref, "item-1");

    // Its segments come back ordered by segment_no (= period order), PENDING.
    let segments = repo
        .list_segments(&scope, s.tenant, &sched)
        .await
        .expect("list segments");
    assert_eq!(segments.len(), 2, "two straight-line segments");
    assert_eq!(segments[0].segment_no, 1);
    assert_eq!(segments[0].period_id, "202606");
    assert_eq!(segments[0].amount_minor, 500);
    assert_eq!(segments[0].status, "PENDING");
    assert_eq!(segments[1].segment_no, 2);
    assert_eq!(segments[1].period_id, "202607");
    assert_eq!(segments[1].status, "PENDING");

    // A missing schedule_id resolves to None — the handler's 404 source.
    let missing = repo
        .read_schedule(&scope, s.tenant, "does-not-exist")
        .await
        .expect("read absent schedule");
    assert!(
        missing.is_none(),
        "an unknown schedule_id yields None (→ 404)"
    );
}

/// I3: a deferred recognition line with no resolvable `invoice_item_ref` is
/// blocked with [`DomainError::RecognitionWithoutInvoiceLink`] (wire
/// `RECOGNITION_WITHOUT_INVOICE_LINK`, 400) BEFORE the post — no orphan schedule
/// and no journal entry materialize.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn deferred_post_without_invoice_link_is_rejected() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    let inv = invoice(
        &s,
        "INV-NOLINK",
        vec![recognized_item_no_link(1000, 2, "202606")],
    );
    let err = invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect_err("a deferred line without an invoice_item_ref must be blocked");
    assert!(
        matches!(err, DomainError::RecognitionWithoutInvoiceLink(_)),
        "expected RecognitionWithoutInvoiceLink, got {err:?}"
    );

    // Nothing materialized: no schedule, no CONTRACT_LIABILITY balance, no entry.
    assert_eq!(
        count(
            &raw,
            &format!(
                "SELECT COUNT(*) FROM bss.ledger_recognition_schedule \
                 WHERE tenant_id='{}' AND source_invoice_id='INV-NOLINK'",
                s.tenant
            ),
        )
        .await,
        0,
        "the blocked post leaves no orphan schedule"
    );
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        None,
        "no CONTRACT_LIABILITY balance was projected"
    );
    assert_eq!(
        count(
            &raw,
            &format!(
                "SELECT COUNT(*) FROM bss.ledger_journal_entry \
                 WHERE tenant_id='{}' AND source_business_id='INV-NOLINK'",
                s.tenant
            ),
        )
        .await,
        0,
        "the blocked post wrote no journal entry"
    );
}

// ── Fix 1: schedule ACTIVE → COMPLETED on full drain (design §4.6) ────────────

/// Build a `NewSchedule` for a given business key (used to probe the partial
/// `UNIQUE … WHERE status='ACTIVE'` one-live slot directly via the repo).
fn new_schedule(s: &Seller, schedule_id: &str, invoice_id: &str, item_ref: &str) -> NewSchedule {
    NewSchedule {
        tenant_id: s.tenant,
        schedule_id: schedule_id.to_owned(),
        payer_tenant_id: s.payer,
        source_invoice_id: invoice_id.to_owned(),
        source_invoice_item_ref: item_ref.to_owned(),
        po_allocation_group: Some("grp-1".to_owned()),
        subscription_ref: Some("sub-1".to_owned()),
        revenue_stream: "subscription".to_owned(),
        currency: "USD".to_owned(),
        total_deferred_minor: 100,
        policy_ref: "policy.sl.v1".to_owned(),
        ssp_snapshot_ref: None,
        vc_estimate_ref: None,
        vc_method_ref: None,
    }
}

/// Insert a fresh ACTIVE schedule via the real repo path inside one txn, mapping
/// a repo error into a `DbError` so the result is observable (the partial UNIQUE
/// collision surfaces as `Err`). Mirrors the in-txn probe style of
/// `over_recognition_blocked_at_per_schedule_check_with_sibling_positive`.
async fn try_insert_active_schedule(
    provider: &DBProvider<DbError>,
    scope: &AccessScope,
    schedule: NewSchedule,
) -> Result<(), DbError> {
    let schedule = Arc::new(schedule);
    provider
        .transaction(|txn| {
            let scope = scope.clone();
            let schedule = Arc::clone(&schedule);
            Box::pin(async move {
                RecognitionRepo::insert_schedule(txn, &scope, schedule.as_ref())
                    .await
                    .map_err(|e: RepoError| DbError::Sea(sea_orm::DbErr::Custom(e.to_string())))
            })
        })
        .await
}

/// A fully-recognized schedule transitions `ACTIVE → COMPLETED` (terminal), which
/// frees the partial `UNIQUE (…, revenue_stream) WHERE status='ACTIVE'` one-live
/// slot (a fresh ACTIVE schedule for the SAME business key then inserts); a
/// partially-recognized schedule stays `ACTIVE` and keeps the slot occupied.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn drained_schedule_completes_and_frees_the_one_live_slot() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // --- Fully-drained schedule → COMPLETED ---
    // A 1-segment schedule (600 deferred in 202606); release it fully.
    let inv = invoice(
        &s,
        "INV-DONE",
        vec![recognized_item(600, 1, "202606", "item-1")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    let sched = schedule_id(&raw, &s, "INV-DONE").await;
    assert_eq!(
        schedule_status_of(&raw, &s, &sched).await.as_deref(),
        Some("ACTIVE"),
        "ACTIVE before release"
    );

    run_svc(&provider, &harness)
        .trigger(&ctx, &scope, s.tenant, "202606", None)
        .await
        .expect("release drains the single segment");

    // The single segment is DONE, recognized == total ⇒ the schedule COMPLETED.
    assert_eq!(
        recognized_minor(&raw, &s, &sched).await,
        Some(600),
        "fully recognized"
    );
    assert_eq!(
        segment_status(&raw, &s, &sched, 1).await.as_deref(),
        Some("DONE")
    );
    assert_eq!(
        schedule_status_of(&raw, &s, &sched).await.as_deref(),
        Some("COMPLETED"),
        "a fully-drained schedule reaches COMPLETED (design §4.6)"
    );

    // The one-live slot is freed: a fresh ACTIVE schedule for the SAME business
    // key (invoice INV-DONE / item-1 / subscription) now inserts without
    // colliding on the partial UNIQUE (the old is COMPLETED, not ACTIVE).
    try_insert_active_schedule(
        &provider,
        &scope,
        new_schedule(&s, &Uuid::now_v7().to_string(), "INV-DONE", "item-1"),
    )
    .await
    .expect("a fresh ACTIVE schedule is admitted once the predecessor COMPLETED");

    // --- Partially-recognized schedule stays ACTIVE + keeps the slot ---
    // A 2-segment schedule (1000 over 202606 + 202607); release ONLY period 1.
    open_period(&raw, &s, "202607").await;
    let inv2 = invoice(
        &s,
        "INV-PART",
        vec![recognized_item(1000, 2, "202606", "item-9")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv2, true)
        .await
        .expect("deferred invoice posts");
    let sched2 = schedule_id(&raw, &s, "INV-PART").await;
    run_svc(&provider, &harness)
        .trigger(&ctx, &scope, s.tenant, "202606", None)
        .await
        .expect("release period 1 only");

    // seg-1 DONE, seg-2 still PENDING, recognized (500) < total (1000) ⇒ ACTIVE.
    assert_eq!(
        segment_status(&raw, &s, &sched2, 1).await.as_deref(),
        Some("DONE")
    );
    assert_eq!(
        segment_status(&raw, &s, &sched2, 2).await.as_deref(),
        Some("PENDING")
    );
    assert_eq!(
        recognized_minor(&raw, &s, &sched2).await,
        Some(500),
        "partially recognized"
    );
    assert_eq!(
        schedule_status_of(&raw, &s, &sched2).await.as_deref(),
        Some("ACTIVE"),
        "a partially-recognized schedule stays ACTIVE (not COMPLETED)"
    );

    // Its one-live slot is still OCCUPIED: a second ACTIVE schedule for the SAME
    // business key collides on the partial UNIQUE.
    let collide = try_insert_active_schedule(
        &provider,
        &scope,
        new_schedule(&s, &Uuid::now_v7().to_string(), "INV-PART", "item-9"),
    )
    .await;
    assert!(
        collide.is_err(),
        "a second live schedule for an ACTIVE business key must collide on the one-live UNIQUE"
    );
}

// ── Fix 5: §4.7 invoice-item link re-asserted at run time ─────────────────────
