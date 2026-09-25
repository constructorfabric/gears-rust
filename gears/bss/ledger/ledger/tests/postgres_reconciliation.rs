//! Postgres-only integration tests for the Slice 7 Phase 3 reconciliation framework and
//! control-feed close gate (design §4.3 / §4.5). K1: an AR↔derived variance opens a
//! `RECON_MISMATCH` and blocks close. K2: an invoice-completeness gap opens a
//! `MISSED_POSTING` that blocks close, which the upstream's idempotent re-post then
//! auto-clears. K3: a Payments↔PSP divergence opens a `PSP_VARIANCE`. K4: the
//! bill-run-finished close gate (un-asserted blocks, asserted passes). K5: inert until
//! the feed lands. K8: the tick's tenant-registry lifecycle gate — a tenant that has
//! left the registry is not reconciled and its uneventful runs are reclaimed, while its
//! variance evidence is kept. Control feeds are exercised through the real in-process
//! store (the push → framework / close-gate read path). Ignored by default; run with
//! `cargo test -p cf-gears-bss-ledger --test postgres_reconciliation -- --ignored`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown,
    clippy::panic
)]

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use bss_ledger::config::ReconConfig;
use bss_ledger::domain::error::DomainError;
use bss_ledger::domain::ports::metrics::{LedgerMetricsPort, NoopLedgerMetrics};
use bss_ledger::infra::control_feed::InProcessControlFeeds;
use bss_ledger::infra::events::publisher::LedgerEventPublisher;
use bss_ledger::infra::exception::ExceptionRouter;
use bss_ledger::infra::period_close::{CloseControlFeeds, PeriodCloseService};
use bss_ledger::infra::reconciliation::{
    CHECK_AR_DERIVED, CHECK_INVOICE_COMPLETENESS, CHECK_PAYMENTS_PSP, ReconciliationFramework,
};
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::tenant_lifecycle::LiveTenantFilter;
use bss_ledger_sdk::{
    BillRunFinishedV1, IssuedInvoiceManifest, IssuedInvoiceManifestV1, PspSettlementFeedV1,
    PspSettlementReport, UnconfiguredBillRunFinishedV1, UnconfiguredIssuedInvoiceManifestV1,
    UnconfiguredPspSettlementFeedV1,
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

const PERIOD: &str = "202606";
/// A second fiscal period, for the period-scoped PSP recon test (C2).
const PERIOD2: &str = "202607";

fn pg(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

/// Boot + migrate; seed ONE OPEN fiscal period for `tenant` (LE = tenant). Returns the
/// raw conn, the provider, and the tenant id.
async fn setup(url: &str) -> (DatabaseConnection, DBProvider<DbError>, Uuid) {
    let raw = Database::connect(url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();

    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    let provider = DBProvider::<DbError>::new(tdb);

    let tenant = Uuid::now_v7();
    raw.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_fiscal_period (tenant_id, legal_entity_id, period_id, fiscal_tz, status) \
         VALUES ('{tenant}','{tenant}','{PERIOD}','UTC','OPEN')"
    )))
    .await
    .unwrap();
    (raw, provider, tenant)
}

/// Seed one AR chart-of-accounts row (normal side DR) and return its `account_id`.
/// The posted-invoice lines net to zero on this account so the books stay tie-out-clean
/// (the completeness test must isolate `MISSED_POSTING` from the AR↔derived tie-out).
async fn seed_ar_account(raw: &DatabaseConnection, tenant: Uuid) -> Uuid {
    let account = Uuid::now_v7();
    raw.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_tenant_account \
            (account_id, tenant_id, legal_entity_id, account_class, currency, normal_side, \
             may_go_negative, lifecycle_state) \
         VALUES ('{account}','{tenant}','{tenant}','AR','USD','DR', false, 'OPEN')"
    )))
    .await
    .unwrap();
    account
}

/// Seed a posted `INVOICE_POST` journal entry for `invoice_id` (the completeness check
/// reads the header's `source_business_id`). The entry carries two net-zero AR lines
/// (`DR 1000 / CR 1000` on the same `account`) so it is balanced + non-empty (the
/// `check_entry_balanced` trigger) yet tie-out-neutral (computed 0 = cached 0). The
/// `rounding_evidence`/`created_seq` columns default.
async fn seed_invoice_post(
    raw: &DatabaseConnection,
    tenant: Uuid,
    account: Uuid,
    invoice_id: &str,
) {
    use sea_orm::TransactionTrait;
    let entry_id = Uuid::now_v7();
    // The header + both lines MUST commit in ONE transaction: `trg_journal_entry_balanced`
    // is `DEFERRABLE INITIALLY DEFERRED`, so it checks balanced + non-empty only at COMMIT.
    // Inserting the header on its own connection autocommit would commit an empty entry and
    // trip `LEDGER_ENTRY_EMPTY`.
    let txn = raw.begin().await.unwrap();
    txn.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_journal_entry \
            (entry_id, tenant_id, legal_entity_id, period_id, entry_currency, \
             source_doc_type, source_business_id, posted_at_utc, effective_at, \
             origin, posted_by_actor_id, correlation_id) \
         VALUES ('{entry_id}','{tenant}','{tenant}','{PERIOD}','USD', \
             'INVOICE_POST','{invoice_id}', now(), DATE '2026-06-01', \
             'SYSTEM','{tenant}','{tenant}')"
    )))
    .await
    .unwrap();
    for side in ["DR", "CR"] {
        let line_id = Uuid::now_v7();
        txn.execute_raw(pg(format!(
            "INSERT INTO bss.ledger_journal_line \
                (line_id, entry_id, tenant_id, period_id, payer_tenant_id, account_id, \
                 account_class, side, amount_minor, currency, currency_scale, mapping_status) \
             VALUES ('{line_id}','{entry_id}','{tenant}','{PERIOD}','{tenant}','{account}', \
                 'AR','{side}',1000,'USD',2,'RESOLVED')"
        )))
        .await
        .unwrap();
    }
    txn.commit().await.unwrap();
}

/// Seed a posted `PAYMENT_SETTLE` entry in `period` with no fee, so it nets
/// `DR CASH_CLEARING gross / CR UNALLOCATED gross` (balanced, non-empty). The
/// period-scoped PSP recon (C2) sums the `CR UNALLOCATED` leg of these entries
/// for the period; settling into two periods exercises the period boundary.
async fn seed_payment_settle(
    raw: &DatabaseConnection,
    tenant: Uuid,
    period: &str,
    payment_id: &str,
    gross: i64,
) {
    use sea_orm::TransactionTrait;
    let entry_id = Uuid::now_v7();
    let txn = raw.begin().await.unwrap();
    txn.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_journal_entry \
            (entry_id, tenant_id, legal_entity_id, period_id, entry_currency, \
             source_doc_type, source_business_id, posted_at_utc, effective_at, \
             origin, posted_by_actor_id, correlation_id) \
         VALUES ('{entry_id}','{tenant}','{tenant}','{period}','USD', \
             'PAYMENT_SETTLE','{payment_id}', now(), DATE '2026-06-01', \
             'SYSTEM','{tenant}','{tenant}')"
    )))
    .await
    .unwrap();
    for (class, side) in [("CASH_CLEARING", "DR"), ("UNALLOCATED", "CR")] {
        let line_id = Uuid::now_v7();
        let nil = Uuid::nil();
        txn.execute_raw(pg(format!(
            "INSERT INTO bss.ledger_journal_line \
                (line_id, entry_id, tenant_id, period_id, payer_tenant_id, account_id, \
                 account_class, side, amount_minor, currency, currency_scale, mapping_status) \
             VALUES ('{line_id}','{entry_id}','{tenant}','{period}','{tenant}','{nil}', \
                 '{class}','{side}',{gross},'USD',2,'RESOLVED')"
        )))
        .await
        .unwrap();
    }
    txn.commit().await.unwrap();
}

/// Count exception rows for `(tenant, type)` in a given status.
async fn count_exceptions(
    raw: &DatabaseConnection,
    tenant: Uuid,
    exception_type: &str,
    status: &str,
) -> i64 {
    let row = raw
        .query_one_raw(pg(format!(
            "SELECT count(*) AS c FROM bss.ledger_exception_queue \
             WHERE tenant_id='{tenant}' AND exception_type='{exception_type}' AND status='{status}'"
        )))
        .await
        .unwrap()
        .expect("count row");
    row.try_get::<i64>("", "c").unwrap()
}

/// Count `reconciliation_run` rows for `(tenant, check_type)`.
async fn count_runs(raw: &DatabaseConnection, tenant: Uuid, check_type: &str) -> i64 {
    let row = raw
        .query_one_raw(pg(format!(
            "SELECT count(*) AS c FROM bss.ledger_reconciliation_run \
             WHERE tenant_id='{tenant}' AND check_type='{check_type}'"
        )))
        .await
        .unwrap()
        .expect("count row");
    row.try_get::<i64>("", "c").unwrap()
}

/// The latest `within_tolerance` flag for `(tenant, check_type)`.
async fn latest_within_tolerance(raw: &DatabaseConnection, tenant: Uuid, check_type: &str) -> bool {
    raw.query_one_raw(pg(format!(
        "SELECT within_tolerance FROM bss.ledger_reconciliation_run \
         WHERE tenant_id='{tenant}' AND check_type='{check_type}' ORDER BY at_utc DESC LIMIT 1"
    )))
    .await
    .unwrap()
    .expect("a run row")
    .try_get::<bool>("", "within_tolerance")
    .unwrap()
}

fn noop_metrics() -> Arc<dyn LedgerMetricsPort> {
    Arc::new(NoopLedgerMetrics)
}

/// Tenant-registry double reporting every candidate live. The `run_check`-driven
/// tests below never exercise the tick's lifecycle gate, so this keeps them on
/// the pre-gate behaviour.
struct AllTenantsLive;

#[async_trait]
impl LiveTenantFilter for AllTenantsLive {
    async fn live_tenants(&self, candidates: &[Uuid]) -> anyhow::Result<HashSet<Uuid>> {
        Ok(candidates.iter().copied().collect())
    }
}

/// Tenant-registry double that knows exactly one set of live tenants; every
/// other candidate is reported retired (soft-deleted or absent — the registry
/// answers both by omission).
struct FixedLiveTenants(HashSet<Uuid>);

#[async_trait]
impl LiveTenantFilter for FixedLiveTenants {
    async fn live_tenants(&self, candidates: &[Uuid]) -> anyhow::Result<HashSet<Uuid>> {
        Ok(candidates
            .iter()
            .copied()
            .filter(|t| self.0.contains(t))
            .collect())
    }
}

/// Build a framework over the in-process control-feed store (the push → read path).
#[allow(
    clippy::needless_pass_by_value,
    reason = "test helper — clones the provider for the engine and moves it into the exception router"
)]
fn framework(
    provider: DBProvider<DbError>,
    feeds: Arc<InProcessControlFeeds>,
    config: ReconConfig,
) -> ReconciliationFramework {
    framework_with_live(provider, feeds, config, Arc::new(AllTenantsLive))
}

/// As [`framework`], with an explicit tenant-registry lifecycle gate.
#[allow(
    clippy::needless_pass_by_value,
    reason = "test helper — clones the provider for the engine and moves it into the exception router"
)]
fn framework_with_live(
    provider: DBProvider<DbError>,
    feeds: Arc<InProcessControlFeeds>,
    config: ReconConfig,
    live_tenants: Arc<dyn LiveTenantFilter>,
) -> ReconciliationFramework {
    ReconciliationFramework::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        noop_metrics(),
        ExceptionRouter::shared(provider),
        Arc::clone(&feeds) as Arc<dyn IssuedInvoiceManifestV1>,
        Arc::clone(&feeds) as Arc<dyn PspSettlementFeedV1>,
        live_tenants,
        config,
    )
}

fn close_service(provider: DBProvider<DbError>, control: CloseControlFeeds) -> PeriodCloseService {
    PeriodCloseService::new(
        provider,
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(bss_ledger::infra::audit::secured_audit_sink::NoopSecuredAuditSink::new()),
    )
    .with_control_feeds(control)
}

async fn boot(url: &str) -> (DatabaseConnection, DBProvider<DbError>, Uuid) {
    setup(url).await
}

/// K1 — an AR↔derived tie-out variance (a stray balance-cache grain) opens a
/// `RECON_MISMATCH` (out of tolerance) and blocks period close.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn k1_ar_variance_opens_recon_mismatch_and_blocks_close() {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, tenant) = boot(&url).await;

    // A stray `account_balance` cache row with no journal → the recompute disagrees
    // with the cache (computed 0 ≠ cached 50_000): an out-of-tolerance tie-out variance.
    let account = Uuid::now_v7();
    raw.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_account_balance \
            (tenant_id, account_id, currency, account_class, normal_side, balance_minor, version) \
         VALUES ('{tenant}','{account}','USD','AR','DR', 50000, 1)"
    )))
    .await
    .unwrap();

    let feeds = Arc::new(InProcessControlFeeds::new());
    let fw = framework(provider.clone(), feeds, ReconConfig::default());
    fw.run_check(
        &SecurityContext::anonymous(),
        tenant,
        PERIOD,
        CHECK_AR_DERIVED,
    )
    .await
    .expect("AR_DERIVED check runs");

    assert_eq!(
        count_runs(&raw, tenant, CHECK_AR_DERIVED).await,
        1,
        "one AR run written"
    );
    assert!(
        !latest_within_tolerance(&raw, tenant, CHECK_AR_DERIVED).await,
        "the 50_000 variance is out of tolerance"
    );
    assert_eq!(
        count_exceptions(&raw, tenant, "RECON_MISMATCH", "OPEN").await,
        1,
        "an out-of-tolerance AR run opens exactly one RECON_MISMATCH"
    );

    // Close is blocked (both by the tie-out variance directly and by the OPEN exception).
    let close = close_service(provider, CloseControlFeeds::inert());
    let err = close
        .close(
            &SecurityContext::anonymous(),
            tenant,
            tenant,
            PERIOD.to_owned(),
        )
        .await
        .expect_err("an AR variance blocks close");
    assert!(
        matches!(err, DomainError::PeriodCloseBlocked(_)),
        "got {err:?}"
    );
}

/// K2 — an issued invoice with no committed `INVOICE_POST` opens a `MISSED_POSTING` and
/// blocks close; the upstream's idempotent re-post auto-clears it and the period closes.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn k2_missed_posting_blocks_then_idempotent_repost_clears() {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, tenant) = boot(&url).await;

    // Manifest issued {inv-1, inv-2}; only inv-1 is posted ⇒ inv-2 is a missed posting.
    let account = seed_ar_account(&raw, tenant).await;
    seed_invoice_post(&raw, tenant, account, "inv-1").await;
    let feeds = Arc::new(InProcessControlFeeds::new());
    feeds.ingest_manifest(
        tenant,
        PERIOD,
        IssuedInvoiceManifest {
            invoice_ids: vec!["inv-1".to_owned(), "inv-2".to_owned()],
            count: 2,
            gross_total_minor: 2000,
        },
    );
    let config = ReconConfig {
        manifest_enforcement: true,
        ..ReconConfig::default()
    };
    let fw = framework(provider.clone(), Arc::clone(&feeds), config);

    fw.run_check(
        &SecurityContext::anonymous(),
        tenant,
        PERIOD,
        CHECK_INVOICE_COMPLETENESS,
    )
    .await
    .expect("completeness check runs");
    assert_eq!(
        count_exceptions(&raw, tenant, "MISSED_POSTING", "OPEN").await,
        1,
        "the unposted inv-2 opens one MISSED_POSTING"
    );

    // Close is blocked (manifest enforcement on + the missing posting).
    let control = CloseControlFeeds {
        manifest_feed: Arc::clone(&feeds) as Arc<dyn IssuedInvoiceManifestV1>,
        bill_run_feed: Arc::new(UnconfiguredBillRunFinishedV1),
        manifest_enforcement: true,
        bill_run_enforcement: false,
        fx_revaluation_enforcement: false,
    };
    let close = close_service(provider.clone(), control);
    let err = close
        .close(
            &SecurityContext::anonymous(),
            tenant,
            tenant,
            PERIOD.to_owned(),
        )
        .await
        .expect_err("a missed posting blocks close");
    assert!(
        matches!(err, DomainError::PeriodCloseBlocked(_)),
        "got {err:?}"
    );

    // The upstream's idempotent re-post lands inv-2; the re-run auto-resolves the
    // MISSED_POSTING and the period now closes cleanly.
    seed_invoice_post(&raw, tenant, account, "inv-2").await;
    fw.run_check(
        &SecurityContext::anonymous(),
        tenant,
        PERIOD,
        CHECK_INVOICE_COMPLETENESS,
    )
    .await
    .expect("completeness re-run");
    assert_eq!(
        count_exceptions(&raw, tenant, "MISSED_POSTING", "OPEN").await,
        0,
        "the re-post auto-resolves the MISSED_POSTING"
    );
    assert_eq!(
        count_exceptions(&raw, tenant, "MISSED_POSTING", "RESOLVED").await,
        1,
        "the cleared row is RESOLVED, not deleted"
    );
    let outcome = close
        .close(
            &SecurityContext::anonymous(),
            tenant,
            tenant,
            PERIOD.to_owned(),
        )
        .await
        .expect("a complete period closes");
    assert!(
        !outcome.already_closed,
        "the period closes on the re-attempt"
    );
}

/// K3 — a Payments↔PSP settlement divergence (the PSP reports settled the ledger has not
/// recorded) opens a `PSP_VARIANCE`.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn k3_psp_variance_opens_exception() {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, tenant) = boot(&url).await;

    // No ledger settlements (settled 0) but the PSP reports 100 settled ⇒ variance.
    let feeds = Arc::new(InProcessControlFeeds::new());
    feeds.ingest_psp_report(
        tenant,
        PERIOD,
        PspSettlementReport {
            report_id: "rpt-1".to_owned(),
            settled_minor: 100,
            currency: "USD".to_owned(),
        },
    );
    let fw = framework(provider, Arc::clone(&feeds), ReconConfig::default());
    fw.run_check(
        &SecurityContext::anonymous(),
        tenant,
        PERIOD,
        CHECK_PAYMENTS_PSP,
    )
    .await
    .expect("PSP check runs");

    assert_eq!(
        count_runs(&raw, tenant, CHECK_PAYMENTS_PSP).await,
        1,
        "one PSP run written"
    );
    assert!(
        !latest_within_tolerance(&raw, tenant, CHECK_PAYMENTS_PSP).await,
        "ledger 0 vs PSP 100 is out of tolerance"
    );
    assert_eq!(
        count_exceptions(&raw, tenant, "PSP_VARIANCE", "OPEN").await,
        1,
        "a PSP divergence opens one PSP_VARIANCE"
    );
}

/// K6 (C2) — the Payments↔PSP ledger total is PERIOD-SCOPED: a settle in a PRIOR
/// period must not inflate the current period's recon. Settle 100 in period 1 and
/// 60 in period 2, push a PSP report of 60 for period 2, and assert period 2
/// reconciles within tolerance. The pre-C2 lifetime `payment_settlement.settled_minor`
/// sum compared 160 vs 60 and opened a spurious `PSP_VARIANCE`; the period-scoped
/// journal sum compares 60 vs 60.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn k6_psp_check_is_period_scoped() {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, tenant) = boot(&url).await;

    // A second OPEN fiscal period alongside the default one.
    raw.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_fiscal_period (tenant_id, legal_entity_id, period_id, fiscal_tz, status) \
         VALUES ('{tenant}','{tenant}','{PERIOD2}','UTC','OPEN')"
    )))
    .await
    .unwrap();

    // Settle 100 in period 1, 60 in period 2.
    seed_payment_settle(&raw, tenant, PERIOD, "PAY-P1", 100).await;
    seed_payment_settle(&raw, tenant, PERIOD2, "PAY-P2", 60).await;

    // The PSP reports 60 for period 2 — matching period 2's settlements only.
    let feeds = Arc::new(InProcessControlFeeds::new());
    feeds.ingest_psp_report(
        tenant,
        PERIOD2,
        PspSettlementReport {
            report_id: "rpt-p2".to_owned(),
            settled_minor: 60,
            currency: "USD".to_owned(),
        },
    );
    let fw = framework(provider, Arc::clone(&feeds), ReconConfig::default());
    fw.run_check(
        &SecurityContext::anonymous(),
        tenant,
        PERIOD2,
        CHECK_PAYMENTS_PSP,
    )
    .await
    .expect("PSP check runs");

    assert!(
        latest_within_tolerance(&raw, tenant, CHECK_PAYMENTS_PSP).await,
        "period-2 ledger settled (60) must match PSP 60 — period-scoped, not the 160 lifetime sum"
    );
    assert_eq!(
        count_exceptions(&raw, tenant, "PSP_VARIANCE", "OPEN").await,
        0,
        "no spurious PSP_VARIANCE once the recon is period-scoped"
    );
}

/// K7 (C3) — the Mode-B FX-revaluation completeness gate: with enforcement ON and
/// NO COMPLETE marker for the period, close BLOCKS; once the period-end run records
/// the marker, close proceeds. Clean empty books, so only the fx-reval gate is under
/// test.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn k7_fx_revaluation_gate_blocks_until_marker() {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, tenant) = boot(&url).await;

    let control = || CloseControlFeeds {
        manifest_feed: Arc::new(UnconfiguredIssuedInvoiceManifestV1),
        bill_run_feed: Arc::new(UnconfiguredBillRunFinishedV1),
        manifest_enforcement: false,
        bill_run_enforcement: false,
        fx_revaluation_enforcement: true,
    };

    // No COMPLETE marker for the period ⇒ close blocks.
    let err = close_service(provider.clone(), control())
        .close(
            &SecurityContext::anonymous(),
            tenant,
            tenant,
            PERIOD.to_owned(),
        )
        .await
        .expect_err("a missing FX-revaluation marker blocks close");
    assert!(
        matches!(err, DomainError::PeriodCloseBlocked(_)),
        "got {err:?}"
    );

    // The period-end revaluation records the COMPLETE marker ⇒ the gate passes.
    raw.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_fx_revaluation_run \
            (tenant_id, period_id, scope, status, completed_at_utc) \
         VALUES ('{tenant}','{PERIOD}','PERIOD','COMPLETE', now())"
    )))
    .await
    .unwrap();
    let outcome = close_service(provider, control())
        .close(
            &SecurityContext::anonymous(),
            tenant,
            tenant,
            PERIOD.to_owned(),
        )
        .await
        .expect("a COMPLETE FX-revaluation marker lets close proceed");
    assert!(
        !outcome.already_closed,
        "the period closes once the marker is present"
    );
}

/// K4 — the bill-run-finished close gate blocks until the signal is asserted (flag ON);
/// with `Some(true)` asserted the close passes.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn k4_bill_run_gate_blocks_until_asserted() {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, tenant) = boot(&url).await;

    // Empty books (clean tie-out). Bill-run enforcement ON, but nothing asserted ⇒ block.
    let feeds = Arc::new(InProcessControlFeeds::new());
    let control_absent = CloseControlFeeds {
        manifest_feed: Arc::new(UnconfiguredIssuedInvoiceManifestV1),
        bill_run_feed: Arc::clone(&feeds) as Arc<dyn BillRunFinishedV1>,
        manifest_enforcement: false,
        bill_run_enforcement: true,
        fx_revaluation_enforcement: false,
    };
    let close_absent = close_service(provider.clone(), control_absent);
    let err = close_absent
        .close(
            &SecurityContext::anonymous(),
            tenant,
            tenant,
            PERIOD.to_owned(),
        )
        .await
        .expect_err("an un-asserted bill run blocks close");
    assert!(
        matches!(err, DomainError::PeriodCloseBlocked(_)),
        "got {err:?}"
    );

    // Orchestration asserts the bill run finished ⇒ the gate passes and the period closes.
    feeds.ingest_bill_run_finished(tenant, PERIOD, true);
    let control_done = CloseControlFeeds {
        manifest_feed: Arc::new(UnconfiguredIssuedInvoiceManifestV1),
        bill_run_feed: Arc::clone(&feeds) as Arc<dyn BillRunFinishedV1>,
        manifest_enforcement: false,
        bill_run_enforcement: true,
        fx_revaluation_enforcement: false,
    };
    let close_done = close_service(provider, control_done);
    let outcome = close_done
        .close(
            &SecurityContext::anonymous(),
            tenant,
            tenant,
            PERIOD.to_owned(),
        )
        .await
        .expect("an asserted bill run lets close proceed");
    assert!(
        !outcome.already_closed,
        "the period closes once the bill run is asserted"
    );
    let status = raw
        .query_one_raw(pg(format!(
            "SELECT status FROM bss.ledger_fiscal_period \
             WHERE tenant_id='{tenant}' AND period_id='{PERIOD}'"
        )))
        .await
        .unwrap()
        .expect("period row")
        .try_get::<String>("", "status")
        .unwrap();
    assert_eq!(status, "CLOSED");
}

/// K5 — inert-until-the-feed-lands: with no manifest configured (the default
/// `Unconfigured…` / empty store) the invoice-completeness check is inert (no run, no
/// exception) and the close gate does not block on it (enforcement OFF, the default).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn k5_inert_until_feed_lands() {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, tenant) = boot(&url).await;

    // Empty control store (nothing pushed) + default config (enforcement OFF).
    let feeds = Arc::new(InProcessControlFeeds::new());
    let fw = framework(provider.clone(), Arc::clone(&feeds), ReconConfig::default());

    // The completeness check is inert: `run_check` rejects (nothing to reconcile), no
    // run row is written, and no MISSED_POSTING is opened.
    let err = fw
        .run_check(
            &SecurityContext::anonymous(),
            tenant,
            PERIOD,
            CHECK_INVOICE_COMPLETENESS,
        )
        .await
        .expect_err("an unconfigured manifest is inert");
    assert!(matches!(err, DomainError::InvalidRequest(_)), "got {err:?}");
    assert_eq!(
        count_runs(&raw, tenant, CHECK_INVOICE_COMPLETENESS).await,
        0,
        "no run written"
    );
    assert_eq!(
        count_exceptions(&raw, tenant, "MISSED_POSTING", "OPEN").await,
        0,
        "no exception"
    );

    // Close is NOT blocked by the (inert) completeness gate — enforcement OFF + clean books.
    let close = close_service(
        provider,
        CloseControlFeeds {
            manifest_feed: Arc::clone(&feeds) as Arc<dyn IssuedInvoiceManifestV1>,
            bill_run_feed: Arc::new(UnconfiguredBillRunFinishedV1),
            manifest_enforcement: false,
            bill_run_enforcement: false,
            fx_revaluation_enforcement: false,
        },
    );
    let outcome = close
        .close(
            &SecurityContext::anonymous(),
            tenant,
            tenant,
            PERIOD.to_owned(),
        )
        .await
        .expect("an inert completeness gate does not block close");
    assert!(!outcome.already_closed, "the period closes cleanly");
}

/// A configured PSP feed returning `None` (Unconfigured default) leaves the Payments↔PSP
/// check inert (`run_check` rejects, no run) — the same inert-until-live contract.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn k5b_unconfigured_psp_check_is_inert() {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, tenant) = boot(&url).await;

    // A framework whose PSP port is the fail-safe Unconfigured default (always None).
    let fw = ReconciliationFramework::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        noop_metrics(),
        ExceptionRouter::shared(provider),
        Arc::new(UnconfiguredIssuedInvoiceManifestV1),
        Arc::new(UnconfiguredPspSettlementFeedV1),
        Arc::new(AllTenantsLive),
        ReconConfig::default(),
    );
    let err = fw
        .run_check(
            &SecurityContext::anonymous(),
            tenant,
            PERIOD,
            CHECK_PAYMENTS_PSP,
        )
        .await
        .expect_err("an unconfigured PSP feed is inert");
    assert!(matches!(err, DomainError::InvalidRequest(_)), "got {err:?}");
    assert_eq!(
        count_runs(&raw, tenant, CHECK_PAYMENTS_PSP).await,
        0,
        "no run written"
    );
}

/// Seed one finalized `reconciliation_run` row directly, bypassing the framework —
/// the backlog a retired tenant accumulated before the lifecycle gate existed.
async fn seed_recon_run(
    raw: &DatabaseConnection,
    tenant: Uuid,
    status: &str,
    within_tolerance: bool,
) -> Uuid {
    let run_id = Uuid::now_v7();
    raw.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_reconciliation_run \
            (tenant_id, run_id, period_id, check_type, variance_minor, within_tolerance, status) \
         VALUES ('{tenant}','{run_id}','{PERIOD}','{CHECK_AR_DERIVED}',0,{within_tolerance},'{status}')"
    )))
    .await
    .unwrap();
    run_id
}

/// Does a specific run row still exist?
async fn run_exists(raw: &DatabaseConnection, tenant: Uuid, run_id: Uuid) -> bool {
    raw.query_one_raw(pg(format!(
        "SELECT 1 AS x FROM bss.ledger_reconciliation_run \
         WHERE tenant_id='{tenant}' AND run_id='{run_id}'"
    )))
    .await
    .unwrap()
    .is_some()
}

/// Seed a fully-formed reconcilable tenant: an OPEN fiscal period, an AR account,
/// and one posted `INVOICE_POST` entry (so the tenant shows up in the tick's
/// `journal_entry`-derived candidate enumeration).
async fn seed_reconcilable_tenant(raw: &DatabaseConnection, tenant: Uuid) {
    raw.execute_raw(pg(format!(
        "INSERT INTO bss.ledger_fiscal_period (tenant_id, legal_entity_id, period_id, fiscal_tz, status) \
         VALUES ('{tenant}','{tenant}','{PERIOD}','UTC','OPEN')"
    )))
    .await
    .unwrap();
    let account = seed_ar_account(raw, tenant).await;
    seed_invoice_post(raw, tenant, account, "inv-1").await;
}

/// K8 — the tick's tenant-registry lifecycle gate. Two tenants are
/// indistinguishable from ledger data alone: both have posted entries and an OPEN
/// period. Only one is still live in the platform registry. The tick must
/// reconcile the live one, write NOTHING for the retired one, and reclaim the
/// retired one's uneventful run backlog — while keeping its variance evidence and
/// any unfinalized row.
///
/// Without the gate the tick reconciles both forever: it enumerates from
/// append-only ledger data and appends one row per tenant per tick to a table with
/// no delete path (29 GB / 61M rows on stage1, 99.9% of it for tenants that no
/// longer exist).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn k8_retired_tenants_are_skipped_and_their_uneventful_runs_reclaimed() {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    // `setup` seeds the live tenant's OPEN period; give it ledger data too.
    let (raw, provider, live) = boot(&url).await;
    let live_account = seed_ar_account(&raw, live).await;
    seed_invoice_post(&raw, live, live_account, "inv-live").await;

    let retired = Uuid::now_v7();
    seed_reconcilable_tenant(&raw, retired).await;
    // The retired tenant's accumulated backlog: one uneventful finalized run
    // (reclaimable), one that recorded a breach (evidence), one still RUNNING.
    let uneventful = seed_recon_run(&raw, retired, "DONE", true).await;
    let breached = seed_recon_run(&raw, retired, "DONE", false).await;
    let unfinalized = seed_recon_run(&raw, retired, "RUNNING", true).await;

    let feeds = Arc::new(InProcessControlFeeds::new());
    let fw = framework_with_live(
        provider,
        feeds,
        ReconConfig::default(),
        Arc::new(FixedLiveTenants(HashSet::from([live]))),
    );

    fw.run().await.unwrap();

    assert_eq!(
        count_runs(&raw, live, CHECK_AR_DERIVED).await,
        1,
        "the live tenant's AR↔derived check still runs every tick"
    );
    assert_eq!(
        count_runs(&raw, retired, CHECK_AR_DERIVED).await,
        2,
        "the retired tenant gets no new run, and its uneventful backlog is reclaimed"
    );
    assert!(
        !run_exists(&raw, retired, uneventful).await,
        "an uneventful DONE run of a retired tenant is reclaimable noise"
    );
    assert!(
        run_exists(&raw, retired, breached).await,
        "a run that recorded a variance is evidence and is never purged"
    );
    assert!(
        run_exists(&raw, retired, unfinalized).await,
        "an unfinalized run is not proven uneventful and is never purged"
    );
}

/// K8b — the purge is opt-out, and turning it off leaves the backlog untouched
/// while the lifecycle gate still stops the tick from adding to it. (Storage
/// reclamation and the write-volume fix are independent switches: an operator may
/// want the growth stopped before any deletes are allowed to run.)
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn k8b_purge_disabled_keeps_the_backlog_but_still_skips_retired_tenants() {
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, live) = boot(&url).await;

    let retired = Uuid::now_v7();
    seed_reconcilable_tenant(&raw, retired).await;
    let uneventful = seed_recon_run(&raw, retired, "DONE", true).await;

    let config = ReconConfig {
        purge_max_rows_per_tick: 0,
        ..ReconConfig::default()
    };
    let fw = framework_with_live(
        provider,
        Arc::new(InProcessControlFeeds::new()),
        config,
        Arc::new(FixedLiveTenants(HashSet::from([live]))),
    );

    fw.run().await.unwrap();

    assert!(
        run_exists(&raw, retired, uneventful).await,
        "purge disabled: nothing is deleted"
    );
    assert_eq!(
        count_runs(&raw, retired, CHECK_AR_DERIVED).await,
        1,
        "but the retired tenant is still not reconciled — no new row"
    );
}
