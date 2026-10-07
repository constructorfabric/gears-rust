//! Fast `SQLite` coverage for the reconciliation tick's tenant-registry lifecycle
//! gate ([`ReconciliationFramework::run`]): which registry answers let the tick
//! reclaim a tenant's uneventful runs, and which must leave them alone.
//!
//! The tenant posts one entry (so it lands in the tick's `journal_entry`-derived
//! candidate set) but has no OPEN period, so the reconcile step itself is a no-op
//! here — the Postgres suite (`postgres_reconciliation.rs`, K8*) covers the
//! reconcile-or-skip half against real checks. What this pins is the purge side:
//! only a positive `Deleted` answer, with the purge switched on, may delete.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test assertions unwrap"
)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use bss_ledger::config::ReconConfig;
use bss_ledger::domain::model::{NewEntry, NewLine};
use bss_ledger::domain::ports::metrics::NoopLedgerMetrics;
use bss_ledger::infra::control_feed::InProcessControlFeeds;
use bss_ledger::infra::events::publisher::LedgerEventPublisher;
use bss_ledger::infra::exception::ExceptionRouter;
use bss_ledger::infra::reconciliation::{CHECK_AR_DERIVED, ReconciliationFramework};
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::{JournalRepo, ReconciliationRunRepo};
use bss_ledger::infra::tenant_lifecycle::{RegistryLifecycle, TenantLifecycleReader};
use bss_ledger_sdk::{
    AccountClass, IssuedInvoiceManifestV1, MappingStatus, PspSettlementFeedV1, Side, SourceDocType,
};
use chrono::NaiveDate;
use sea_orm_migration::MigratorTrait;
use serde_json::json;
use time::OffsetDateTime;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

/// What the registry double answers for every candidate.
enum Answer {
    /// Report every candidate with this lifecycle.
    All(RegistryLifecycle),
    /// Succeed, recognising none of the candidates.
    Nobody,
    /// Fail the read.
    Fail,
}

struct Registry(Answer);

#[async_trait]
impl TenantLifecycleReader for Registry {
    async fn lifecycles(
        &self,
        candidates: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, RegistryLifecycle>> {
        match self.0 {
            Answer::All(l) => Ok(candidates.iter().map(|t| (*t, l)).collect()),
            Answer::Nobody => Ok(HashMap::new()),
            Answer::Fail => Err(anyhow::anyhow!("no tenant-resolver plugin bound")),
        }
    }
}

async fn sqlite() -> DBProvider<DbError> {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect in-memory sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrator");
    DBProvider::<DbError>::new(db)
}

/// Post one balanced entry for `tenant`, making it a tick candidate.
async fn post_entry(provider: &DBProvider<DbError>, tenant: Uuid) {
    let repo = JournalRepo::new(provider.clone());
    let account_id = Uuid::now_v7();
    let entry = NewEntry {
        entry_id: Uuid::now_v7(),
        tenant_id: tenant,
        legal_entity_id: tenant,
        period_id: "202609".to_owned(),
        entry_currency: "USD".to_owned(),
        source_doc_type: SourceDocType::InvoicePost,
        source_business_id: "inv-1".to_owned(),
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: OffsetDateTime::now_utc(),
        effective_at: NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        origin: "SYSTEM".to_owned(),
        posted_by_actor_id: Uuid::now_v7(),
        correlation_id: Uuid::now_v7(),
        rounding_evidence: json!({}),
        rate_snapshot_ref: None,
    };
    let mk_line = |account_class: AccountClass, side: Side| NewLine {
        line_id: Uuid::now_v7(),
        payer_tenant_id: tenant,
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id,
        account_class,
        gl_code: None,
        side,
        amount_minor: 1000,
        currency: "USD".to_owned(),
        currency_scale: 2,
        invoice_id: Some("inv-1".to_owned()),
        due_date: None,
        revenue_stream: None,
        mapping_status: MappingStatus::Resolved,
        functional_amount_minor: None,
        functional_currency: None,
        tax_jurisdiction: None,
        tax_filing_period: None,
        tax_rate_ref: None,
        legal_entity_id: None,
        invoice_item_ref: None,
        sku_or_plan_ref: None,
        price_id: None,
        pricing_snapshot_ref: None,
        po_allocation_group: None,
        credit_grant_event_type: None,
        ar_status: None,
    };
    let lines = vec![
        mk_line(AccountClass::Ar, Side::Debit),
        mk_line(AccountClass::CashClearing, Side::Credit),
    ];
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                repo.insert_entry_with_lines(tx, entry, lines)
                    .await
                    .map_err(|e| DbError::Other(anyhow::Error::msg(e.to_string())))
            })
        })
        .await
        .expect("post entry");
}

/// Seed one uneventful finalized run for `tenant` — the backlog the purge reclaims.
async fn seed_uneventful_run(provider: &DBProvider<DbError>, tenant: Uuid) -> Uuid {
    let run_id = Uuid::now_v7();
    provider
        .transaction(move |txn| {
            Box::pin(async move {
                let scope = AccessScope::for_tenant(tenant);
                ReconciliationRunRepo::start(
                    txn,
                    &scope,
                    tenant,
                    run_id,
                    "202609",
                    CHECK_AR_DERIVED,
                )
                .await
                .map_err(|e| DbError::Other(anyhow::anyhow!("{e}")))?;
                ReconciliationRunRepo::finalize(
                    txn, &scope, tenant, run_id, "DONE", 0, true, None, None,
                )
                .await
                .map_err(|e| DbError::Other(anyhow::anyhow!("{e}")))
            })
        })
        .await
        .expect("seed run");
    run_id
}

/// Run one tick against a fresh tenant with a seeded uneventful run, and report
/// whether that run survived.
async fn run_survives_tick(answer: Answer, config: ReconConfig) -> bool {
    let provider = sqlite().await;
    let tenant = Uuid::now_v7();
    post_entry(&provider, tenant).await;
    let run_id = seed_uneventful_run(&provider, tenant).await;

    let feeds = Arc::new(InProcessControlFeeds::new());
    let fw = ReconciliationFramework::new(
        provider.clone(),
        Arc::new(LedgerEventPublisher::noop()),
        Arc::new(NoopLedgerMetrics),
        ExceptionRouter::shared(provider.clone()),
        Arc::clone(&feeds) as Arc<dyn IssuedInvoiceManifestV1>,
        feeds as Arc<dyn PspSettlementFeedV1>,
        Arc::new(Registry(answer)),
        config,
    );
    fw.run().await.expect("tick");

    ReconciliationRunRepo::new(provider)
        .read(&AccessScope::for_tenant(tenant), tenant, run_id)
        .await
        .expect("read run")
        .is_some()
}

fn purge_on() -> ReconConfig {
    ReconConfig {
        purge_max_rows_per_tick: 1_000,
        ..ReconConfig::default()
    }
}

#[tokio::test]
async fn deleted_tenant_runs_are_reclaimed_when_the_purge_is_on() {
    assert!(!run_survives_tick(Answer::All(RegistryLifecycle::Deleted), purge_on()).await);
}

#[tokio::test]
async fn deleted_tenant_runs_are_kept_by_default() {
    assert!(
        run_survives_tick(
            Answer::All(RegistryLifecycle::Deleted),
            ReconConfig::default()
        )
        .await,
        "the purge is opt-in"
    );
}

#[tokio::test]
async fn live_tenant_runs_are_never_reclaimed() {
    assert!(run_survives_tick(Answer::All(RegistryLifecycle::Live), purge_on()).await);
}

#[tokio::test]
async fn failed_registry_read_purges_nothing() {
    assert!(run_survives_tick(Answer::Fail, purge_on()).await);
}

#[tokio::test]
async fn registry_recognising_nobody_purges_nothing() {
    // single-tenant-tr-plugin's answer for the tick's anonymous context. Read
    // literally, every candidate would be "unregistered".
    assert!(run_survives_tick(Answer::Nobody, purge_on()).await);
}
