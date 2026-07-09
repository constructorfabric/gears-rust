//! Postgres-only integration test for the Slice 4 ASC 606 **disaggregation**
//! (Group G2/G3), driven through the REAL stack: `InvoicePostService` (to
//! materialize per-stream deferred schedules + segments) →
//! `RecognitionRunService` (to release them) → `RecognitionRepo::
//! list_revenue_disaggregation` (the by-stream report the `GET
//! /revenue/disaggregation` endpoint reads through).
//!
//! Covers (design §3.5 / §4.5, Group G2):
//! - **per-stream drain**: a multi-stream deferred bundle (one item per stream,
//!   `subscription` + `usage`) materializes ONE schedule per stream (the partial
//!   UNIQUE on `(tenant, source_invoice_id, source_invoice_item_ref,
//!   revenue_stream)`); a run releases every due segment so EACH stream's
//!   per-stream `CONTRACT_LIABILITY` balance drains to zero;
//! - **disaggregation**: the report returns one entry per stream with the right
//!   `recognized_minor`, at the `(period_id, revenue_stream)` grain.
//!
//! `LedgerLocalClient::new` is `pub(crate)`, so this out-of-crate test drives the
//! `pub` services + repo directly (mirrors `postgres_recognition_run.rs`).
//! Ignored by default; run with `-- --ignored`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic,
    clippy::too_many_lines,
    clippy::similar_names
)]

use std::sync::Arc;

use bss_ledger::config::{FxConfig, RecognitionConfig};
use bss_ledger::domain::invoice::builder::{InvoiceItem, PostedInvoice, TaxBreakdown};
use bss_ledger::domain::model::{AccountRow, CurrencyScaleRow};
use bss_ledger::domain::money::DEFAULT_PLAUSIBLE_MAX_MAJOR;
use bss_ledger::domain::recognition::input::{RecognitionInput, RecognitionTiming};
use bss_ledger::infra::events::publisher::LedgerEventPublisher;
use bss_ledger::infra::invoice_post::InvoicePostService;
use bss_ledger::infra::metrics::test_harness::MetricsHarness;
use bss_ledger::infra::recognition::run_service::RecognitionRunService;
use bss_ledger::infra::storage::migrations::Migrator;
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

fn naive(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// Provisioned seller ids. Two revenue streams (`subscription` + `usage`), each
/// with its OWN per-stream `REVENUE` + `CONTRACT_LIABILITY` account, so a
/// per-stream drain is observable on distinct balance rows.
struct Seller {
    tenant: Uuid,
    payer: Uuid,
    ar: Uuid,
    revenue_sub: Uuid,
    revenue_use: Uuid,
    cl_sub: Uuid,
    cl_use: Uuid,
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

/// Force a period CLOSED (test shortcut — the harness raw-INSERTs OPEN periods via
/// [`open_period`]; this flips one to CLOSED so an E-2 missed-close is observable:
/// `fiscal_period` is mutable, unlike the append-only journal).
async fn close_period(raw: &DatabaseConnection, s: &Seller, period_id: &str) {
    raw.execute(pg(format!(
        "UPDATE bss.ledger_fiscal_period SET status='CLOSED' \
         WHERE tenant_id='{}' AND period_id='{period_id}'",
        s.tenant
    )))
    .await
    .unwrap();
}

/// Boot, migrate, seed USD@2 + an OPEN period + AR / TAX / SUSPENSE and the TWO
/// per-stream REVENUE + CONTRACT_LIABILITY accounts (`subscription` + `usage`).
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
        revenue_sub: Uuid::now_v7(),
        revenue_use: Uuid::now_v7(),
        cl_sub: Uuid::now_v7(),
        cl_use: Uuid::now_v7(),
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
            s.revenue_sub,
            AccountClass::Revenue,
            Side::Credit,
            Some("subscription"),
        ),
        account(
            s.tenant,
            s.revenue_use,
            AccountClass::Revenue,
            Side::Credit,
            Some("usage"),
        ),
        account(
            s.tenant,
            s.cl_sub,
            AccountClass::ContractLiability,
            Side::Credit,
            Some("subscription"),
        ),
        account(
            s.tenant,
            s.cl_use,
            AccountClass::ContractLiability,
            Side::Credit,
            Some("usage"),
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

/// A `stream` item with a straight-line recognition spec over `periods` months
/// from `first_period`, with its own `item_ref` (so each stream's schedule keys
/// on a distinct `(source_invoice_item_ref, revenue_stream)`).
fn recognized_item(
    amount: i64,
    stream: &str,
    periods: u32,
    first_period: &str,
    item_ref: &str,
) -> InvoiceItem {
    InvoiceItem {
        amount_minor_ex_tax: amount,
        deferred_minor: 0,
        currency: "USD".to_owned(),
        revenue_stream: stream.to_owned(),
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

// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn per_stream_bundle_drains_each_stream_and_disaggregates() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // One deferred bundle, ONE item per stream — each deferred straight-line over
    // 2 months from 202606 (so the segments land in 202606 + 202607). Distinct
    // item_refs ⇒ a distinct ACTIVE schedule per stream (the per-stream split,
    // §3.5). subscription: 1000 over 2 ⇒ 500 + 500; usage: 600 over 2 ⇒ 300 + 300.
    open_period(&raw, &s, "202607").await;
    let inv = invoice(
        &s,
        "INV-MULTI",
        vec![
            recognized_item(1000, "subscription", 2, "202606", "item-sub"),
            recognized_item(600, "usage", 2, "202606", "item-use"),
        ],
    );
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("multi-stream deferred invoice posts");

    // Two ACTIVE schedules (one per stream) materialized in the post txn.
    let schedule_count = scalar_i64(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.ledger_recognition_schedule \
             WHERE tenant_id='{}' AND source_invoice_id='INV-MULTI' AND status='ACTIVE'",
            s.tenant
        ),
    )
    .await;
    assert_eq!(schedule_count, Some(2), "one ACTIVE schedule per stream");

    // Each per-stream CONTRACT_LIABILITY account holds its own deferred total.
    assert_eq!(
        bal(&raw, &s, s.cl_sub).await,
        Some(1000),
        "subscription CL deferred"
    );
    assert_eq!(
        bal(&raw, &s, s.cl_use).await,
        Some(600),
        "usage CL deferred"
    );

    // Run the LATER period 202607 (releases both segments of BOTH schedules, in
    // order). 4 segments total (2 per stream).
    let svc = run_svc(&provider, &harness);
    let outcome = svc
        .trigger(&ctx, &scope, s.tenant, "202607", None)
        .await
        .expect("run releases every due segment");
    match outcome {
        RecognitionRunOutcome::Ran(r) => {
            assert_eq!(r.released, 4, "2 segments × 2 streams released fresh");
        }
        RecognitionRunOutcome::Queued(_) => panic!("in-order release must not queue"),
    }

    // (a) EACH stream's per-stream CONTRACT_LIABILITY balance drains to ZERO
    // (fully released), and each stream's Revenue is fully recognized.
    assert_eq!(
        bal(&raw, &s, s.cl_sub).await,
        Some(0),
        "subscription CL fully drained"
    );
    assert_eq!(
        bal(&raw, &s, s.cl_use).await,
        Some(0),
        "usage CL fully drained"
    );
    assert_eq!(
        bal(&raw, &s, s.revenue_sub).await,
        Some(1000),
        "subscription recognized"
    );
    assert_eq!(
        bal(&raw, &s, s.revenue_use).await,
        Some(600),
        "usage recognized"
    );

    // (b) The disaggregation query returns one entry per (period, stream) with the
    // right recognized_minor — over all periods (period_id = None). Ordered by
    // (period_id, revenue_stream): 202606/subscription, 202606/usage,
    // 202607/subscription, 202607/usage.
    let repo = RecognitionRepo::new(provider.clone());
    let all = repo
        .list_revenue_disaggregation(&scope, s.tenant, None)
        .await
        .expect("disaggregation over all periods");
    let got: Vec<(String, String, i64, String)> = all
        .iter()
        .map(|e| {
            (
                e.period_id.clone(),
                e.revenue_stream.clone(),
                e.recognized_minor,
                e.currency.clone(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (
                "202606".to_owned(),
                "subscription".to_owned(),
                500,
                "USD".to_owned()
            ),
            (
                "202606".to_owned(),
                "usage".to_owned(),
                300,
                "USD".to_owned()
            ),
            (
                "202607".to_owned(),
                "subscription".to_owned(),
                500,
                "USD".to_owned()
            ),
            (
                "202607".to_owned(),
                "usage".to_owned(),
                300,
                "USD".to_owned()
            ),
        ],
        "one entry per (period, stream) with Σ amount_minor, ordered"
    );

    // Per-stream totals across periods sum to each stream's deferred total.
    let sub_total: i64 = all
        .iter()
        .filter(|e| e.revenue_stream == "subscription")
        .map(|e| e.recognized_minor)
        .sum();
    let use_total: i64 = all
        .iter()
        .filter(|e| e.revenue_stream == "usage")
        .map(|e| e.recognized_minor)
        .sum();
    assert_eq!(
        sub_total, 1000,
        "subscription recognized in full across periods"
    );
    assert_eq!(use_total, 600, "usage recognized in full across periods");

    // (c) Narrowing to one period returns only that period's two streams.
    let p1 = repo
        .list_revenue_disaggregation(&scope, s.tenant, Some("202606"))
        .await
        .expect("disaggregation for 202606");
    let p1_got: Vec<(String, i64)> = p1
        .iter()
        .map(|e| (e.revenue_stream.clone(), e.recognized_minor))
        .collect();
    assert_eq!(
        p1_got,
        vec![("subscription".to_owned(), 500), ("usage".to_owned(), 300)],
        "202606 narrows to that period's per-stream recognized revenue"
    );

    // (d) BOLA: a foreign tenant's scope yields no entries.
    let foreign = AccessScope::for_tenant(Uuid::now_v7());
    let none = repo
        .list_revenue_disaggregation(&foreign, s.tenant, None)
        .await
        .expect("foreign-scope read succeeds but yields nothing");
    assert!(
        none.is_empty(),
        "a foreign tenant scope sees no recognized revenue"
    );
}

/// **E-2 missed-close → the report buckets revenue under the ACTUAL (open) period,
/// not the planned/closed one** (design §4.3 / §4.5). A segment keeps its planned
/// `period_id` as the audit target, but an E-2 missed-close releases it INTO the
/// current open period; the journal entry (hence `journal_line.period_id`) carries
/// that open period. Because the disaggregation read is journal-sourced, it reports
/// the period the revenue truly landed in — whereas a DONE-segment scan would
/// wrongly split it under the closed planned period. (The same journal source makes
/// the report reversal-aware — a `DR REVENUE` clawback nets out — exercised by the
/// release/reversal paths in `postgres_recognition_change.rs`.)
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn missed_close_disaggregates_under_the_open_period() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await; // setup opens 202606 + seeds accounts
    let harness = MetricsHarness::new();
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // The schedule straight-lines 1000 over 202605 + 202606 (500 each). Open 202605
    // so the deferred invoice can post there; both segments materialize PENDING.
    open_period(&raw, &s, "202605").await;
    let inv = PostedInvoice {
        invoice_id: "INV-MISSED".to_owned(),
        payer_tenant_id: s.payer,
        resource_tenant_id: None,
        seller_tenant_id: s.tenant,
        effective_at: naive(2026, 5, 1),
        due_date: Some(naive(2026, 6, 1)),
        period_id: "202605".to_owned(),
        items: vec![recognized_item(
            1000,
            "subscription",
            2,
            "202605",
            "item-sub",
        )],
        tax: Vec::<TaxBreakdown>::new(),
        posted_by_actor_id: s.tenant,
        correlation_id: s.tenant,
    };
    invoice_svc(&provider, &harness)
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice posts in 202605");

    // 202605 now CLOSES; 202606 (opened by setup) is the current open period.
    close_period(&raw, &s, "202605").await;

    // Run the open period 202606: BOTH segments are due (period ≤ 202606). The
    // 202605 segment's planned period is closed (< open 202606) ⇒ E-2 releases it
    // INTO 202606; the 202606 segment releases into its own (open) period. All 1000
    // lands in 202606.
    let svc = run_svc(&provider, &harness);
    let outcome = svc
        .trigger(&ctx, &scope, s.tenant, "202606", None)
        .await
        .expect("run releases both due segments into the open period");
    match outcome {
        RecognitionRunOutcome::Ran(r) => assert_eq!(r.released, 2, "both segments released fresh"),
        RecognitionRunOutcome::Queued(_) => panic!("in-order release must not queue"),
    }

    // CONTRACT_LIABILITY fully drained; REVENUE fully recognized.
    assert_eq!(bal(&raw, &s, s.cl_sub).await, Some(0), "CL fully drained");
    assert_eq!(
        bal(&raw, &s, s.revenue_sub).await,
        Some(1000),
        "revenue fully recognized"
    );

    // Disaggregation (all periods): BOTH releases bucket under 202606 — the ACTUAL
    // open period the entries posted into — with NOTHING under the planned-but-
    // closed 202605. (A segment-period scan would wrongly split 500/500.)
    let repo = RecognitionRepo::new(provider.clone());
    let all = repo
        .list_revenue_disaggregation(&scope, s.tenant, None)
        .await
        .expect("disaggregation over all periods");
    let got: Vec<(String, String, i64, String)> = all
        .iter()
        .map(|e| {
            (
                e.period_id.clone(),
                e.revenue_stream.clone(),
                e.recognized_minor,
                e.currency.clone(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![(
            "202606".to_owned(),
            "subscription".to_owned(),
            1000,
            "USD".to_owned()
        )],
        "all recognized revenue is reported under the ACTUAL open period 202606"
    );
    assert!(
        all.iter().all(|e| e.period_id != "202605"),
        "no revenue is reported under the planned-but-closed period 202605"
    );

    // Narrowing confirms it: the closed planned period reports nothing; the open
    // period reports the full recognized revenue.
    let p_closed = repo
        .list_revenue_disaggregation(&scope, s.tenant, Some("202605"))
        .await
        .expect("query 202605");
    assert!(
        p_closed.is_empty(),
        "the closed planned period reports no recognized revenue"
    );
    let p_open: Vec<(String, i64)> = repo
        .list_revenue_disaggregation(&scope, s.tenant, Some("202606"))
        .await
        .expect("query 202606")
        .iter()
        .map(|e| (e.revenue_stream.clone(), e.recognized_minor))
        .collect();
    assert_eq!(
        p_open,
        vec![("subscription".to_owned(), 1000)],
        "the open period reports the full recognized revenue"
    );
}
