//! Tests for `LedgerApprovalExecutor` — the concrete `ApprovalExecutor` that
//! replays an approved `ApprovalIntent` against the real mutation surfaces
//! (VHP-1852, Group E). Ignored by default; run with
//! `cargo test -p bss-ledger --test postgres_executor -- --ignored`.
//!
//! Drives `execute` per intent kind against a recording stub `LedgerClientV1` +
//! stub `InvoicePoster` and a real `PayerStateRepo` (testcontainers, for the
//! payer-closure arm). Asserts each arm dispatches to the right surface, and that
//! a `Reverse` whose entry no longer resolves (`get_entry -> None`) fails closed
//! (`ApprovalNotActionable`) rather than posting a reversal — the confused-deputy
//! guard at the executor seam. The full happy reverse / material-backdating posts
//! are exercised by the e2e (they need a reversible entry / a built invoice).

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
use std::sync::Mutex;

use bss_ledger::domain::approval::intent::{
    ApprovalIntent, ChargebackLossIntent, CreditGrantIntent, PayerClosureIntent,
    RecognitionScheduleChangeIntent, ReverseIntent,
};
use bss_ledger::domain::error::DomainError;
use bss_ledger::infra::adjustment::manual_adjustment_service::ManualAdjustmentHandler;
use bss_ledger::infra::adjustment::refund_service::RefundHandler;
use bss_ledger::infra::approval::executor::LedgerApprovalExecutor;
use bss_ledger::infra::approval::service::ApprovalExecutor;
use bss_ledger::infra::events::publisher::LedgerEventPublisher;
use bss_ledger::infra::invoice_post::InvoicePoster;
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::PayerStateRepo;
use bss_ledger_sdk::api::LedgerClientV1;
use bss_ledger_sdk::posting::{
    CreditApplicationApplied, DisputeOutcome, DisputeRecorded, PostingRef, ScheduleChangeRef,
};
use sea_orm::Database;
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

async fn boot() -> (
    testcontainers_modules::testcontainers::ContainerAsync<Postgres>,
    DBProvider<DbError>,
) {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let raw = Database::connect(&url).await.unwrap();
    Migrator::up(&raw, None).await.unwrap();
    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    (container, DBProvider::<DbError>::new(tdb))
}

fn stub_posting_ref() -> PostingRef {
    PostingRef {
        entry_id: Uuid::now_v7(),
        created_seq: 1,
        replayed: false,
    }
}

/// A recording stub `LedgerClientV1`: the four methods the executor dispatches to
/// push a tag onto `calls` and return a canned Ok; the rest are unreached.
/// `get_entry` returns `None` (so the `Reverse` arm hits its not-found branch).
#[derive(Clone, Default)]
struct RecordingClient {
    calls: Arc<Mutex<Vec<&'static str>>>,
}

impl RecordingClient {
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().expect("lock").clone()
    }
}

#[async_trait::async_trait]
impl LedgerClientV1 for RecordingClient {
    async fn get_entry(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        _entry_id: Uuid,
    ) -> Result<Option<bss_ledger_sdk::EntryView>, toolkit::api::canonical_prelude::CanonicalError>
    {
        self.calls.lock().expect("lock").push("get_entry");
        Ok(None)
    }

    async fn post_credit_application(
        &self,
        _ctx: &SecurityContext,
        _req: bss_ledger_sdk::CreditApplication,
    ) -> Result<CreditApplicationApplied, toolkit::api::canonical_prelude::CanonicalError> {
        self.calls
            .lock()
            .expect("lock")
            .push("post_credit_application");
        Ok(CreditApplicationApplied {
            posting: stub_posting_ref(),
            debits: Vec::new(),
            applications: Vec::new(),
        })
    }

    async fn record_dispute_phase(
        &self,
        _ctx: &SecurityContext,
        _req: bss_ledger_sdk::RecordDisputePhase,
    ) -> Result<DisputeOutcome, toolkit::api::canonical_prelude::CanonicalError> {
        self.calls
            .lock()
            .expect("lock")
            .push("record_dispute_phase");
        Ok(DisputeOutcome::Recorded(DisputeRecorded {
            posting: stub_posting_ref(),
        }))
    }

    async fn change_recognition_schedule(
        &self,
        _ctx: &SecurityContext,
        _cmd: bss_ledger_sdk::ChangeRecognitionSchedule,
    ) -> Result<ScheduleChangeRef, toolkit::api::canonical_prelude::CanonicalError> {
        self.calls
            .lock()
            .expect("lock")
            .push("change_recognition_schedule");
        Ok(ScheduleChangeRef {
            schedule_id: "SCH-1".to_owned(),
            new_schedule_id: None,
            status: "CANCELLED".to_owned(),
        })
    }

    // ── not reached by the executor ──
    async fn return_payment(
        &self,
        _ctx: &SecurityContext,
        _req: bss_ledger_sdk::ReturnPayment,
    ) -> Result<bss_ledger_sdk::PostingRef, toolkit::api::canonical_prelude::CanonicalError> {
        unimplemented!()
    }
    async fn post_balanced_entry(
        &self,
        _ctx: &SecurityContext,
        _entry: bss_ledger_sdk::PostEntry,
    ) -> Result<bss_ledger_sdk::PostingRef, toolkit::api::canonical_prelude::CanonicalError> {
        unimplemented!()
    }
    async fn read_account_balance(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        _account_id: Uuid,
    ) -> Result<Option<i64>, toolkit::api::canonical_prelude::CanonicalError> {
        unimplemented!()
    }
    async fn list_accounts(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        _query: &bss_ledger_sdk::ODataQuery,
    ) -> Result<
        bss_ledger_sdk::Page<bss_ledger_sdk::AccountInfo>,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        unimplemented!()
    }
    async fn list_lines(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        _query: &bss_ledger_sdk::ODataQuery,
    ) -> Result<
        bss_ledger_sdk::Page<bss_ledger_sdk::LineView>,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        unimplemented!()
    }
    async fn list_balances(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        _query: &bss_ledger_sdk::ODataQuery,
    ) -> Result<
        bss_ledger_sdk::Page<bss_ledger_sdk::BalanceView>,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        unimplemented!()
    }
    async fn list_ar_invoice_balances(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        _payer_tenant_id: Option<Uuid>,
    ) -> Result<
        Vec<bss_ledger_sdk::ArInvoiceBalanceView>,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        unimplemented!()
    }
    async fn provision(
        &self,
        _ctx: &SecurityContext,
        _req: bss_ledger_sdk::ProvisionRequest,
    ) -> Result<bss_ledger_sdk::ProvisionOutcome, toolkit::api::canonical_prelude::CanonicalError>
    {
        unimplemented!()
    }
    async fn close_period(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        _period_id: String,
    ) -> Result<bss_ledger_sdk::CloseOutcome, toolkit::api::canonical_prelude::CanonicalError> {
        unimplemented!()
    }
    async fn settle_payment(
        &self,
        _ctx: &SecurityContext,
        _req: bss_ledger_sdk::SettlePayment,
    ) -> Result<bss_ledger_sdk::PostingRef, toolkit::api::canonical_prelude::CanonicalError> {
        unimplemented!()
    }
    async fn allocate_payment(
        &self,
        _ctx: &SecurityContext,
        _req: bss_ledger_sdk::AllocatePayment,
    ) -> Result<bss_ledger_sdk::AllocateOutcome, toolkit::api::canonical_prelude::CanonicalError>
    {
        unimplemented!()
    }
    async fn list_payment_allocations(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        _payment_id: String,
    ) -> Result<Vec<bss_ledger_sdk::AllocationView>, toolkit::api::canonical_prelude::CanonicalError>
    {
        unimplemented!()
    }
    async fn read_unallocated(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        _payer_tenant_id: Uuid,
        _currency: String,
    ) -> Result<bss_ledger_sdk::UnallocatedView, toolkit::api::canonical_prelude::CanonicalError>
    {
        unimplemented!()
    }
    async fn trigger_recognition_run(
        &self,
        _ctx: &SecurityContext,
        _req: bss_ledger_sdk::TriggerRecognitionRun,
    ) -> Result<
        bss_ledger_sdk::RecognitionRunOutcome,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        unimplemented!()
    }
    async fn list_revenue_disaggregation(
        &self,
        _ctx: &SecurityContext,
        _query: bss_ledger_sdk::RevenueDisaggregationQuery,
    ) -> Result<
        bss_ledger_sdk::RevenueDisaggregation,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        unimplemented!()
    }
    async fn get_recognition_schedule(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        _schedule_id: String,
    ) -> Result<
        Option<bss_ledger_sdk::RecognitionScheduleView>,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        unimplemented!()
    }
    async fn list_recognition_schedules(
        &self,
        _ctx: &SecurityContext,
        _tenant_id: Uuid,
        _invoice_id: Option<String>,
        _revenue_stream: Option<String>,
    ) -> Result<
        bss_ledger_sdk::RecognitionScheduleList,
        toolkit::api::canonical_prelude::CanonicalError,
    > {
        unimplemented!()
    }
}

/// Stub `InvoicePoster` — the executor's reverse/backdating posts are not reached
/// by these arms (reverse hits the `get_entry -> None` branch first).
struct UnusedPoster;

#[async_trait::async_trait]
impl InvoicePoster for UnusedPoster {
    async fn post_invoice(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _inv: &bss_ledger::domain::invoice::builder::PostedInvoice,
        _payer_open: bool,
    ) -> Result<PostingRef, DomainError> {
        unimplemented!("material-backdating post is exercised by the e2e")
    }
    async fn post_reversal(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _reversal: bss_ledger_sdk::PostEntry,
        _reason: Option<String>,
    ) -> Result<PostingRef, DomainError> {
        unimplemented!("the reverse arm fails at get_entry -> None before posting")
    }
    async fn post_correction(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _correction: bss_ledger_sdk::PostEntry,
    ) -> Result<PostingRef, DomainError> {
        unimplemented!()
    }
}

fn ctx_for(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .subject_type("gts.cf.core.security.subject_user.v1~")
        .token_scopes(vec!["*".to_owned()])
        .build()
        .expect("authed SecurityContext must build")
}

fn executor(provider: &DBProvider<DbError>, client: &RecordingClient) -> LedgerApprovalExecutor {
    LedgerApprovalExecutor::new(
        Arc::new(client.clone()) as Arc<dyn LedgerClientV1>,
        Arc::new(UnusedPoster) as Arc<dyn InvoicePoster>,
        PayerStateRepo::new(provider.clone()),
        // The refund replay arm is exercised by `postgres_dual_control.rs` end-to-end
        // (gate -> approve -> post); here the un-gated handler is just wired so the
        // executor constructs (the client-routed arms under test never touch it).
        Arc::new(RefundHandler::new(
            provider.clone(),
            Arc::new(LedgerEventPublisher::noop()),
        )),
        // Likewise the manual-adjustment replay arm: an un-gated handler wired only so
        // the executor constructs (the client-routed arms under test never touch it).
        Arc::new(ManualAdjustmentHandler::new(
            provider.clone(),
            Arc::new(LedgerEventPublisher::noop()),
            Arc::new(bss_ledger::infra::audit::secured_audit_sink::NoopSecuredAuditSink::new()),
        )),
        // The note replay arms (CreditNote/DebitNote) are un-gated handlers wired only
        // so the executor constructs (the client-routed arms under test never touch
        // them; the gated gate->approve->post path is exercised elsewhere).
        Arc::new(
            bss_ledger::infra::adjustment::credit_note_service::CreditNoteHandler::new(
                provider.clone(),
                Arc::new(LedgerEventPublisher::noop()),
                Arc::new(bss_ledger::domain::ports::metrics::NoopLedgerMetrics),
            ),
        ),
        Arc::new(
            bss_ledger::infra::adjustment::debit_note_service::DebitNoteHandler::new(
                provider.clone(),
                Arc::new(LedgerEventPublisher::noop()),
                Arc::new(bss_ledger::domain::ports::metrics::NoopLedgerMetrics),
                bss_ledger::config::RecognitionConfig::default(),
            ),
        ),
        bss_ledger::infra::period_close::PeriodCloseService::new(
            provider.clone(),
            Arc::new(LedgerEventPublisher::noop()),
            Arc::new(bss_ledger::infra::audit::secured_audit_sink::NoopSecuredAuditSink::new()),
        ),
    )
}

/// Each client-routed intent dispatches to its matching `LedgerClientV1` surface
/// (the surface re-applies its own PEP gate + idempotency — replay is safe).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn client_routed_intents_dispatch_to_their_surface() {
    let (_c, provider) = boot().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx_for(tenant);
    let client = RecordingClient::default();
    let exec = executor(&provider, &client);

    exec.execute(
        &ctx,
        &scope,
        &ApprovalIntent::CreditGrant(CreditGrantIntent {
            tenant_id: tenant,
            payer_tenant_id: Uuid::now_v7(),
            credit_application_id: "CA-1".to_owned(),
            currency: "USD".to_owned(),
            amount_minor: 120_000,
            credit_grant_event_type: Some("promo".to_owned()),
        }),
    )
    .await
    .expect("credit grant replays");

    exec.execute(
        &ctx,
        &scope,
        &ApprovalIntent::ChargebackLoss(ChargebackLossIntent {
            tenant_id: tenant,
            payer_tenant_id: Uuid::now_v7(),
            payment_id: "PAY-1".to_owned(),
            dispute_id: "DSP-1".to_owned(),
            invoice_id: Some("INV-1".to_owned()),
            cycle: 1,
            funds_at_open: "withheld".to_owned(),
            disputed_amount_minor: 120_000,
            currency: "USD".to_owned(),
        }),
    )
    .await
    .expect("chargeback loss replays");

    exec.execute(
        &ctx,
        &scope,
        &ApprovalIntent::RecognitionScheduleChange(RecognitionScheduleChangeIntent {
            tenant_id: tenant,
            schedule_id: "SCH-1".to_owned(),
            change_id: "CHG-1".to_owned(),
            action: "cancel".to_owned(),
            treatment: "prospective".to_owned(),
            new_segments: None,
        }),
    )
    .await
    .expect("schedule change replays");

    assert_eq!(
        client.calls(),
        vec![
            "post_credit_application",
            "record_dispute_phase",
            "change_recognition_schedule"
        ]
    );
}

/// Confused-deputy guard: a `Reverse` whose entry no longer resolves under the
/// approver's tenant (`get_entry -> None`) fails closed (`ApprovalNotActionable`),
/// it never posts a reversal.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn reverse_of_a_vanished_entry_fails_closed() {
    let (_c, provider) = boot().await;
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx_for(tenant);
    let client = RecordingClient::default();
    let exec = executor(&provider, &client);

    let err = exec
        .execute(
            &ctx,
            &scope,
            &ApprovalIntent::Reverse(ReverseIntent {
                entry_id: Uuid::now_v7(),
                into_period_id: None,
                effective_at: None,
                reason: "reverse".to_owned(),
            }),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DomainError::ApprovalNotActionable(_)),
        "a vanished entry must fail closed, got {err:?}"
    );
    assert_eq!(
        client.calls(),
        vec!["get_entry"],
        "only the read was attempted"
    );
}

/// The `PayerClosure` arm runs the real `PayerStateRepo.close` under the
/// approver's scope — the row lands CLOSED for the intent's tenant/payer.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn payer_closure_intent_closes_the_payer() {
    let (_c, provider) = boot().await;
    let tenant = Uuid::now_v7();
    let payer = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);
    let ctx = ctx_for(tenant);
    let client = RecordingClient::default();
    let exec = executor(&provider, &client);

    exec.execute(
        &ctx,
        &scope,
        &ApprovalIntent::PayerClosure(PayerClosureIntent {
            tenant_id: tenant,
            payer_tenant_id: payer,
            closed_with_open_balance: true,
            disposition: None,
        }),
    )
    .await
    .expect("payer closure replays");

    let row = PayerStateRepo::new(provider.clone())
        .read(&scope, tenant, payer)
        .await
        .expect("read")
        .expect("payer-state row present after closure");
    assert_eq!(row.lifecycle_state, "CLOSED");
    assert!(row.closed_with_open_balance);
}
