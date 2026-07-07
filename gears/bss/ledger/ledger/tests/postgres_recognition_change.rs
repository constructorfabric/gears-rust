//! Postgres-only integration tests for the Slice 4 Group H schedule
//! change/cancel path (design §3.6 / §4.6), driven through the REAL stack:
//! `InvoicePostService` (materialize a deferred schedule) →
//! `RecognitionRunService` (release a period) → `RecognitionChangeService`
//! (cancel / replace) → `RecognitionRunService` again (verify the new/old
//! schedule release behaviour).
//!
//! Covers (Group H4):
//! - **replace (prospective)**: a 1200/3-period schedule, release period 1 (400),
//!   then `replace` the remaining 800 over fresh segments → old `REPLACED`, a new
//!   ACTIVE schedule with `total_deferred = 800` + its PENDING segments, the
//!   already-DONE segment stays DONE, a run on the new period releases from the
//!   NEW schedule, and the OLD schedule's remaining segments do NOT release;
//! - **cancel**: post + cancel → schedule `CANCELLED`, a later run releases
//!   nothing for it;
//! - **catch_up** → `ModificationTreatmentReview`, schedule unchanged (ACTIVE);
//! - **idempotent** `change_id` replay → same result, no second new schedule.
//!
//! `LedgerLocalClient::new` is `pub(crate)`, so this out-of-crate test drives the
//! `pub` services directly (mirrors `postgres_recognition_run.rs`). Ignored by
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
    clippy::too_many_arguments,
    clippy::similar_names
)]

use std::sync::Arc;

use bss_ledger::config::{FxConfig, RecognitionConfig};
use bss_ledger::domain::error::DomainError;
use bss_ledger::domain::invoice::builder::{InvoiceItem, PostedInvoice, TaxBreakdown};
use bss_ledger::domain::model::{AccountRow, CurrencyScaleRow};
use bss_ledger::domain::money::DEFAULT_PLAUSIBLE_MAX_MAJOR;
use bss_ledger::domain::recognition::input::{RecognitionInput, RecognitionTiming};
use bss_ledger::infra::events::publisher::LedgerEventPublisher;
use bss_ledger::infra::invoice_post::InvoicePostService;
use bss_ledger::infra::metrics::test_harness::MetricsHarness;
use bss_ledger::infra::recognition::change_service::RecognitionChangeService;
use bss_ledger::infra::recognition::run_service::RecognitionRunService;
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::ReferenceRepo;
use bss_ledger_sdk::{AccountClass, ChangeRecognitionSchedule, ChangeSegment, Side};
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

async fn scalar_str(conn: &DatabaseConnection, sql: &str) -> Option<String> {
    conn.query_one(pg(sql.to_owned()))
        .await
        .unwrap()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
}

async fn count(conn: &DatabaseConnection, sql: &str) -> i64 {
    scalar_i64(conn, sql).await.unwrap_or(0)
}

fn naive(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// Provisioned seller ids (mirrors `postgres_recognition_run::Seller`).
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

async fn open_period(raw: &DatabaseConnection, s: &Seller, period_id: &str) {
    raw.execute(pg(format!(
        "INSERT INTO bss.ledger_fiscal_period (tenant_id, legal_entity_id, period_id, fiscal_tz, status)
         VALUES ('{}','{}','{period_id}','UTC','OPEN')",
        s.tenant, s.tenant
    )))
    .await
    .unwrap();
}

/// Boot, migrate, seed USD@2 + an OPEN period + the recognition accounts. Mirrors
/// `postgres_recognition_run::setup`.
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

fn change_svc(provider: &DBProvider<DbError>) -> RecognitionChangeService {
    RecognitionChangeService::new(provider.clone(), Arc::new(LedgerEventPublisher::noop()))
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

/// The ACTIVE schedule_id for an invoice (there is exactly one ACTIVE at a time).
async fn active_schedule_id(raw: &DatabaseConnection, s: &Seller, invoice_id: &str) -> String {
    scalar_str(
        raw,
        &format!(
            "SELECT schedule_id FROM bss.ledger_recognition_schedule \
             WHERE tenant_id='{}' AND source_invoice_id='{invoice_id}' AND status='ACTIVE'",
            s.tenant
        ),
    )
    .await
    .expect("an ACTIVE schedule for the invoice")
}

async fn schedule_status(raw: &DatabaseConnection, s: &Seller, schedule: &str) -> Option<String> {
    scalar_str(
        raw,
        &format!(
            "SELECT status FROM bss.ledger_recognition_schedule \
             WHERE tenant_id='{}' AND schedule_id='{schedule}'",
            s.tenant
        ),
    )
    .await
}

async fn total_deferred(raw: &DatabaseConnection, s: &Seller, schedule: &str) -> Option<i64> {
    scalar_i64(
        raw,
        &format!(
            "SELECT total_deferred_minor FROM bss.ledger_recognition_schedule \
             WHERE tenant_id='{}' AND schedule_id='{schedule}'",
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
    scalar_str(
        raw,
        &format!(
            "SELECT status FROM bss.ledger_recognition_segment \
             WHERE tenant_id='{}' AND schedule_id='{schedule}' AND segment_no={segment_no}",
            s.tenant
        ),
    )
    .await
}

async fn segment_count(raw: &DatabaseConnection, s: &Seller, schedule: &str) -> i64 {
    count(
        raw,
        &format!(
            "SELECT COUNT(*) FROM bss.ledger_recognition_segment \
             WHERE tenant_id='{}' AND schedule_id='{schedule}'",
            s.tenant
        ),
    )
    .await
}

/// Count ACTIVE schedules for an invoice (the one-live invariant; a replace must
/// keep this at exactly 1).
async fn active_schedule_count(raw: &DatabaseConnection, s: &Seller, invoice_id: &str) -> i64 {
    count(
        raw,
        &format!(
            "SELECT COUNT(*) FROM bss.ledger_recognition_schedule \
             WHERE tenant_id='{}' AND source_invoice_id='{invoice_id}' AND status='ACTIVE'",
            s.tenant
        ),
    )
    .await
}

fn seg(period_id: &str, amount: i64) -> ChangeSegment {
    ChangeSegment {
        period_id: period_id.to_owned(),
        amount_minor: amount,
    }
}

// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn replace_prospective_re_plans_remaining_and_old_segments_do_not_release() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // A 1200 / 3-period straight-line schedule: seg-1 202606 (400), seg-2 202607
    // (400), seg-3 202608 (400). Open all three periods.
    open_period(&raw, &s, "202607").await;
    open_period(&raw, &s, "202608").await;
    let inv = invoice(
        &s,
        "INV-RPL",
        vec![recognized_item(1200, 3, "202606", "item-1")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    let old = active_schedule_id(&raw, &s, "INV-RPL").await;
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(1200),
        "fully deferred"
    );

    // Release period 1 only (releases seg-1 = 400). recognized = 400, CL = 800.
    run_svc(&provider, &harness)
        .trigger(&ctx, &scope, s.tenant, "202606", None)
        .await
        .expect("release period 1");
    assert_eq!(
        segment_status(&raw, &s, &old, 1).await.as_deref(),
        Some("DONE")
    );
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(800),
        "remaining deferred"
    );
    assert_eq!(
        bal(&raw, &s, s.revenue).await,
        Some(400),
        "period 1 recognized"
    );

    // REPLACE the remaining 800 over two fresh segments in 202607 + 202608
    // (prospective). The supplied segments MUST sum to the remaining 800.
    let cmd = ChangeRecognitionSchedule {
        tenant_id: s.tenant,
        schedule_id: old.clone(),
        change_id: "chg-rpl-1".to_owned(),
        action: "replace".to_owned(),
        treatment: "prospective".to_owned(),
        new_segments: Some(vec![seg("202607", 300), seg("202608", 500)]),
    };
    let result = change_svc(&provider)
        .change(&ctx, &scope, cmd)
        .await
        .expect("replace applies");
    assert_eq!(result.status, "REPLACED");
    let new_id = result
        .new_schedule_id
        .clone()
        .expect("a successor schedule");
    assert_ne!(new_id, old, "the successor is a fresh schedule_id");

    // Old → REPLACED (its DONE seg-1 untouched); new → ACTIVE with total 800 + two
    // PENDING segments; exactly one ACTIVE schedule for the invoice.
    assert_eq!(
        schedule_status(&raw, &s, &old).await.as_deref(),
        Some("REPLACED")
    );
    assert_eq!(
        segment_status(&raw, &s, &old, 1).await.as_deref(),
        Some("DONE"),
        "DONE stays DONE"
    );
    assert_eq!(
        schedule_status(&raw, &s, &new_id).await.as_deref(),
        Some("ACTIVE")
    );
    assert_eq!(
        total_deferred(&raw, &s, &new_id).await,
        Some(800),
        "remaining re-planned"
    );
    assert_eq!(
        segment_count(&raw, &s, &new_id).await,
        2,
        "two replacement segments"
    );
    assert_eq!(
        segment_status(&raw, &s, &new_id, 1).await.as_deref(),
        Some("PENDING")
    );
    assert_eq!(
        segment_status(&raw, &s, &new_id, 2).await.as_deref(),
        Some("PENDING")
    );
    assert_eq!(
        active_schedule_count(&raw, &s, "INV-RPL").await,
        1,
        "exactly one ACTIVE"
    );
    // No compensating entry: CL + Revenue unchanged by the replace itself.
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(800),
        "CL unchanged by replace"
    );
    assert_eq!(
        bal(&raw, &s, s.revenue).await,
        Some(400),
        "Revenue unchanged by replace"
    );

    // A run over 202608 releases the NEW schedule's two segments (300 + 500 = 800)
    // — and the OLD schedule's remaining seg-2/seg-3 do NOT release (it is
    // REPLACED, excluded from the runner's ACTIVE-only feed).
    run_svc(&provider, &harness)
        .trigger(&ctx, &scope, s.tenant, "202608", None)
        .await
        .expect("release the new schedule");
    assert_eq!(
        segment_status(&raw, &s, &new_id, 1).await.as_deref(),
        Some("DONE")
    );
    assert_eq!(
        segment_status(&raw, &s, &new_id, 2).await.as_deref(),
        Some("DONE")
    );
    // The OLD schedule's seg-2 / seg-3 are still PENDING (never released).
    assert_eq!(
        segment_status(&raw, &s, &old, 2).await.as_deref(),
        Some("PENDING"),
        "old seg untouched"
    );
    assert_eq!(
        segment_status(&raw, &s, &old, 3).await.as_deref(),
        Some("PENDING"),
        "old seg untouched"
    );
    // Books: CL fully drained (800 released), Revenue == 1200 total (400 + 800).
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(0),
        "CL drained via the new schedule"
    );
    assert_eq!(
        bal(&raw, &s, s.revenue).await,
        Some(1200),
        "all recognized, none double"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn cancel_marks_cancelled_and_later_run_releases_nothing() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // A single 600 / 1-period schedule, never released.
    let inv = invoice(
        &s,
        "INV-CXL",
        vec![recognized_item(600, 1, "202606", "item-1")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    let sched = active_schedule_id(&raw, &s, "INV-CXL").await;
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(600),
        "deferred"
    );

    // CANCEL it. The unreleased deferred remainder stays as CONTRACT_LIABILITY (no
    // auto-reversal); the schedule flips CANCELLED, no successor.
    let cmd = ChangeRecognitionSchedule {
        tenant_id: s.tenant,
        schedule_id: sched.clone(),
        change_id: "chg-cxl-1".to_owned(),
        action: "cancel".to_owned(),
        treatment: "prospective".to_owned(),
        new_segments: None,
    };
    let result = change_svc(&provider)
        .change(&ctx, &scope, cmd)
        .await
        .expect("cancel applies");
    assert_eq!(result.status, "CANCELLED");
    assert!(
        result.new_schedule_id.is_none(),
        "cancel mints no successor"
    );
    assert_eq!(
        schedule_status(&raw, &s, &sched).await.as_deref(),
        Some("CANCELLED")
    );
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(600),
        "remainder stays as CL"
    );

    // A later run for the period releases nothing for the CANCELLED schedule.
    run_svc(&provider, &harness)
        .trigger(&ctx, &scope, s.tenant, "202606", None)
        .await
        .expect("run is a no-op for the cancelled schedule");
    assert_eq!(
        segment_status(&raw, &s, &sched, 1).await.as_deref(),
        Some("PENDING"),
        "never released"
    );
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(600),
        "still deferred"
    );
    assert_eq!(bal(&raw, &s, s.revenue).await, None, "nothing recognized");
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn catch_up_treatment_is_review_and_leaves_schedule_active() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    let inv = invoice(
        &s,
        "INV-CU",
        vec![recognized_item(600, 1, "202606", "item-1")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    let sched = active_schedule_id(&raw, &s, "INV-CU").await;

    // A catch_up treatment is surfaced for review with NO state change (§3.6).
    let cmd = ChangeRecognitionSchedule {
        tenant_id: s.tenant,
        schedule_id: sched.clone(),
        change_id: "chg-cu-1".to_owned(),
        action: "cancel".to_owned(),
        treatment: "catch_up".to_owned(),
        new_segments: None,
    };
    let err = change_svc(&provider)
        .change(&ctx, &scope, cmd)
        .await
        .expect_err("catch_up must be a review");
    assert!(
        matches!(err, DomainError::ModificationTreatmentReview(_)),
        "expected ModificationTreatmentReview, got {err:?}"
    );

    // The schedule is unchanged — still ACTIVE, and no dedup row should block a
    // later legitimate change (the treatment gate ran BEFORE the claim).
    assert_eq!(
        schedule_status(&raw, &s, &sched).await.as_deref(),
        Some("ACTIVE"),
        "still ACTIVE"
    );
    assert_eq!(active_schedule_count(&raw, &s, "INV-CU").await, 1);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn replace_is_idempotent_on_change_id() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // A 900 / 1-period schedule (never released ⇒ remaining = 900).
    let inv = invoice(
        &s,
        "INV-IDEM",
        vec![recognized_item(900, 1, "202606", "item-1")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    let old = active_schedule_id(&raw, &s, "INV-IDEM").await;

    let mk = || ChangeRecognitionSchedule {
        tenant_id: s.tenant,
        schedule_id: old.clone(),
        change_id: "chg-idem-1".to_owned(),
        action: "replace".to_owned(),
        treatment: "prospective".to_owned(),
        new_segments: Some(vec![seg("202606", 900)]),
    };

    let first = change_svc(&provider)
        .change(&ctx, &scope, mk())
        .await
        .expect("first replace applies");
    let new_id = first.new_schedule_id.clone().expect("a successor");
    assert_eq!(first.status, "REPLACED");

    // Replay the SAME change_id: same result (same successor id), and NO second
    // new schedule is minted.
    let replay = change_svc(&provider)
        .change(&ctx, &scope, mk())
        .await
        .expect("replay returns the prior result");
    assert_eq!(replay.status, "REPLACED");
    assert_eq!(
        replay.new_schedule_id.as_deref(),
        Some(new_id.as_str()),
        "replay returns the same successor id"
    );

    // Exactly ONE ACTIVE successor exists for the invoice (no second mint).
    assert_eq!(
        active_schedule_count(&raw, &s, "INV-IDEM").await,
        1,
        "no second new schedule"
    );
    // Total schedules for the invoice = 2 (the REPLACED old + the one ACTIVE new).
    let total = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.ledger_recognition_schedule \
             WHERE tenant_id='{}' AND source_invoice_id='INV-IDEM'",
            s.tenant
        ),
    )
    .await;
    assert_eq!(total, 2, "old REPLACED + exactly one ACTIVE successor");
}

// ── Fix 2: replacement-segment PERIOD validation (design §4.6) ────────────────

/// Post a fresh single-period deferred schedule (`amount` deferred in 202606,
/// never released ⇒ remaining == amount) and return its ACTIVE schedule_id — the
/// substrate for the period-validation rejection tests (the supplied replacement
/// segments sum to `amount`, so the sum check passes and the PERIOD check is what
/// fires).
async fn fresh_schedule(
    raw: &DatabaseConnection,
    provider: &DBProvider<DbError>,
    harness: &MetricsHarness,
    ctx: &SecurityContext,
    scope: &AccessScope,
    s: &Seller,
    invoice_id: &str,
    amount: i64,
) -> String {
    let inv = invoice(
        s,
        invoice_id,
        vec![recognized_item(amount, 1, "202606", "item-1")],
    );
    invoice_svc(provider, harness)
        .post_invoice(ctx, scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    active_schedule_id(raw, s, invoice_id).await
}

/// Build a `replace` command for `schedule_id` with the given replacement
/// segments (treatment `prospective`, a unique `change_id`).
fn replace_cmd(
    s: &Seller,
    schedule_id: &str,
    change_id: &str,
    segments: Vec<ChangeSegment>,
) -> ChangeRecognitionSchedule {
    ChangeRecognitionSchedule {
        tenant_id: s.tenant,
        schedule_id: schedule_id.to_owned(),
        change_id: change_id.to_owned(),
        action: "replace".to_owned(),
        treatment: "prospective".to_owned(),
        new_segments: Some(segments),
    }
}

/// Replacement segments in DESCENDING period order are rejected with a 400
/// (`InvalidRequest`) — the periods must be strictly ascending.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn replace_with_descending_periods_is_rejected() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    let sched = fresh_schedule(
        &raw, &provider, &harness, &ctx, &scope, &s, "INV-DESC", 1000,
    )
    .await;
    // Sums to 1000 (remaining), but 202607 then 202606 is descending.
    let cmd = replace_cmd(
        &s,
        &sched,
        "chg-desc",
        vec![seg("202607", 500), seg("202606", 500)],
    );
    let err = change_svc(&provider)
        .change(&ctx, &scope, cmd)
        .await
        .expect_err("descending replacement periods must be rejected");
    assert!(
        matches!(err, DomainError::InvalidRequest(_)),
        "expected InvalidRequest (400), got {err:?}"
    );
    // The schedule is untouched — still ACTIVE (the validation ran before the flip).
    assert_eq!(
        schedule_status(&raw, &s, &sched).await.as_deref(),
        Some("ACTIVE")
    );
    assert_eq!(active_schedule_count(&raw, &s, "INV-DESC").await, 1);
}

/// Replacement segments with a DUPLICATE period are rejected with a 400 — the
/// periods must be distinct.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn replace_with_duplicate_period_is_rejected() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    let sched = fresh_schedule(&raw, &provider, &harness, &ctx, &scope, &s, "INV-DUP", 1000).await;
    // Sums to 1000, but 202606 appears twice (not distinct).
    let cmd = replace_cmd(
        &s,
        &sched,
        "chg-dup",
        vec![seg("202606", 500), seg("202606", 500)],
    );
    let err = change_svc(&provider)
        .change(&ctx, &scope, cmd)
        .await
        .expect_err("a duplicate replacement period must be rejected");
    assert!(
        matches!(err, DomainError::InvalidRequest(_)),
        "expected InvalidRequest (400), got {err:?}"
    );
    assert_eq!(
        schedule_status(&raw, &s, &sched).await.as_deref(),
        Some("ACTIVE")
    );
}

/// A MALFORMED replacement period (not a valid `YYYYMM`) is rejected with a 400.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn replace_with_malformed_period_is_rejected() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    let sched = fresh_schedule(&raw, &provider, &harness, &ctx, &scope, &s, "INV-MAL", 1000).await;
    // Sums to 1000, but "202613" is not a valid YYYYMM (month 13).
    let cmd = replace_cmd(&s, &sched, "chg-mal", vec![seg("202613", 1000)]);
    let err = change_svc(&provider)
        .change(&ctx, &scope, cmd)
        .await
        .expect_err("a malformed replacement period must be rejected");
    assert!(
        matches!(err, DomainError::InvalidRequest(_)),
        "expected InvalidRequest (400), got {err:?}"
    );
    assert_eq!(
        schedule_status(&raw, &s, &sched).await.as_deref(),
        Some("ACTIVE")
    );
}

/// A replacement whose first period overlaps an ALREADY-DONE period of the old
/// schedule is rejected with a 400 — a replacement can never re-recognize a
/// period that already posted (cross-version double-recognition).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn replace_overlapping_an_already_done_period_is_rejected() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // A 1200 / 3-period schedule (202606/202607/202608, 400 each). Release period
    // 1 so 202606 is DONE; remaining = 800.
    open_period(&raw, &s, "202607").await;
    open_period(&raw, &s, "202608").await;
    let inv = invoice(
        &s,
        "INV-OVL",
        vec![recognized_item(1200, 3, "202606", "item-1")],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts");
    let old = active_schedule_id(&raw, &s, "INV-OVL").await;
    run_svc(&provider, &harness)
        .trigger(&ctx, &scope, s.tenant, "202606", None)
        .await
        .expect("release period 1");
    assert_eq!(
        segment_status(&raw, &s, &old, 1).await.as_deref(),
        Some("DONE")
    );

    // Replace the remaining 800 but with a FIRST period (202606) that overlaps the
    // already-DONE 202606. Sums to 800 (remaining), so the sum check passes and the
    // period-floor check is what rejects it.
    let cmd = replace_cmd(&s, &old, "chg-ovl", vec![seg("202606", 800)]);
    let err = change_svc(&provider)
        .change(&ctx, &scope, cmd)
        .await
        .expect_err("a replacement re-targeting an already-DONE period must be rejected");
    assert!(
        matches!(err, DomainError::InvalidRequest(_)),
        "expected InvalidRequest (400), got {err:?}"
    );
    // The old schedule is untouched — still ACTIVE, no successor minted.
    assert_eq!(
        schedule_status(&raw, &s, &old).await.as_deref(),
        Some("ACTIVE")
    );
    assert_eq!(
        active_schedule_count(&raw, &s, "INV-OVL").await,
        1,
        "no successor"
    );
}
