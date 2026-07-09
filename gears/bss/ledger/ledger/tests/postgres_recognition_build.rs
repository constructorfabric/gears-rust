//! Postgres-only integration: the Slice 4 in-transaction recognition-schedule
//! materialization (Group C), driven through the REAL foundation engine
//! (`InvoicePostService` over `PostingService` + the `ScheduleBuilderSidecar`).
//!
//! Provisions a seller (AR / Revenue(subscription) / Contract-liability
//! (subscription) / Tax / Suspense chart + USD@2 + an OPEN period), then asserts:
//! - a **deferred** invoice materializes a `recognition_schedule` + its
//!   `recognition_segment` rows in the SAME txn as the `CR CONTRACT_LIABILITY`
//!   credit (balances + schedule both present);
//! - a derivation failure (segment count over the configured ceiling) rolls the
//!   WHOLE post back — no entry, no schedule, no balances;
//! - a duplicate build (re-post of the same invoice/item/stream) lands on the
//!   existing ACTIVE schedule via the `SCHEDULE_BUILD` claim — no second
//!   `schedule_id`, no second segment set;
//! - a `deferred = 0` invoice posts byte-identically — no Contract-liability
//!   line, no schedule.
//!
//! `LedgerLocalClient::new` is `pub(crate)`, so this out-of-crate test drives the
//! `pub` `InvoicePostService` directly (mirrors `postgres_invoice_post.rs`).
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
    clippy::type_complexity
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
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::ReferenceRepo;
use bss_ledger_sdk::{AccountClass, Side};
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
/// deferred split credits).
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

/// Boot, migrate, seed USD@2 + an OPEN period + AR / REVENUE(subscription) /
/// CONTRACT_LIABILITY(subscription) / TAX / SUSPENSE accounts.
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
    raw.execute(pg(format!(
        "INSERT INTO bss.ledger_fiscal_period (tenant_id, legal_entity_id, period_id, fiscal_tz, status)
         VALUES ('{}','{}','{}','UTC','OPEN')",
        s.tenant, s.tenant, s.period_id
    )))
    .await
    .unwrap();

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

/// A `subscription` item mapped to REVENUE, carrying a straight-line recognition
/// spec over `periods` and an `invoice_item_ref` (required for a deferred line).
fn recognized_item(amount: i64, periods: u32, item_ref: &str) -> InvoiceItem {
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
                first_period_id: None,
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

/// A plain `subscription` item, fully recognized now (no recognition spec).
fn plain_item(amount: i64) -> InvoiceItem {
    InvoiceItem {
        amount_minor_ex_tax: amount,
        deferred_minor: 0,
        currency: "USD".to_owned(),
        revenue_stream: "subscription".to_owned(),
        catalog_class: Some(AccountClass::Revenue),
        contract_class: None,
        gl_code: Some("4000".to_owned()),
        recognition: None,
        invoice_item_ref: None,
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

fn svc(
    provider: &DBProvider<DbError>,
    metrics: &MetricsHarness,
    cfg: RecognitionConfig,
) -> InvoicePostService {
    InvoicePostService::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(metrics.metrics()),
        cfg,
        FxConfig::default(),
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

async fn schedule_count(raw: &DatabaseConnection, s: &Seller, invoice_id: &str) -> i64 {
    count(
        raw,
        &format!(
            "SELECT COUNT(*) FROM bss.ledger_recognition_schedule \
             WHERE tenant_id='{}' AND source_invoice_id='{invoice_id}'",
            s.tenant
        ),
    )
    .await
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn deferred_invoice_materializes_schedule_in_one_txn() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let service = svc(&provider, &harness, RecognitionConfig::default());
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // 1200 ex-tax, straight-line over 12 months ⇒ the WHOLE amount defers to
    // CONTRACT_LIABILITY (Group B defers the whole ex-tax for a straight-line
    // line), recognized-now Revenue = 0.
    let inv = invoice(&s, "INV-DEF", vec![recognized_item(1200, 12, "item-1")]);
    let posted = service
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("deferred invoice must post");
    assert!(!posted.replayed);

    // Balances: AR 1200 debit, Contract-liability 1200 credit, Revenue 0.
    assert_eq!(bal(&raw, &s, s.ar).await, Some(1200), "AR = gross");
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        Some(1200),
        "the whole amount deferred to Contract-liability"
    );
    // Revenue line posts 0 ⇒ either no balance row or a 0 balance; both are fine.
    assert!(
        matches!(bal(&raw, &s, s.revenue).await, None | Some(0)),
        "nothing recognized now"
    );

    // Schedule + segments materialized in the same txn.
    assert_eq!(schedule_count(&raw, &s, "INV-DEF").await, 1, "one schedule");
    let total_deferred = scalar_i64(
        &raw,
        &format!(
            "SELECT total_deferred_minor FROM bss.ledger_recognition_schedule \
             WHERE tenant_id='{}' AND source_invoice_id='INV-DEF'",
            s.tenant
        ),
    )
    .await;
    assert_eq!(total_deferred, Some(1200), "schedule total = deferred");
    let segs = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.ledger_recognition_segment seg \
             JOIN bss.ledger_recognition_schedule sch \
               ON sch.tenant_id = seg.tenant_id AND sch.schedule_id = seg.schedule_id \
             WHERE sch.tenant_id='{}' AND sch.source_invoice_id='INV-DEF'",
            s.tenant
        ),
    )
    .await;
    assert_eq!(segs, 12, "12 straight-line segments");
    let seg_sum = scalar_i64(
        &raw,
        &format!(
            "SELECT SUM(seg.amount_minor)::bigint FROM bss.ledger_recognition_segment seg \
             JOIN bss.ledger_recognition_schedule sch \
               ON sch.tenant_id = seg.tenant_id AND sch.schedule_id = seg.schedule_id \
             WHERE sch.tenant_id='{}' AND sch.source_invoice_id='INV-DEF'",
            s.tenant
        ),
    )
    .await;
    assert_eq!(seg_sum, Some(1200), "segments sum to the deferred amount");
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn over_ceiling_derivation_rolls_back_the_whole_post() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    // Ceiling of 6 ⇒ a 12-segment schedule is over-bound and blocks.
    let cfg = RecognitionConfig {
        max_segments_per_schedule: 6,
        recognition_run_tick_secs: 300,
    };
    let service = svc(&provider, &harness, cfg);
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    let inv = invoice(&s, "INV-LONG", vec![recognized_item(1200, 12, "item-1")]);
    let err = service
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect_err("a schedule over the ceiling must block the post");
    assert!(
        matches!(err, DomainError::ScheduleTooLong(_)),
        "got {err:?}"
    );

    // The WHOLE post rolled back: no entry, no balances, no schedule.
    assert_eq!(
        bal(&raw, &s, s.ar).await,
        None,
        "no AR balance (rolled back)"
    );
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        None,
        "no Contract-liability balance (rolled back)"
    );
    assert_eq!(
        schedule_count(&raw, &s, "INV-LONG").await,
        0,
        "no schedule materialized"
    );
    let entries = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.ledger_journal_entry \
             WHERE tenant_id='{}' AND source_business_id='INV-LONG'",
            s.tenant
        ),
    )
    .await;
    assert_eq!(entries, 0, "no journal entry posted");
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn duplicate_build_lands_on_existing_active_schedule() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let service = svc(&provider, &harness, RecognitionConfig::default());
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    let inv = invoice(&s, "INV-DUP", vec![recognized_item(1200, 12, "item-1")]);

    let first = service
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("first deferred post must succeed");
    assert!(!first.replayed);
    assert_eq!(schedule_count(&raw, &s, "INV-DUP").await, 1, "one schedule");
    let first_schedule_id: Option<String> = raw
        .query_one(pg(format!(
            "SELECT schedule_id FROM bss.ledger_recognition_schedule \
             WHERE tenant_id='{}' AND source_invoice_id='INV-DUP'",
            s.tenant
        )))
        .await
        .unwrap()
        .map(|r| r.try_get_by_index::<String>(0).unwrap());

    // Re-post the SAME invoice ⇒ the journal post replays (INVOICE_POST dedup)
    // AND the SCHEDULE_BUILD claim short-circuits the sidecar — no second
    // schedule, no second segment set.
    let replay = service
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("the re-post must replay");
    assert!(replay.replayed, "the invoice post replays");
    assert_eq!(
        replay.entry_id, first.entry_id,
        "the replay returns the prior entry id"
    );
    assert_eq!(
        schedule_count(&raw, &s, "INV-DUP").await,
        1,
        "still exactly ONE schedule (no second schedule_id)"
    );
    let after_schedule_id: Option<String> = raw
        .query_one(pg(format!(
            "SELECT schedule_id FROM bss.ledger_recognition_schedule \
             WHERE tenant_id='{}' AND source_invoice_id='INV-DUP'",
            s.tenant
        )))
        .await
        .unwrap()
        .map(|r| r.try_get_by_index::<String>(0).unwrap());
    assert_eq!(
        first_schedule_id, after_schedule_id,
        "the same ACTIVE schedule_id survives the duplicate build"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn non_deferred_invoice_posts_with_no_schedule_or_cl_line() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let service = svc(&provider, &harness, RecognitionConfig::default());
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // No recognition spec ⇒ fully recognized now (byte-identical to today): AR +
    // Revenue only, NO Contract-liability line, NO schedule.
    let inv = invoice(&s, "INV-PLAIN", vec![plain_item(1000)]);
    let posted = service
        .post_invoice(&ctx, &scope, &inv, true)
        .await
        .expect("a non-deferred invoice must post");
    assert!(!posted.replayed);

    assert_eq!(
        bal(&raw, &s, s.ar).await,
        Some(1000),
        "AR = the full amount"
    );
    assert_eq!(
        bal(&raw, &s, s.revenue).await,
        Some(1000),
        "the whole amount recognizes now"
    );
    assert_eq!(
        bal(&raw, &s, s.contract_liability).await,
        None,
        "no Contract-liability balance (no deferral)"
    );
    assert_eq!(
        schedule_count(&raw, &s, "INV-PLAIN").await,
        0,
        "no schedule materialized for a non-deferred invoice"
    );
    // The posted entry carries NO Contract-liability line.
    let cl_lines = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.ledger_journal_line \
             WHERE tenant_id='{}' AND account_class='CONTRACT_LIABILITY' \
               AND invoice_id='INV-PLAIN'",
            s.tenant
        ),
    )
    .await;
    assert_eq!(cl_lines, 0, "no CR Contract-liability line emitted");
}

/// `RecognitionRepo::list_schedules` (the discovery surface backing
/// `GET /recognition-schedules` + the invoice-post `schedule_id` echo): the
/// optional `invoice_id` / `revenue_stream` filters narrow the result, and the
/// `AccessScope` binds it to the tenant (SQL-level BOLA — a foreign scope reads
/// nothing).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn list_schedules_filters_by_invoice_and_stream_and_is_tenant_scoped() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (_raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let service = svc(&provider, &harness, RecognitionConfig::default());
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    // Seed a SECOND revenue stream ("support") so the filters must DISCRIMINATE
    // between two schedules — not merely return the only row present.
    let reference = ReferenceRepo::new(provider.clone());
    for row in [
        account(
            s.tenant,
            Uuid::now_v7(),
            AccountClass::Revenue,
            Side::Credit,
            Some("support"),
        ),
        account(
            s.tenant,
            Uuid::now_v7(),
            AccountClass::ContractLiability,
            Side::Credit,
            Some("support"),
        ),
    ] {
        reference.insert_account(row).await.unwrap();
    }

    // Schedule A: INV-DEF / subscription / item-1 (1200 ex-tax, fully deferred).
    let inv_a = invoice(&s, "INV-DEF", vec![recognized_item(1200, 12, "item-1")]);
    service
        .post_invoice(&ctx, &scope, &inv_a, true)
        .await
        .expect("post A (subscription) must succeed");
    // Schedule B: INV-OTHER / support / item-2 (800 ex-tax, fully deferred).
    let support_item = InvoiceItem {
        amount_minor_ex_tax: 800,
        deferred_minor: 0,
        currency: "USD".to_owned(),
        revenue_stream: "support".to_owned(),
        catalog_class: Some(AccountClass::Revenue),
        contract_class: None,
        gl_code: Some("4001".to_owned()),
        recognition: Some(RecognitionInput {
            policy_ref: "policy.sl.v1".to_owned(),
            timing: RecognitionTiming::StraightLine {
                periods: 12,
                first_period_id: None,
            },
            po_allocation_group: Some("grp-2".to_owned()),
            multi_po: false,
            ssp_snapshot_ref: None,
            subscription_ref: Some("sub-2".to_owned()),
            vc_estimate_ref: None,
            vc_method_ref: None,
            immaterial_one_shot_sku: false,
        }),
        invoice_item_ref: Some("item-2".to_owned()),
        sku_or_plan_ref: None,
        price_id: None,
        pricing_snapshot_ref: None,
    };
    let inv_b = invoice(&s, "INV-OTHER", vec![support_item]);
    service
        .post_invoice(&ctx, &scope, &inv_b, true)
        .await
        .expect("post B (support) must succeed");

    let repo = bss_ledger::infra::storage::repo::RecognitionRepo::new(provider.clone());

    // Tenant-only: BOTH schedules, untruncated.
    let (all, truncated) = repo
        .list_schedules(&scope, s.tenant, None, None)
        .await
        .expect("list (tenant)");
    assert_eq!(all.len(), 2, "both schedules for the tenant");
    assert!(!truncated, "two rows are well under the cap");
    let sched_a = all
        .iter()
        .find(|r| r.source_invoice_id == "INV-DEF")
        .expect("schedule A present");
    assert_eq!(sched_a.revenue_stream, "subscription");
    assert_eq!(sched_a.source_invoice_item_ref, "item-1");
    assert_eq!(sched_a.status, "ACTIVE");
    assert_eq!(sched_a.total_deferred_minor, 1200);
    let id_a = sched_a.schedule_id.clone();
    let id_b = all
        .iter()
        .find(|r| r.source_invoice_id == "INV-OTHER")
        .expect("schedule B present")
        .schedule_id
        .clone();
    assert_ne!(id_a, id_b, "distinct schedules");

    // `invoice_id` DISCRIMINATES — selects the right ONE of two (not "the only row").
    let (by_a, _) = repo
        .list_schedules(&scope, s.tenant, Some("INV-DEF"), None)
        .await
        .expect("list (INV-DEF)");
    assert_eq!(
        by_a.iter()
            .map(|r| r.schedule_id.clone())
            .collect::<Vec<_>>(),
        vec![id_a.clone()],
    );
    let (by_b, _) = repo
        .list_schedules(&scope, s.tenant, Some("INV-OTHER"), None)
        .await
        .expect("list (INV-OTHER)");
    assert_eq!(
        by_b.iter()
            .map(|r| r.schedule_id.clone())
            .collect::<Vec<_>>(),
        vec![id_b.clone()],
    );
    let (none_inv, _) = repo
        .list_schedules(&scope, s.tenant, Some("INV-NOPE"), None)
        .await
        .expect("list (unknown invoice)");
    assert!(none_inv.is_empty(), "no schedule for an unknown invoice");

    // `revenue_stream` DISCRIMINATES.
    let (by_subscription, _) = repo
        .list_schedules(&scope, s.tenant, None, Some("subscription"))
        .await
        .expect("list (subscription)");
    assert_eq!(
        by_subscription
            .iter()
            .map(|r| r.schedule_id.clone())
            .collect::<Vec<_>>(),
        vec![id_a.clone()],
    );
    let (by_support, _) = repo
        .list_schedules(&scope, s.tenant, None, Some("support"))
        .await
        .expect("list (support)");
    assert_eq!(
        by_support
            .iter()
            .map(|r| r.schedule_id.clone())
            .collect::<Vec<_>>(),
        vec![id_b.clone()],
    );
    let (none_stream, _) = repo
        .list_schedules(&scope, s.tenant, None, Some("usage"))
        .await
        .expect("list (unbooked stream)");
    assert!(none_stream.is_empty(), "no schedule for an unbooked stream");

    // Combined filter = intersection (A); a cross filter (A's invoice + B's stream)
    // matches nothing.
    let (both, _) = repo
        .list_schedules(&scope, s.tenant, Some("INV-DEF"), Some("subscription"))
        .await
        .expect("list (both filters)");
    assert_eq!(
        both.iter()
            .map(|r| r.schedule_id.clone())
            .collect::<Vec<_>>(),
        vec![id_a.clone()],
    );
    let (cross, _) = repo
        .list_schedules(&scope, s.tenant, Some("INV-DEF"), Some("support"))
        .await
        .expect("list (cross filters)");
    assert!(
        cross.is_empty(),
        "invoice A + stream B is an empty intersection"
    );

    // SQL-level BOLA: a foreign tenant's scope reads none of `s.tenant`'s schedules.
    let foreign_scope = AccessScope::for_tenant(Uuid::now_v7());
    let (foreign, _) = repo
        .list_schedules(&foreign_scope, s.tenant, None, None)
        .await
        .expect("list (foreign scope)");
    assert!(
        foreign.is_empty(),
        "a foreign-tenant scope yields no rows (SQL-level BOLA)"
    );
}
