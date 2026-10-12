//! The money an approved intent replays with: the credit grant and the
//! chargeback loss reach their client surface carrying the intent's own amount,
//! currency and stored scale (a scale-0 and a scale-3 currency here), never a
//! re-derived or default scale. SQLite-backed executor over a capturing client.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use bss_ledger_sdk::api::LedgerClientV1;
use bss_ledger_sdk::posting::{
    CreditApplicationApplied, DisputeOutcome, DisputeRecorded, PostingRef, ScheduleChangeRef,
};
use bss_ledger_sdk::{CurrencySpec, PostedMoney};
use sea_orm_migration::MigratorTrait;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::LedgerApprovalExecutor;
use crate::domain::approval::intent::{ApprovalIntent, ChargebackLossIntent, CreditGrantIntent};
use crate::domain::error::DomainError;
use crate::infra::approval::service::ApprovalExecutor;
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::invoice_post::InvoicePoster;
use crate::infra::storage::repo::PayerStateRepo;

fn stub_posting_ref() -> PostingRef {
    PostingRef {
        entry_id: Uuid::now_v7(),
        created_seq: 1,
        replayed: false,
    }
}

/// Captures the commands the executor dispatches; every other method is unreached.
#[derive(Clone, Default)]
struct RecordingClient {
    grants: Arc<Mutex<Vec<bss_ledger_sdk::CreditApplication>>>,
    disputes: Arc<Mutex<Vec<bss_ledger_sdk::RecordDisputePhase>>>,
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
        Ok(None)
    }

    async fn post_credit_application(
        &self,
        _ctx: &SecurityContext,
        req: bss_ledger_sdk::CreditApplication,
    ) -> Result<CreditApplicationApplied, toolkit::api::canonical_prelude::CanonicalError> {
        self.grants.lock().expect("lock").push(req);
        Ok(CreditApplicationApplied {
            posting: stub_posting_ref(),
            debits: Vec::new(),
            applications: Vec::new(),
        })
    }

    async fn record_dispute_phase(
        &self,
        _ctx: &SecurityContext,
        req: bss_ledger_sdk::RecordDisputePhase,
    ) -> Result<DisputeOutcome, toolkit::api::canonical_prelude::CanonicalError> {
        self.disputes.lock().expect("lock").push(req);
        Ok(DisputeOutcome::Recorded(DisputeRecorded {
            posting: stub_posting_ref(),
        }))
    }

    async fn change_recognition_schedule(
        &self,
        _ctx: &SecurityContext,
        _cmd: bss_ledger_sdk::ChangeRecognitionSchedule,
    ) -> Result<ScheduleChangeRef, toolkit::api::canonical_prelude::CanonicalError> {
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
    ) -> Result<Option<PostedMoney>, toolkit::api::canonical_prelude::CanonicalError> {
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

struct UnusedPoster;

#[async_trait::async_trait]
impl InvoicePoster for UnusedPoster {
    async fn post_invoice(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _inv: &crate::domain::invoice::builder::PostedInvoice,
        _payer_open: bool,
    ) -> Result<PostingRef, DomainError> {
        unimplemented!()
    }
    async fn post_reversal(
        &self,
        _ctx: &SecurityContext,
        _scope: &AccessScope,
        _reversal: bss_ledger_sdk::PostEntry,
        _reason: Option<String>,
    ) -> Result<PostingRef, DomainError> {
        unimplemented!()
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

async fn executor(client: &RecordingClient) -> LedgerApprovalExecutor {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    let provider = DBProvider::<DbError>::new(db);
    let publisher = || Arc::new(LedgerEventPublisher::noop());
    let audit = || Arc::new(crate::infra::audit::secured_audit_sink::NoopSecuredAuditSink::new());
    LedgerApprovalExecutor::new(
        Arc::new(client.clone()) as Arc<dyn LedgerClientV1>,
        Arc::new(UnusedPoster) as Arc<dyn InvoicePoster>,
        PayerStateRepo::new(provider.clone()),
        Arc::new(
            crate::infra::adjustment::refund_service::RefundHandler::new(
                provider.clone(),
                publisher(),
            ),
        ),
        Arc::new(
            crate::infra::adjustment::manual_adjustment_service::ManualAdjustmentHandler::new(
                provider.clone(),
                publisher(),
                audit(),
            ),
        ),
        Arc::new(
            crate::infra::adjustment::credit_note_service::CreditNoteHandler::new(
                provider.clone(),
                publisher(),
                Arc::new(crate::domain::ports::metrics::NoopLedgerMetrics),
            ),
        ),
        Arc::new(
            crate::infra::adjustment::debit_note_service::DebitNoteHandler::new(
                provider.clone(),
                publisher(),
                Arc::new(crate::domain::ports::metrics::NoopLedgerMetrics),
                crate::config::RecognitionConfig::default(),
            ),
        ),
        crate::infra::period_close::PeriodCloseService::new(provider, publisher(), audit()),
    )
}

fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        bss_ledger_sdk::parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::now_v7())
        .subject_tenant_id(tenant)
        .build()
        .unwrap()
}

#[tokio::test]
async fn an_approved_credit_grant_replays_the_intents_money_and_scale() {
    for amount in [money("125000", "JPY", 0), money("1200.125", "KWD", 3)] {
        let client = RecordingClient::default();
        let exec = executor(&client).await;
        let tenant = Uuid::now_v7();
        let payer = Uuid::now_v7();
        exec.execute(
            &ctx(tenant),
            &AccessScope::for_tenant(tenant),
            &ApprovalIntent::CreditGrant(CreditGrantIntent {
                tenant_id: tenant,
                payer_tenant_id: payer,
                credit_application_id: "CA-1".to_owned(),
                amount: amount.clone(),
                credit_grant_event_type: Some("promo".to_owned()),
            }),
        )
        .await
        .unwrap();
        let sent = { client.grants.lock().unwrap().clone() };
        assert_eq!(sent.len(), 1);
        match &sent[0] {
            bss_ledger_sdk::CreditApplication::Grant(grant) => {
                assert_eq!(grant.money, amount);
                assert_eq!(grant.money.currency().scale(), amount.currency().scale());
                assert_eq!(grant.tenant_id, tenant);
                assert_eq!(grant.payer_tenant_id, payer);
                assert_eq!(grant.credit_grant_event_type, "promo");
            }
            bss_ledger_sdk::CreditApplication::Apply(other) => {
                panic!("expected a grant, got an apply {other:?}")
            }
        }
    }
}

#[tokio::test]
async fn an_approved_chargeback_loss_replays_the_disputed_amount_and_scale() {
    for disputed in [money("98000", "JPY", 0), money("17.505", "KWD", 3)] {
        let client = RecordingClient::default();
        let exec = executor(&client).await;
        let tenant = Uuid::now_v7();
        exec.execute(
            &ctx(tenant),
            &AccessScope::for_tenant(tenant),
            &ApprovalIntent::ChargebackLoss(ChargebackLossIntent {
                tenant_id: tenant,
                payer_tenant_id: Uuid::now_v7(),
                payment_id: "PAY-1".to_owned(),
                dispute_id: "DSP-1".to_owned(),
                invoice_id: Some("INV-1".to_owned()),
                cycle: 2,
                funds_at_open: "withheld".to_owned(),
                disputed_amount: disputed.clone(),
            }),
        )
        .await
        .unwrap();
        let sent = { client.disputes.lock().unwrap().clone() };
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].disputed_amount, disputed);
        assert_eq!(sent[0].cycle, 2);
        assert_eq!(
            sent[0].phase,
            crate::domain::payment::chargeback::DisputePhase::Lost.as_str()
        );
        assert_eq!(sent[0].funds_at_open, "withheld");
    }
}
