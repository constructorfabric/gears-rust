//! Real migrated lifecycle tests; the executor is a test port, not financial posting.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::domain::approval::intent::{CreditGrantIntent, PayerClosureIntent};
use crate::domain::ports::metrics::NoopLedgerMetrics;
use crate::infra::posting::retry::retry_transaction;
use bss_ledger_sdk::parse_decimal;
use sea_orm_migration::MigratorTrait;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{Mutex, Notify};
use toolkit_db::secure::Db;
use toolkit_db::{ConnectOpts, connect_db};

#[derive(Default)]
struct Executor {
    // Held across the deterministic execution barrier, modelling one idempotent executor port.
    intents: Mutex<Vec<ApprovalIntent>>,
    // Every `execute` call, including repeats `intents` deduplicates.
    calls: AtomicUsize,
    fail: AtomicBool,
    pause: AtomicBool,
    entered: Notify,
    release: Notify,
}
#[async_trait::async_trait]
impl ApprovalExecutor for Executor {
    async fn execute(
        &self,
        _: &SecurityContext,
        _: &AccessScope,
        intent: &ApprovalIntent,
    ) -> Result<(), DomainError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut intents = self.intents.lock().await;
        if !intents.contains(intent) {
            intents.push(intent.clone());
        }
        self.entered.notify_one();
        if self.pause.load(Ordering::SeqCst) {
            self.release.notified().await;
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(DomainError::PeriodClosed("test execution failed".into()));
        }
        Ok(())
    }
}
fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.into(), scale).unwrap(),
    )
    .unwrap()
}
fn context(tenant: Uuid, actor: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(actor)
        .subject_tenant_id(tenant)
        .build()
        .unwrap()
}
async fn service(
    db: Db,
) -> (
    ApprovalService,
    Arc<Executor>,
    SecurityContext,
    SecurityContext,
    AccessScope,
) {
    if db.backend() == sea_orm::DbBackend::Sqlite {
        toolkit_db::migration_runner::run_migrations_for_testing(
            &db,
            crate::infra::storage::migrations::Migrator::migrations(),
        )
        .await
        .unwrap();
    }
    let tenant = Uuid::now_v7();
    let executor = Arc::new(Executor::default());
    let service = ApprovalService::new(
        DBProvider::new(db),
        executor.clone(),
        Arc::new(NoopLedgerMetrics),
        FxConfig::default(),
    );
    (
        service,
        executor,
        context(tenant, Uuid::now_v7()),
        context(tenant, Uuid::now_v7()),
        AccessScope::for_tenant(tenant),
    )
}
async fn setup() -> (
    ApprovalService,
    Arc<Executor>,
    SecurityContext,
    SecurityContext,
    AccessScope,
) {
    service(
        connect_db("sqlite::memory:", ConnectOpts::default())
            .await
            .unwrap(),
    )
    .await
}
fn grant(ctx: &SecurityContext, amount: PostedMoney) -> ApprovalIntent {
    ApprovalIntent::CreditGrant(CreditGrantIntent {
        tenant_id: ctx.subject_tenant_id(),
        payer_tenant_id: Uuid::from_u128(123),
        credit_application_id: "app:001".into(),
        amount,
        credit_grant_event_type: Some("001.00".into()),
    })
}
fn facts(intent: &ApprovalIntent) -> OperationFacts {
    OperationFacts {
        kind: intent.kind(),
        amount: intent.amount().unwrap(),
        effective_at: None,
        has_outstanding_balance: false,
    }
}
async fn pending(
    s: &ApprovalService,
    ctx: &SecurityContext,
    scope: &AccessScope,
    intent: ApprovalIntent,
) -> Uuid {
    s.gate(ctx, scope, intent.clone(), facts(&intent), "POLICY".into())
        .await
        .unwrap()
        .unwrap()
}

async fn lifecycle(db: Db) {
    let (s, executor, preparer, approver, scope) = service(db).await;
    let original = grant(&preparer, money("1000", "EUR", 2));
    let id = pending(&s, &preparer, &scope, original.clone()).await;
    assert_eq!(pending(&s, &preparer, &scope, original.clone()).await, id);
    assert!(matches!(
        s.approve(&preparer, &scope, id).await,
        Err(DomainError::SelfApprovalForbidden(_))
    ));
    s.add_comment(&preparer, &scope, id, "001.00".into())
        .await
        .unwrap();
    s.request_changes(&approver, &scope, id, "lower amount".into())
        .await
        .unwrap();
    let edited = grant(&preparer, money("999.99", "EUR", 2));
    s.resubmit(&preparer, &scope, id, edited.clone())
        .await
        .unwrap();
    let row = s.get(&preparer, &scope, id).await.unwrap().unwrap();
    assert_eq!((row.state.as_str(), row.revision), ("PENDING", 1));
    assert_eq!(row.amount, edited.amount().unwrap());
    assert_eq!(decode_intent(row.intent.clone()).unwrap(), edited);
    assert_eq!(
        row.threshold_snapshot["basis"],
        "resubmission_transaction_snapshot"
    );
    assert_eq!(row.threshold_snapshot["d2_threshold"]["currency"], "EUR");
    assert_eq!(row.threshold_snapshot["d2_threshold"]["amount"], "1000");
    s.approve(&approver, &scope, id).await.unwrap();
    let row = s.get(&preparer, &scope, id).await.unwrap().unwrap();
    assert_eq!(row.state, "APPROVED");
    assert_eq!(row.approved_by, Some(approver.subject_id()));
    let seen = { executor.intents.lock().await.clone() };
    assert_eq!(seen, vec![edited]);
    let thread = s.thread(&preparer, &scope, id).await.unwrap();
    assert_eq!(thread.len(), 4);
    assert_eq!(
        thread.iter().map(|v| v.revision).collect::<Vec<_>>(),
        vec![0, 0, 1, 1]
    );
    assert_eq!(thread[0].body, "001.00");
    let decision: serde_json::Value = serde_json::from_str(&thread[3].body).unwrap();
    assert_eq!(decision["decision"], "approved");
    assert!(matches!(
        s.approve(&approver, &scope, id).await,
        Err(DomainError::ApprovalNotActionable(_))
    ));
}
#[tokio::test]
async fn sqlite_lifecycle_keeps_typed_intent_metadata_and_audit() {
    lifecycle(
        connect_db("sqlite::memory:", ConnectOpts::default())
            .await
            .unwrap(),
    )
    .await;
}
#[tokio::test]
async fn stale_observed_revision_cannot_latch_or_decide_after_rework_resubmit_aba() {
    let (s, executor, preparer, approver, scope) = setup().await;
    let original = grant(&preparer, money("1000", "EUR", 2));
    let id = pending(&s, &preparer, &scope, original).await;
    let stale = s
        .load_for_approve(&scope, preparer.subject_tenant_id(), id)
        .await
        .unwrap();
    s.request_changes(&approver, &scope, id, "edit".into())
        .await
        .unwrap();
    let edited = grant(&preparer, money("2000", "EUR", 2));
    s.resubmit(&preparer, &scope, id, edited.clone())
        .await
        .unwrap();
    assert!(
        !s.bare_transition(
            preparer.subject_tenant_id(),
            &scope,
            id,
            ApprovalState::Pending,
            stale.revision,
            ApprovalState::Approving
        )
        .await
        .unwrap()
    );
    assert!(matches!(
        s.commit_transition(
            preparer.subject_tenant_id(),
            &scope,
            id,
            ApprovalState::Pending,
            ApprovalState::Rejected,
            approver.subject_id(),
            stale.revision,
            "stale".into()
        )
        .await,
        Err(DomainError::ApprovalNotActionable(_))
    ));
    let executed = { executor.intents.lock().await.len() };
    assert_eq!(executed, 0);
    assert_eq!(s.thread(&preparer, &scope, id).await.unwrap().len(), 2);
    s.approve(&approver, &scope, id).await.unwrap();
    let seen = { executor.intents.lock().await.clone() };
    assert_eq!(seen, vec![edited]);
}
#[tokio::test]
async fn executor_failure_reverts_pinned_latch_and_recovery_replays_same_revision() {
    let (s, executor, preparer, approver, scope) = setup().await;
    let intent = grant(&preparer, money("1000", "EUR", 2));
    let id = pending(&s, &preparer, &scope, intent.clone()).await;
    executor.fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        s.approve(&approver, &scope, id).await,
        Err(DomainError::PeriodClosed(_))
    ));
    assert_eq!(
        s.get(&preparer, &scope, id).await.unwrap().unwrap().state,
        "PENDING"
    );
    assert!(s.thread(&preparer, &scope, id).await.unwrap().is_empty());
    executor.fail.store(false, Ordering::SeqCst);
    // Simulate the durable crash window after latch, before execute/mark.
    assert!(
        s.bare_transition(
            preparer.subject_tenant_id(),
            &scope,
            id,
            ApprovalState::Pending,
            0,
            ApprovalState::Approving
        )
        .await
        .unwrap()
    );
    s.approve(&approver, &scope, id).await.unwrap();
    let seen = { executor.intents.lock().await.clone() };
    assert_eq!(seen, vec![intent]);
    // The failed first approve executed once; the APPROVING recovery re-executed
    // (the deduplicating `intents` alone cannot tell), then marked APPROVED.
    assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
    let row = s.get(&preparer, &scope, id).await.unwrap().unwrap();
    assert_eq!(row.state, ApprovalState::Approved.as_str());
    assert_eq!(row.revision, 0);
}
#[tokio::test]
async fn concurrent_decisions_cannot_reject_cancel_or_rework_latched_execution() {
    let (s, executor, preparer, approver, scope) = setup().await;
    let id = pending(
        &s,
        &preparer,
        &scope,
        grant(&preparer, money("1000", "EUR", 2)),
    )
    .await;
    executor.pause.store(true, Ordering::SeqCst);
    let task = {
        let s = s.clone();
        let approver = approver.clone();
        let scope = scope.clone();
        tokio::spawn(async move { s.approve(&approver, &scope, id).await })
    };
    executor.entered.notified().await;
    assert!(matches!(
        s.reject(&approver, &scope, id, "reject".into()).await,
        Err(DomainError::ApprovalNotActionable(_))
    ));
    assert!(matches!(
        s.cancel(&preparer, &scope, id).await,
        Err(DomainError::ApprovalNotActionable(_))
    ));
    assert!(matches!(
        s.request_changes(&approver, &scope, id, "edit".into())
            .await,
        Err(DomainError::ApprovalNotActionable(_))
    ));
    executor.release.notify_one();
    task.await.unwrap().unwrap();
    assert_eq!(
        s.get(&preparer, &scope, id).await.unwrap().unwrap().state,
        "APPROVED"
    );
}
#[tokio::test]
async fn policy_validates_registry_preserves_historical_scale_and_named_errors() {
    let (s, _, preparer, approver, scope) = setup().await;
    let now = OffsetDateTime::now_utc();
    assert!(matches!(
        s.set_policy(
            &preparer,
            &scope,
            vec![money("1000", "EUR", 3)],
            5,
            1000,
            now
        )
        .await,
        Err(DomainError::InconsistentScale(_))
    ));
    // With several thresholds, the refusal names the stale currency and both
    // scales (the registry's and the submitted one).
    match s
        .set_policy(
            &preparer,
            &scope,
            vec![money("1000", "USD", 2), money("100000", "JPY", 2)],
            5,
            1000,
            now,
        )
        .await
    {
        Err(DomainError::InconsistentScale(detail)) => assert!(
            detail.contains("JPY")
                && detail.contains("registered at scale 0")
                && detail.contains("submitted at scale 2"),
            "{detail}"
        ),
        other => panic!("expected InconsistentScale, got {other:?}"),
    }
    assert!(matches!(
        s.set_policy(
            &preparer,
            &scope,
            vec![money("1000", "ZZZZ", 2)],
            5,
            1000,
            now
        )
        .await,
        Err(DomainError::InvalidRequest(_))
    ));
    assert!(
        s.read_effective_policy(&scope, preparer.subject_tenant_id(), now)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        s.set_policy(
            &preparer,
            &scope,
            vec![money("99.99", "EUR", 2)],
            5,
            1000,
            now
        )
        .await,
        Err(DomainError::DualControlPolicyOutOfRange(_))
    ));
    assert_eq!(
        s.set_policy(
            &preparer,
            &scope,
            vec![money("1000", "EUR", 2)],
            5,
            1000,
            now
        )
        .await
        .unwrap(),
        0
    );
    let intent = grant(&preparer, money("1000", "EUR", 2));
    let id = pending(&s, &preparer, &scope, intent).await;
    let row = s.get(&preparer, &scope, id).await.unwrap().unwrap();
    assert_eq!(row.threshold_snapshot["policy_version"], 0);
    assert!(row.threshold_snapshot["policy_effective_from"].is_string());
    assert_eq!(row.threshold_snapshot["basis"], "transaction_gate");
    s.approve(&approver, &scope, id).await.unwrap();
}
#[tokio::test]
async fn structural_absence_scope_and_terminal_decisions_are_preserved() {
    let (s, executor, preparer, approver, scope) = setup().await;
    let intent = ApprovalIntent::PayerClosure(PayerClosureIntent {
        tenant_id: preparer.subject_tenant_id(),
        payer_tenant_id: Uuid::now_v7(),
        closed_with_open_balance: true,
        disposition: Some("001.00".into()),
    });
    let mut f = facts(&intent);
    f.has_outstanding_balance = true;
    let id = s
        .gate(&preparer, &scope, intent, f, "CLOSURE".into())
        .await
        .unwrap()
        .unwrap();
    let row = s.get(&preparer, &scope, id).await.unwrap().unwrap();
    assert_eq!(row.amount, None);
    assert!(
        !row.threshold_snapshot
            .as_object()
            .unwrap()
            .contains_key("d2_threshold")
    );
    assert!(
        s.get(&preparer, &AccessScope::for_tenant(Uuid::now_v7()), id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        s.cancel(&approver, &scope, id).await,
        Err(DomainError::ApprovalNotActionable(_))
    ));
    s.cancel(&preparer, &scope, id).await.unwrap();
    assert_eq!(
        s.get(&preparer, &scope, id).await.unwrap().unwrap().state,
        "CANCELLED"
    );
    let id = pending(
        &s,
        &preparer,
        &scope,
        grant(&preparer, money("1000", "EUR", 2)),
    )
    .await;
    s.reject(&approver, &scope, id, "reject reason".into())
        .await
        .unwrap();
    assert_eq!(
        s.get(&preparer, &scope, id).await.unwrap().unwrap().state,
        "REJECTED"
    );
    let executed = { executor.intents.lock().await.len() };
    assert_eq!(executed, 0);
}
#[tokio::test]
async fn invalid_stored_intent_money_is_internal_and_never_executes() {
    use crate::infra::storage::entity::dual_control_approval as approval;
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait};
    use toolkit_db::secure::SecureUpdateExt;
    let (s, executor, preparer, approver, scope) = setup().await;
    let id = pending(
        &s,
        &preparer,
        &scope,
        grant(&preparer, money("1000", "EUR", 2)),
    )
    .await;
    let mut stored = s.get(&preparer, &scope, id).await.unwrap().unwrap().intent;
    stored["amount"]["amount"] = "0.001".into();
    retry_transaction(&s.db.db(), |tx| {
        let scope = scope.clone();
        let stored = stored.clone();
        Box::pin(async move {
            approval::Entity::update_many()
                .secure()
                .scope_with(&scope)
                .col_expr(approval::Column::Intent, Expr::value(stored))
                .filter(approval::Column::ApprovalId.eq(id).into())
                .exec(tx)
                .await
                .map_err(|e| AttemptError::Business(DomainError::Internal(e.to_string())))?;
            Ok(())
        })
    })
    .await
    .unwrap();
    assert!(matches!(
        s.approve(&approver, &scope, id).await,
        Err(DomainError::Internal(_))
    ));
    assert_eq!(
        s.get(&preparer, &scope, id).await.unwrap().unwrap().state,
        "PENDING"
    );
    let executed = { executor.intents.lock().await.len() };
    assert_eq!(executed, 0);
}
#[tokio::test]
#[ignore = "requires local Docker PostgreSQL testcontainer"]
async fn postgres_lifecycle_and_simultaneous_approve_reject_race() {
    use testcontainers_modules::testcontainers::runners::AsyncRunner;
    let container = test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let raw = sea_orm::Database::connect(&url).await.unwrap();
    crate::infra::storage::migrations::Migrator::up(&raw, None)
        .await
        .unwrap();
    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let db = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    lifecycle(db.clone()).await;
    // Migrations are already applied; a second fixture uses the same isolated container.
    let (s, executor, preparer, approver, scope) = service(db).await;
    let id = pending(
        &s,
        &preparer,
        &scope,
        grant(&preparer, money("1000", "EUR", 2)),
    )
    .await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let approve = {
        let s = s.clone();
        let ctx = approver.clone();
        let scope = scope.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            s.approve(&ctx, &scope, id).await
        })
    };
    let reject = {
        let s = s.clone();
        let ctx = approver.clone();
        let scope = scope.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            s.reject(&ctx, &scope, id, "race".into()).await
        })
    };
    let a = approve.await.unwrap();
    let r = reject.await.unwrap();
    assert_ne!(a.is_ok(), r.is_ok());
    let row = s.get(&preparer, &scope, id).await.unwrap().unwrap();
    assert_eq!(s.thread(&preparer, &scope, id).await.unwrap().len(), 1);
    if a.is_ok() {
        assert_eq!(row.state, "APPROVED");
        let executed = { executor.intents.lock().await.len() };
        assert_eq!(executed, 1);
    } else {
        assert_eq!(row.state, "REJECTED");
        let executed = { executor.intents.lock().await.len() };
        assert_eq!(executed, 0);
    }
}

async fn functional(s: &ApprovalService, ctx: &SecurityContext, scope: &AccessScope, code: &str) {
    let row = crate::domain::model::FiscalCalendarRow {
        tenant_id: ctx.subject_tenant_id(),
        legal_entity_id: Uuid::now_v7(),
        fiscal_tz: "UTC".into(),
        granularity: "MONTH".into(),
        fy_start_month: 1,
        functional_currency: Some(code.into()),
    };
    retry_transaction(&s.db.db(), |tx| {
        let row = row.clone();
        let reference = s.reference.clone();
        Box::pin(async move {
            reference
                .upsert_fiscal_calendar_if_absent_txn(tx, row)
                .await
                .map_err(AttemptError::from)?;
            Ok(())
        })
    })
    .await
    .unwrap();
    assert_eq!(
        s.reference
            .functional_currency(scope, ctx.subject_tenant_id())
            .await
            .unwrap()
            .as_deref(),
        Some(code)
    );
}
async fn locked_payment(
    s: &ApprovalService,
    ctx: &SecurityContext,
    scope: &AccessScope,
    base: CurrencySpec,
    quote: CurrencySpec,
) -> ApprovalIntent {
    use crate::domain::model::{NewEntry, NewLine};
    use crate::infra::storage::repo::fx_repo::NewRateSnapshot;
    use bss_ledger_sdk::{AccountClass, MappingStatus, Side};
    let tenant = ctx.subject_tenant_id();
    let amount = money("1000", "EUR", 2);
    let snapshot = NewRateSnapshot {
        tenant_id: tenant,
        base_currency: base,
        quote_currency: quote,
        rate: parse_decimal("2.000000000000000000000000001").unwrap(),
        as_of: OffsetDateTime::now_utc(),
        provider: "locked".into(),
        stale: false,
        fallback_order: 0,
        triangulated_via: None,
    };
    let line = NewLine {
        line_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: Uuid::now_v7(),
        account_class: AccountClass::Ar,
        gl_code: None,
        side: Side::Debit,
        money: amount.clone(),
        invoice_id: None,
        due_date: None,
        revenue_stream: None,
        mapping_status: MappingStatus::Resolved,
        functional_money: None,
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
    let mut credit = line.clone();
    credit.line_id = Uuid::now_v7();
    credit.account_id = Uuid::now_v7();
    credit.side = Side::Credit;
    let entry = NewEntry {
        entry_id: Uuid::now_v7(),
        tenant_id: tenant,
        legal_entity_id: Uuid::now_v7(),
        period_id: "202610".into(),
        entry_currency: "EUR".into(),
        source_doc_type: SourceDocType::PaymentSettle,
        source_business_id: "pay".into(),
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: OffsetDateTime::now_utc(),
        effective_at: chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(),
        origin: "USER".into(),
        posted_by_actor_id: ctx.subject_id(),
        correlation_id: Uuid::now_v7(),
        rounding_evidence: serde_json::json!({}),
        rate_snapshot_ref: None,
    };
    retry_transaction(&s.db.db(), |tx| {
        let mut entry = entry.clone();
        let snapshot = snapshot.clone();
        let lines = vec![line.clone(), credit.clone()];
        let scope = scope.clone();
        let db = s.db.clone();
        let journal = s.journal.clone();
        Box::pin(async move {
            entry.rate_snapshot_ref = Some(
                FxRepo::new(db)
                    .insert_snapshot_in(tx, &scope, &snapshot)
                    .await
                    .map_err(AttemptError::from)?,
            );
            journal
                .insert_entry_with_lines(tx, entry, lines)
                .await
                .map_err(AttemptError::from)?;
            Ok(())
        })
    })
    .await
    .unwrap();
    ApprovalIntent::Refund(crate::domain::approval::intent::RefundIntent {
        tenant_id: tenant,
        payer_tenant_id: Uuid::now_v7(),
        refund_id: "refund".into(),
        psp_refund_id: "psp".into(),
        phase: "confirmed".into(),
        pattern: crate::domain::adjustment::refund::RefundPattern::AUnallocated
            .as_str()
            .into(),
        payment_id: "pay".into(),
        invoice_id: None,
        amount,
        two_stage: false,
        relates_to_refund_id: None,
        direction: crate::domain::adjustment::refund::RefundDirection::Outbound
            .as_str()
            .into(),
    })
}
#[tokio::test]
async fn functional_valuation_uses_full_locked_pair_and_historical_target_scale() {
    let (s, _, preparer, approver, scope) = setup().await;
    functional(&s, &preparer, &scope, "JPY").await;
    let intent = locked_payment(
        &s,
        &preparer,
        &scope,
        CurrencySpec::try_new("EUR".into(), 2).unwrap(),
        CurrencySpec::try_new("JPY".into(), 2).unwrap(),
    )
    .await;
    // Historical JPY scale 2 is deliberately different from today's ISO scale 0.
    let id = pending(&s, &preparer, &scope, intent.clone()).await;
    let row = s.get(&preparer, &scope, id).await.unwrap().unwrap();
    assert_eq!(row.amount, Some(money("2000", "JPY", 2)));
    assert_eq!(row.threshold_snapshot["basis"], "functional_gate");
    assert_eq!(row.threshold_snapshot["d2_threshold"]["currency_scale"], 2);
    assert_eq!(row.threshold_snapshot["d2_threshold"]["amount"], "1000");
    s.request_changes(&approver, &scope, id, "same request".into())
        .await
        .unwrap();
    s.resubmit(&preparer, &scope, id, intent).await.unwrap();
    let row = s.get(&preparer, &scope, id).await.unwrap().unwrap();
    assert_eq!(row.amount, Some(money("1000", "EUR", 2)));
    assert_eq!(
        row.threshold_snapshot["basis"],
        "resubmission_transaction_snapshot"
    );
    assert_eq!(row.threshold_snapshot["d2_threshold"]["currency"], "EUR");
}
#[tokio::test]
async fn locked_metadata_mismatch_is_named_and_does_not_fallback_to_current_quote() {
    for (base, quote, scale_error) in [
        ("EUR", "JPY", true),
        ("USD", "JPY", false),
        ("EUR", "USD", false),
    ] {
        let (s, _, preparer, _, scope) = setup().await;
        functional(&s, &preparer, &scope, "JPY").await;
        let intent = locked_payment(
            &s,
            &preparer,
            &scope,
            CurrencySpec::try_new(base.into(), if scale_error { 3 } else { 2 }).unwrap(),
            CurrencySpec::try_new(quote.into(), 0).unwrap(),
        )
        .await;
        let result = s
            .gate(
                &preparer,
                &scope,
                intent.clone(),
                facts(&intent),
                "gate".into(),
            )
            .await;
        if scale_error {
            assert!(matches!(result, Err(DomainError::InconsistentScale(_))));
        } else {
            assert!(matches!(result, Err(DomainError::CurrencyMismatch(_))));
        }
        assert!(
            s.list(&preparer, &scope, None, None)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
#[tokio::test]
async fn reverse_and_recognition_keep_transaction_basis_without_current_fx() {
    use crate::domain::approval::intent::{RecognitionScheduleChangeIntent, ReverseIntent};
    let (s, _, preparer, _, scope) = setup().await;
    functional(&s, &preparer, &scope, "JPY").await;
    for intent in [
        ApprovalIntent::Reverse(ReverseIntent {
            entry_id: Uuid::now_v7(),
            into_period_id: None,
            effective_at: None,
            reason: "001.00".into(),
        }),
        ApprovalIntent::RecognitionScheduleChange(RecognitionScheduleChangeIntent {
            tenant_id: preparer.subject_tenant_id(),
            schedule_id: "schedule".into(),
            change_id: "change".into(),
            action: "replace".into(),
            treatment: "prospective".into(),
            new_segments: None,
        }),
    ] {
        let f = OperationFacts {
            kind: intent.kind(),
            amount: Some(money("1000", "EUR", 2)),
            effective_at: None,
            has_outstanding_balance: false,
        };
        let id = s
            .gate(&preparer, &scope, intent, f, "derived".into())
            .await
            .unwrap()
            .unwrap();
        let row = s.get(&preparer, &scope, id).await.unwrap().unwrap();
        assert_eq!(row.amount, Some(money("1000", "EUR", 2)));
        assert_eq!(row.threshold_snapshot["basis"], "transaction_gate");
    }
}
#[tokio::test]
async fn active_business_key_idempotence_binds_the_full_canonical_intent() {
    let (s, _, preparer, _, scope) = setup().await;
    let intent = grant(&preparer, money("1000", "EUR", 2));
    let id = pending(&s, &preparer, &scope, intent.clone()).await;
    let mut changed = intent.clone();
    if let ApprovalIntent::CreditGrant(i) = &mut changed {
        i.amount = money("1000.01", "EUR", 2);
    }
    assert!(matches!(
        s.gate(
            &preparer,
            &scope,
            changed.clone(),
            facts(&changed),
            "changed".into()
        )
        .await,
        Err(DomainError::ApprovalNotActionable(_))
    ));
    assert_eq!(
        s.get(&preparer, &scope, id).await.unwrap().unwrap().amount,
        intent.amount().unwrap()
    );
}

#[tokio::test]
async fn stored_intent_and_policy_snapshots_keep_specs_after_registry_changes() {
    let (s, _, preparer, approver, scope) = setup().await;
    let tenant = preparer.subject_tenant_id();
    for scale in [3, 4] {
        s.reference
            .upsert_currency_scale(crate::domain::model::CurrencyScaleRow {
                tenant_id: tenant,
                currency: "TOK".into(),
                currency_scale: scale,
                source: "TENANT".into(),
            })
            .await
            .unwrap();
        if scale == 3 {
            s.set_policy(
                &preparer,
                &scope,
                vec![money("100", "TOK", 3)],
                5,
                604_800,
                OffsetDateTime::now_utc(),
            )
            .await
            .unwrap();
            pending(
                &s,
                &preparer,
                &scope,
                grant(&preparer, money("100", "TOK", 3)),
            )
            .await;
        }
    }
    let row = s
        .list(&preparer, &scope, None, None)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        CurrencyScaleResolver::new(s.reference.clone())
            .resolve(&scope, tenant, "TOK")
            .await
            .unwrap(),
        4
    );
    assert_eq!(
        decode_intent(row.intent.clone()).unwrap().amount().unwrap(),
        Some(money("100", "TOK", 3))
    );
    let snapshot = validate_threshold_snapshot(row.threshold_snapshot.clone()).unwrap();
    assert_eq!(snapshot.d2_threshold.unwrap().currency_scale, 3);
    let policy = s
        .read_effective_policy(&scope, tenant, OffsetDateTime::now_utc())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        policy.policy.d2_thresholds().clone().into_vec(),
        vec![money("100", "TOK", 3)]
    );
    s.approve(&approver, &scope, row.approval_id).await.unwrap();
}
#[tokio::test]
async fn gate_rejects_facts_amount_currency_and_scale_different_from_captured_intent() {
    let (s, _, preparer, _, scope) = setup().await;
    let intent = grant(&preparer, money("1000", "EUR", 2));
    for (amount, variant) in [
        (money("1000", "USD", 2), "currency"),
        (money("1000", "EUR", 3), "scale"),
        (money("1001", "EUR", 2), "amount"),
    ] {
        let mut f = facts(&intent);
        f.amount = Some(amount);
        let result = s
            .gate(&preparer, &scope, intent.clone(), f, "gate".into())
            .await;
        assert!(match variant {
            "currency" => matches!(result, Err(DomainError::CurrencyMismatch(_))),
            "scale" => matches!(result, Err(DomainError::InconsistentScale(_))),
            _ => matches!(result, Err(DomainError::InvalidRequest(_))),
        });
    }
    assert!(
        s.list(&preparer, &scope, None, None)
            .await
            .unwrap()
            .is_empty()
    );
}

/// A rejected policy names what failed: the out-of-range threshold with its
/// currency, scale and bounds, and a scale conflict with both scales.
#[test]
fn policy_config_errors_carry_currency_value_and_bounds() {
    let threshold = money("1", "EUR", 2);
    let out = policy_config_to_domain(PolicyConfigError::D2OutOfRange {
        threshold,
        min: parse_decimal("100").unwrap(),
        max: parse_decimal("1000000").unwrap(),
    });
    match out {
        DomainError::DualControlPolicyOutOfRange(detail) => {
            for needle in ["1 EUR", "scale 2", "[100, 1000000]"] {
                assert!(detail.contains(needle), "{needle}: {detail}");
            }
        }
        other => panic!("unexpected {other:?}"),
    }
    let out = policy_config_to_domain(PolicyConfigError::MetadataConflict {
        currency: "USD".into(),
        configured_scale: 2,
        other_scale: 3,
    });
    match out {
        DomainError::InconsistentScale(detail) => {
            for needle in ["D2 threshold", "USD", "scale 2", "scale 3"] {
                assert!(detail.contains(needle), "{needle}: {detail}");
            }
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// A stored snapshot tampered after creation (resolved threshold no longer what
/// the captured policy yields) is refused on approve: nothing executes and the
/// approval stays PENDING.
#[tokio::test]
async fn approve_refuses_a_tampered_threshold_snapshot_without_executing() {
    use crate::infra::storage::entity::dual_control_approval;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use toolkit_db::secure::SecureUpdateExt;
    let (s, executor, preparer, approver, scope) = setup().await;
    let id = pending(
        &s,
        &preparer,
        &scope,
        grant(&preparer, money("5000", "EUR", 2)),
    )
    .await;
    let row = s.get(&preparer, &scope, id).await.unwrap().unwrap();
    let mut snapshot = row.threshold_snapshot.clone();
    snapshot["d2_threshold"] = serde_json::json!({
        "amount": "999999", "currency": "EUR", "currency_scale": 2
    });
    dual_control_approval::Entity::update_many()
        .col_expr(
            dual_control_approval::Column::ThresholdSnapshot,
            sea_orm::sea_query::Expr::value(snapshot),
        )
        .filter(dual_control_approval::Column::ApprovalId.eq(id))
        .secure()
        .scope_with(&scope)
        .exec(&s.db.conn().unwrap())
        .await
        .unwrap();
    assert!(matches!(
        s.approve(&approver, &scope, id).await,
        Err(DomainError::Internal(_))
    ));
    let executed = { executor.intents.lock().await.len() };
    assert_eq!(executed, 0);
    let after = s.get(&preparer, &scope, id).await.unwrap().unwrap();
    assert_eq!(after.state, ApprovalState::Pending.as_str());
}

/// A monetary intent whose facts omit the amount cannot skip the D2 compare:
/// the gate refuses it and no approval row is created.
#[tokio::test]
async fn gate_rejects_a_monetary_intent_without_amount_facts() {
    let (s, _, preparer, _, scope) = setup().await;
    let intent = grant(&preparer, money("5000", "EUR", 2));
    let mut f = facts(&intent);
    f.amount = None;
    assert!(matches!(
        s.gate(&preparer, &scope, intent, f, "gate".into()).await,
        Err(DomainError::InvalidRequest(_))
    ));
    assert!(
        s.list(&preparer, &scope, None, None)
            .await
            .unwrap()
            .is_empty()
    );
}

/// With a functional currency set and no locked rate snapshot for the
/// operation, a functional-basis kind is valued through the current reference
/// quote at the functional currency's registry scale; an unknown functional
/// scale is a named error, never a guessed one.
#[tokio::test]
async fn functional_valuation_falls_back_to_the_current_quote_at_registry_scale() {
    use crate::infra::storage::repo::NewFxRate;
    let (s, _, preparer, _, scope) = setup().await;
    functional(&s, &preparer, &scope, "JPY").await;
    FxRepo::new(s.db.clone())
        .upsert_rate(&NewFxRate {
            tenant_id: preparer.subject_tenant_id(),
            base_currency: "EUR".into(),
            quote_currency: "JPY".into(),
            provider: "ecb".into(),
            rate: parse_decimal("160.5").unwrap(),
            as_of: OffsetDateTime::now_utc(),
            fallback_order: 0,
        })
        .await
        .unwrap();
    // A credit grant has no operation rate snapshot: the current quote applies.
    let intent = grant(&preparer, money("1000", "EUR", 2));
    let id = pending(&s, &preparer, &scope, intent).await;
    let row = s.get(&preparer, &scope, id).await.unwrap().unwrap();
    assert_eq!(row.amount, Some(money("160500", "JPY", 0)));
    assert_eq!(row.threshold_snapshot["basis"], "functional_gate");
    assert_eq!(row.threshold_snapshot["d2_threshold"]["currency"], "JPY");
    assert_eq!(row.threshold_snapshot["d2_threshold"]["currency_scale"], 0);

    let (s, _, preparer, _, scope) = setup().await;
    functional(&s, &preparer, &scope, "ZZZZ").await;
    let intent = grant(&preparer, money("1000", "EUR", 2));
    assert!(matches!(
        s.gate(
            &preparer,
            &scope,
            intent.clone(),
            facts(&intent),
            "gate".into()
        )
        .await,
        Err(DomainError::InvalidRequest(_))
    ));
}

/// A comment stamped with a revision the approval has moved past (a resubmit
/// between the read and the write) is refused, and the thread is unchanged.
#[tokio::test]
async fn a_comment_at_a_stale_revision_is_refused_and_leaves_the_thread_unchanged() {
    let (s, _, preparer, approver, scope) = setup().await;
    let intent = grant(&preparer, money("5000", "EUR", 2));
    let id = pending(&s, &preparer, &scope, intent.clone()).await;
    s.request_changes(&approver, &scope, id, "fix it".into())
        .await
        .unwrap();
    let stale = s
        .get(&preparer, &scope, id)
        .await
        .unwrap()
        .unwrap()
        .revision;
    s.resubmit(&preparer, &scope, id, intent).await.unwrap();
    let before = s.thread(&preparer, &scope, id).await.unwrap().len();
    assert!(matches!(
        s.append_comment_at(&approver, &scope, id, stale, "late question".into())
            .await,
        Err(DomainError::ApprovalNotActionable(_))
    ));
    assert_eq!(s.thread(&preparer, &scope, id).await.unwrap().len(), before);
    // The public path reads the current revision and succeeds.
    s.add_comment(&approver, &scope, id, "current question".into())
        .await
        .unwrap();
    assert_eq!(
        s.thread(&preparer, &scope, id).await.unwrap().len(),
        before + 1
    );
}

/// A lost DC13 race stops its attempt at once: no retry, no sleep, and the
/// branch after the loop sees the conflict and reads the winner. Contention
/// keeps its retry classification.
#[tokio::test]
async fn a_lost_active_approval_race_is_not_retried() {
    assert!(matches!(
        pending_insert_error(InsertPendingError::ActiveExists),
        AttemptError::Business(DomainError::ConcurrentModification(_))
    ));
    assert!(matches!(
        pending_insert_error(InsertPendingError::Repo(
            crate::domain::model::RepoError::Conflict("approval database contention".into())
        )),
        AttemptError::Conflict
    ));
    assert!(matches!(
        pending_insert_error(InsertPendingError::Repo(
            crate::domain::model::RepoError::Db("io".into())
        )),
        AttemptError::Business(DomainError::Internal(_))
    ));

    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    let result: Result<(), DomainError> = retry_transaction(&db, move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(pending_insert_error(InsertPendingError::ActiveExists)) })
    })
    .await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1, "one attempt only");
    assert!(matches!(
        result,
        Err(DomainError::ConcurrentModification(_))
    ));
}
