//! Postgres-only integration tests for the Slice 7 Phase 3 reconciliation framework and
//! control-feed close gate (design §4.3 / §4.5). K1: an AR↔derived variance opens a
//! `RECON_MISMATCH` and blocks close. K2: an invoice-completeness gap opens a
//! `MISSED_POSTING` that blocks close, which the upstream's idempotent re-post then
//! auto-clears. K3: a Payments↔PSP divergence opens a `PSP_VARIANCE`. K4: the
//! bill-run-finished close gate (un-asserted blocks, asserted passes). K5: inert until
//! the feed lands. Control feeds are exercised through the real in-process store (the
//! push → framework / close-gate read path). Ignored by default; run with
//! `cargo test -p bss-ledger --test postgres_reconciliation -- --ignored`.

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
use bss_ledger_sdk::{
    BillRunFinishedV1, IssuedInvoiceManifest, IssuedInvoiceManifestV1, PspSettlementFeedV1,
    PspSettlementReport, UnconfiguredBillRunFinishedV1, UnconfiguredIssuedInvoiceManifestV1,
    UnconfiguredPspSettlementFeedV1,
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
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
    raw.execute(pg(format!(
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
    raw.execute(pg(format!(
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
    txn.execute(pg(format!(
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
        txn.execute(pg(format!(
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
    txn.execute(pg(format!(
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
        txn.execute(pg(format!(
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
        .query_one(pg(format!(
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
        .query_one(pg(format!(
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
    raw.query_one(pg(format!(
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
    ReconciliationFramework::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        noop_metrics(),
        ExceptionRouter::shared(provider),
        Arc::clone(&feeds) as Arc<dyn IssuedInvoiceManifestV1>,
        Arc::clone(&feeds) as Arc<dyn PspSettlementFeedV1>,
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
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, tenant) = boot(&url).await;

    // A stray `account_balance` cache row with no journal → the recompute disagrees
    // with the cache (computed 0 ≠ cached 50_000): an out-of-tolerance tie-out variance.
    let account = Uuid::now_v7();
    raw.execute(pg(format!(
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
    let container = Postgres::default().start().await.unwrap();
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
    let container = Postgres::default().start().await.unwrap();
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
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let (raw, provider, tenant) = boot(&url).await;

    // A second OPEN fiscal period alongside the default one.
    raw.execute(pg(format!(
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
    let container = Postgres::default().start().await.unwrap();
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
    raw.execute(pg(format!(
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
    let container = Postgres::default().start().await.unwrap();
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
        .query_one(pg(format!(
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
    let container = Postgres::default().start().await.unwrap();
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
    let container = Postgres::default().start().await.unwrap();
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
