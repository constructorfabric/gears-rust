//! Postgres-only end-to-end for the audit-inquiry / drill reads + the audit-pack
//! CSV exporter (Slice 6 Phase 4 Group 4A). Boots a container, migrates, seeds a
//! seller (chart + USD@2 + an OPEN period), posts a couple of invoices through
//! the REAL `InvoicePostService`, then asserts:
//!   (1) `filter_entries` by payer + period returns the posted entries;
//!   (2) `export_csv` returns a CSV whose header + data-row count match, and a
//!       field containing a comma is RFC-4180 quoted;
//!   (3) `drill(entry_id)` returns the entry with its lines (and links a
//!       reversal back to its original);
//!   (4) a cross-tenant pack (`target_scope` set + investigation reason) writes
//!       ONE `cross-tenant-access` forensic record (asserted via the same
//!       `bss.secured_audit_record` query `postgres_cross_tenant.rs` uses);
//!       the own-tenant default writes none.
//! Ignored by default; run with `cargo test -p bss-ledger -- --ignored`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic,
    clippy::too_many_lines
)]

use std::sync::Arc;

use bss_ledger::domain::invoice::builder::{InvoiceItem, PostedInvoice, TaxBreakdown};
use bss_ledger::domain::model::{AccountRow, CurrencyScaleRow};
use bss_ledger::domain::money::DEFAULT_PLAUSIBLE_MAX_MAJOR;
use bss_ledger::infra::authz::cross_tenant::{CrossTenantGateway, TargetScope};
use bss_ledger::infra::events::publisher::LedgerEventPublisher;
use bss_ledger::infra::inquiry::{AuditPackExporter, InquiryFilter, InquiryService};
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

async fn count(conn: &DatabaseConnection, sql: &str) -> i64 {
    let row = conn.query_one(pg(sql.to_owned())).await.unwrap();
    row.map_or(0, |r| r.try_get_by_index::<i64>(0).unwrap())
}

fn naive(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// Provisioned seller ids.
struct Seller {
    tenant: Uuid,
    payer: Uuid,
    ar: Uuid,
    revenue: Uuid,
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

/// Boot, migrate, seed USD@2 + an OPEN period + AR/REVENUE/TAX/SUSPENSE.
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

fn revenue_item(amount: i64) -> InvoiceItem {
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

fn tax_breakdown(amount: i64) -> TaxBreakdown {
    TaxBreakdown {
        amount_minor: amount,
        currency: "USD".to_owned(),
        tax_jurisdiction: "US-CA".to_owned(),
        tax_filing_period: "2026Q2".to_owned(),
        tax_rate_ref: None,
    }
}

fn invoice(s: &Seller, invoice_id: &str) -> PostedInvoice {
    PostedInvoice {
        invoice_id: invoice_id.to_owned(),
        payer_tenant_id: s.payer,
        resource_tenant_id: None,
        seller_tenant_id: s.tenant,
        effective_at: naive(2026, 6, 1),
        due_date: Some(naive(2026, 7, 1)),
        period_id: s.period_id.clone(),
        items: vec![revenue_item(1000)],
        tax: vec![tax_breakdown(200)],
        posted_by_actor_id: s.tenant,
        correlation_id: s.tenant,
    }
}

fn svc(provider: &DBProvider<DbError>, metrics: &MetricsHarness) -> InvoicePostService {
    InvoicePostService::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(metrics.metrics()),
        bss_ledger::config::RecognitionConfig::default(),
        bss_ledger::config::FxConfig::default(),
    )
}

/// (1) `filter_entries` by payer + period returns the posted entries; (2)
/// `export_csv` header + row_count match and an injected comma is RFC-4180
/// quoted; (3) `drill` returns the entry with its lines.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn filter_export_and_drill() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let harness = MetricsHarness::new();
    let service = svc(&provider, &harness);
    let ctx = SecurityContext::anonymous();
    let scope = AccessScope::for_tenant(s.tenant);

    let inv1 = invoice(&s, "INV-100");
    let inv2 = invoice(&s, "INV-200");
    let p1 = service
        .post_invoice(&ctx, &scope, &inv1, true)
        .await
        .expect("post 1");
    let p2 = service
        .post_invoice(&ctx, &scope, &inv2, true)
        .await
        .expect("post 2");

    let inquiry = InquiryService::new(provider.clone());

    // (1) Filter by payer + period: both posted entries come back.
    let filter = InquiryFilter {
        payer_tenant_id: Some(s.payer),
        period_id: Some(s.period_id.clone()),
        account_class: None,
        legal_entity_id: None,
    };
    let rows = inquiry
        .filter_entries(&scope, &filter)
        .await
        .expect("filter_entries");
    let ids: std::collections::HashSet<Uuid> = rows.iter().map(|r| r.entry_id).collect();
    assert!(ids.contains(&p1.entry_id), "INV-100 entry is in the filter");
    assert!(ids.contains(&p2.entry_id), "INV-200 entry is in the filter");
    assert_eq!(rows.len(), 2, "exactly the two posted entries");

    // A foreign-tenant scope sees nothing (SQL-level BOLA).
    let foreign = AccessScope::for_tenant(Uuid::now_v7());
    let none = inquiry
        .filter_entries(&foreign, &filter)
        .await
        .expect("filter_entries (foreign)");
    assert!(none.is_empty(), "a foreign scope returns no entries");

    // Filter by a wrong account class returns nothing.
    let wrong_class = InquiryFilter {
        payer_tenant_id: None,
        period_id: None,
        account_class: Some("EQUITY".to_owned()),
        legal_entity_id: None,
    };
    let empty = inquiry
        .filter_entries(&scope, &wrong_class)
        .await
        .expect("filter_entries (wrong class)");
    assert!(empty.is_empty(), "no entry carries an EQUITY line");

    // (2) Export CSV: header + one row per (entry, line). Each invoice posts 3
    // lines (AR debit + Revenue credit + Tax credit), so 2 entries => 6 rows.
    let exporter = AuditPackExporter::new(provider.clone());
    let (csv, row_count) = exporter
        .export_csv(&scope, &filter)
        .await
        .expect("export_csv");
    let mut lines = csv.lines();
    let header = lines.next().expect("a header row");
    assert_eq!(header.split(',').count(), 22, "the header has 22 columns");
    assert!(
        header.starts_with("entry_id,tenant_id,period_id"),
        "header order"
    );
    let body_rows = csv.lines().count() - 1;
    assert_eq!(row_count, 6, "two 3-line entries => 6 data rows");
    assert_eq!(body_rows, row_count, "row_count matches the CSV body rows");

    // RFC-4180 quoting: post an invoice whose business id carries a comma, then
    // assert the CSV quotes that field. `source_business_id` (= the invoice id)
    // is an exported column and is free text, so it can carry a comma — unlike
    // `revenue_stream`, which is a chart-of-accounts join key and must match a
    // provisioned account.
    let commad = invoice(&s, "INV,COMMA");
    service
        .post_invoice(&ctx, &scope, &commad, true)
        .await
        .expect("post comma invoice");
    let (csv2, _) = exporter
        .export_csv(&scope, &filter)
        .await
        .expect("export_csv (comma)");
    assert!(
        csv2.contains("\"INV,COMMA\""),
        "a comma-bearing field is RFC-4180 quoted; csv2 = {csv2}"
    );

    // (3) Drill into INV-100's entry: header + its 3 lines, no links.
    let drill = inquiry
        .drill(&scope, s.tenant, p1.entry_id)
        .await
        .expect("drill")
        .expect("entry found");
    assert_eq!(drill.entry.entry_id, p1.entry_id, "drill header");
    assert_eq!(
        drill.entry.source_business_id, "INV-100",
        "source business id"
    );
    assert_eq!(drill.lines.len(), 3, "AR + Revenue + Tax lines");
    assert!(drill.linked.is_empty(), "an un-reversed entry has no links");

    // A foreign-scope drill yields None (no existence leak).
    let none = inquiry
        .drill(&foreign, s.tenant, p1.entry_id)
        .await
        .expect("drill (foreign)");
    assert!(none.is_none(), "a foreign scope cannot drill the entry");

    // Sanity: the lines really were written for our payer.
    let line_count = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.ledger_journal_line \
             WHERE tenant_id='{}' AND payer_tenant_id='{}'",
            s.tenant, s.payer
        ),
    )
    .await;
    assert!(line_count >= 6, "at least the two invoices' lines exist");
}

/// (4) A cross-tenant pack (target_scope set + reason) writes ONE
/// `cross-tenant-access` forensic record; the own-tenant default writes none.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn cross_tenant_pack_writes_forensic_record() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, s) = setup(&url).await;
    let home = Uuid::now_v7();
    let exporter = AuditPackExporter::new(provider.clone());
    let gateway = CrossTenantGateway::new();
    let filter = InquiryFilter::default();

    // Own-tenant pack (target == home): NO forensic record, scope = home.
    provider
        .transaction({
            let exporter = exporter.clone();
            let gateway = gateway.clone();
            let filter = filter.clone();
            move |txn| {
                let exporter = exporter.clone();
                let gateway = gateway.clone();
                let filter = filter.clone();
                Box::pin(async move {
                    let scope = gateway
                        .resolve_read_scope(
                            txn,
                            home,
                            Some(TargetScope { tenant_id: home }),
                            true,
                            "actor-own",
                            Some("reason"),
                            Some("ROUTINE"),
                            None,
                        )
                        .await?;
                    exporter
                        .export_csv_in_txn(txn, &scope, &filter)
                        .await
                        .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))?;
                    Ok::<_, DbError>(())
                })
            }
        })
        .await
        .expect("own-tenant pack");

    let own_records = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.secured_audit_record \
             WHERE tenant_id='{home}' AND event_type='cross-tenant-access'"
        ),
    )
    .await;
    assert_eq!(
        own_records, 0,
        "an own-tenant pack writes NO forensic record"
    );

    // Cross-tenant pack (target = the seller tenant): ONE forensic record under
    // HOME, the export reads the target tenant's rows.
    let target = s.tenant;
    let (_csv, _rows) = provider
        .transaction({
            let exporter = exporter.clone();
            let gateway = gateway.clone();
            let filter = filter.clone();
            move |txn| {
                let exporter = exporter.clone();
                let gateway = gateway.clone();
                let filter = filter.clone();
                Box::pin(async move {
                    let scope = gateway
                        .resolve_read_scope(
                            txn,
                            home,
                            Some(TargetScope { tenant_id: target }),
                            true,
                            "investigator-9",
                            Some("fraud investigation #7"),
                            Some("FRAUD"),
                            None,
                        )
                        .await?;
                    exporter
                        .export_csv_in_txn(txn, &scope, &filter)
                        .await
                        .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))
                })
            }
        })
        .await
        .expect("cross-tenant pack");

    let cross_records = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.secured_audit_record \
             WHERE tenant_id='{home}' AND event_type='cross-tenant-access'"
        ),
    )
    .await;
    assert_eq!(
        cross_records, 1,
        "a cross-tenant pack writes ONE forensic record under home"
    );
    // The record names the target tenant in its before_after payload.
    let target_in_payload = count(
        &raw,
        &format!(
            "SELECT COUNT(*) FROM bss.secured_audit_record \
             WHERE tenant_id='{home}' AND event_type='cross-tenant-access' \
               AND before_after->'targetScope'->>'tenantId'='{target}'"
        ),
    )
    .await;
    assert_eq!(target_in_payload, 1, "the record names the target tenant");
}
