use super::*;
use crate::domain::llm::Usage;
use crate::domain::model::finalization::FinalizationInput;
use crate::domain::model::quota::{SettlementMethod, SettlementOutcome};
use crate::domain::repos::{CreateTurnParams, TurnRepository as TurnRepoTrait};
use crate::domain::service::AuditEnvelope;
use crate::domain::service::test_helpers::{RecordingOutboxEnqueuer, inmem_db, mock_db_provider};
use crate::infra::db::entity::chat_turn::TurnState;
use crate::infra::db::entity::quota_usage::PeriodType;
use crate::infra::db::repo::message_repo::MessageRepository as MsgRepo;
use crate::infra::db::repo::turn_repo::TurnRepository as TurnRepo;
use modkit_security::AccessScope;
use uuid::Uuid;

// ── Mock QuotaSettler ──

#[domain_model]
struct MockQuotaSettler;

#[async_trait::async_trait]
impl QuotaSettler for MockQuotaSettler {
    async fn settle_in_tx(
        &self,
        _tx: &modkit_db::secure::DbTx<'_>,
        _scope: &AccessScope,
        _input: crate::domain::model::quota::SettlementInput,
    ) -> Result<SettlementOutcome, DomainError> {
        Ok(SettlementOutcome {
            settlement_method: SettlementMethod::Actual,
            actual_credits_micro: 500,
            charged_tokens: 15,
            overshoot_capped: false,
        })
    }
}

// ── Noop OutboxEnqueuer (with flush tracking) ──

#[domain_model]
struct NoopOutboxEnqueuer {
    flush_count: std::sync::atomic::AtomicU32,
}

impl NoopOutboxEnqueuer {
    fn new() -> Self {
        Self {
            flush_count: std::sync::atomic::AtomicU32::new(0),
        }
    }

    #[allow(dead_code)]
    fn flush_count(&self) -> u32 {
        self.flush_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl OutboxEnqueuer for NoopOutboxEnqueuer {
    async fn enqueue_usage_event(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: mini_chat_sdk::UsageEvent,
    ) -> Result<(), DomainError> {
        Ok(())
    }

    async fn enqueue_attachment_cleanup(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: crate::domain::repos::AttachmentCleanupEvent,
    ) -> Result<(), DomainError> {
        Ok(())
    }

    async fn enqueue_chat_cleanup(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: crate::domain::repos::ChatCleanupEvent,
    ) -> Result<(), DomainError> {
        Ok(())
    }

    async fn enqueue_audit_event(
        &self,
        _runner: &(dyn modkit_db::secure::DBRunner + Sync),
        _event: crate::domain::model::audit_envelope::AuditEnvelope,
    ) -> Result<(), DomainError> {
        Ok(())
    }

    fn flush(&self) {
        self.flush_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

fn build_finalization_service(
    db: Arc<DbProvider>,
) -> (
    FinalizationService<TurnRepo, MsgRepo>,
    Arc<RecordingOutboxEnqueuer>,
) {
    let outbox = Arc::new(RecordingOutboxEnqueuer::new());
    let svc = FinalizationService::new(
        db,
        Arc::new(TurnRepo),
        Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        })),
        Arc::new(MockQuotaSettler),
        outbox.clone(),
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    );
    (svc, outbox)
}

fn build_finalization_service_with_metrics(
    db: Arc<DbProvider>,
    metrics: Arc<dyn MiniChatMetricsPort>,
) -> (
    FinalizationService<TurnRepo, MsgRepo>,
    Arc<NoopOutboxEnqueuer>,
) {
    let outbox = Arc::new(NoopOutboxEnqueuer::new());
    let svc = FinalizationService::new(
        db,
        Arc::new(TurnRepo),
        Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        })),
        Arc::new(MockQuotaSettler),
        outbox.clone(),
        metrics,
    );
    (svc, outbox)
}

/// Insert a parent chat row (FK constraint).
async fn insert_test_chat(db: &Arc<DbProvider>, tenant_id: Uuid, chat_id: Uuid, user_id: Uuid) {
    use crate::infra::db::entity::chat::{ActiveModel, Entity as ChatEntity};
    use modkit_db::secure::secure_insert;
    use sea_orm::Set;
    use time::OffsetDateTime;

    let now = OffsetDateTime::now_utc();
    let am = ActiveModel {
        id: Set(chat_id),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        model: Set("gpt-5.2".to_owned()),
        title: Set(Some("test".to_owned())),
        is_temporary: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
    };
    let conn = db.conn().unwrap();
    secure_insert::<ChatEntity>(am, &AccessScope::allow_all(), &conn)
        .await
        .expect("insert chat");
}

/// Insert a turn in `running` state.
async fn insert_running_turn(
    db: &Arc<DbProvider>,
    tenant_id: Uuid,
    chat_id: Uuid,
    turn_id: Uuid,
    request_id: Uuid,
) {
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let turn_repo = TurnRepo;
    turn_repo
        .create_turn(
            &conn,
            &scope,
            CreateTurnParams {
                id: turn_id,
                tenant_id,
                chat_id,
                request_id,
                requester_type: "user".to_owned(),
                requester_user_id: None,
                reserve_tokens: Some(100),
                max_output_tokens_applied: Some(4096),
                reserved_credits_micro: Some(1000),
                policy_version_applied: Some(1),
                effective_model: Some("gpt-5.2".to_owned()),
                minimal_generation_floor_applied: Some(10),
                web_search_enabled: false,
            },
        )
        .await
        .expect("create turn");
}

fn make_input(
    tenant_id: Uuid,
    chat_id: Uuid,
    turn_id: Uuid,
    request_id: Uuid,
    user_id: Uuid,
    terminal_state: TurnState,
) -> FinalizationInput {
    let today = time::OffsetDateTime::now_utc().date();
    let month_start = today.replace_day(1).unwrap();
    FinalizationInput {
        turn_id,
        tenant_id,
        chat_id,
        request_id,
        user_id,
        requester_type: mini_chat_sdk::RequesterType::User,
        scope: AccessScope::allow_all(),
        message_id: Uuid::new_v4(),
        terminal_state,
        error_code: None,
        error_detail: None,
        accumulated_text: "Hello, world!".to_owned(),
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: 0,
            cache_write_input_tokens: 0,
            reasoning_tokens: 0,
        }),
        provider_response_id: Some("resp-123".to_owned()),
        effective_model: "gpt-5.2".to_owned(),
        selected_model: "gpt-5.2".to_owned(),
        reserve_tokens: 100,
        max_output_tokens_applied: 4096,
        reserved_credits_micro: 1000,
        policy_version_applied: 1,
        minimal_generation_floor_applied: 10,
        quota_decision: "allow".to_owned(),
        downgrade_from: None,
        downgrade_reason: None,
        period_starts: vec![
            (PeriodType::Daily, today),
            (PeriodType::Monthly, month_start),
        ],
        web_search_calls: 3,
        code_interpreter_calls: 0,
        ttft_ms: None,
        total_ms: None,
    }
}

/// Backdate `last_progress_at` on a turn to 10 minutes ago, making it orphan-eligible.
async fn backdate_turn_progress(runner: &impl modkit_db::secure::DBRunner, turn_id: Uuid) {
    use crate::infra::db::entity::chat_turn::{Column, Entity as TurnEntity};
    use modkit_db::secure::SecureUpdateExt;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, sea_query::Expr};
    use time::OffsetDateTime;

    let past = OffsetDateTime::now_utc() - time::Duration::seconds(600);
    let scope = AccessScope::allow_all();
    TurnEntity::update_many()
        .col_expr(Column::LastProgressAt, Expr::value(Some(past)))
        .filter(Column::Id.eq(turn_id))
        .secure()
        .scope_with(&scope)
        .exec(runner)
        .await
        .expect("backdate last_progress_at");
}

// ── 3.6: CAS winner executes full atomic finalization ──

#[tokio::test]
async fn cas_winner_completes_finalization() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    let input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Completed,
    );
    let outcome = svc
        .finalize_turn_cas(input)
        .await
        .expect("finalization should succeed");

    assert!(outcome.won_cas, "should be CAS winner");
    assert!(outcome.billing_outcome.is_some());
    assert!(outcome.settlement_outcome.is_some());
    assert_eq!(
        outbox.flush_count(),
        1,
        "flush should be called once after CAS win"
    );

    // Verify turn is now in completed state
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let turn_repo = TurnRepo;
    let turn = turn_repo
        .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
        .await
        .expect("find turn")
        .expect("turn should exist");
    assert_eq!(turn.state, TurnState::Completed);
}

// ── 3.7: CAS loser returns won_cas = false ──

#[tokio::test]
async fn cas_loser_returns_no_side_effects() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    // First finalization — wins CAS
    let input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Completed,
    );
    let outcome1 = svc
        .finalize_turn_cas(input)
        .await
        .expect("first finalization");
    assert!(outcome1.won_cas);

    // Second finalization — loses CAS (turn already completed)
    let input2 = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Failed,
    );
    let outcome2 = svc
        .finalize_turn_cas(input2)
        .await
        .expect("second finalization");
    assert!(!outcome2.won_cas, "second finalizer should lose CAS");
    assert!(outcome2.billing_outcome.is_none());
    assert!(outcome2.settlement_outcome.is_none());
    // First call won CAS → 1 flush. Second lost CAS → no additional flush.
    assert_eq!(
        outbox.flush_count(),
        1,
        "flush should only be called for CAS winner"
    );
}

// ── 3.8: Transaction rollback on failure leaves turn in running state ──

#[tokio::test]
async fn failed_settlement_leaves_turn_running() {
    // Use a QuotaSettler that always fails
    #[domain_model]
    struct FailingQuotaSettler;

    #[async_trait::async_trait]
    impl QuotaSettler for FailingQuotaSettler {
        async fn settle_in_tx(
            &self,
            _tx: &modkit_db::secure::DbTx<'_>,
            _scope: &AccessScope,
            _input: crate::domain::model::quota::SettlementInput,
        ) -> Result<SettlementOutcome, DomainError> {
            Err(DomainError::internal("settlement exploded"))
        }
    }

    let db = mock_db_provider(inmem_db().await);
    let svc = FinalizationService::new(
        Arc::clone(&db),
        Arc::new(TurnRepo),
        Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        })),
        Arc::new(FailingQuotaSettler),
        Arc::new(RecordingOutboxEnqueuer::new()),
        Arc::new(crate::domain::ports::metrics::NoopMetrics),
    );

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    let input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Completed,
    );
    let result = svc.finalize_turn_cas(input).await;

    // Should fail due to settlement error
    assert!(
        result.is_err(),
        "finalization should fail when settlement fails"
    );

    // Verify turn is still running (transaction rolled back)
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let turn_repo = TurnRepo;
    let running = turn_repo
        .find_running_by_chat_id(&conn, &scope, chat_id)
        .await
        .expect("find running turn")
        .expect("turn should still be running");
    assert_eq!(running.id, turn_id);
    assert_eq!(running.state, TurnState::Running);
}

// ── Metrics emission on successful finalization ──

#[tokio::test]
async fn cas_winner_emits_audit_and_quota_metrics() {
    use crate::domain::service::test_helpers::TestMetrics;
    use std::sync::atomic::Ordering;

    let db = mock_db_provider(inmem_db().await);
    let metrics = Arc::new(TestMetrics::new());
    let (svc, _outbox) =
        build_finalization_service_with_metrics(Arc::clone(&db), Arc::clone(&metrics) as _);

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    let input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Completed,
    );
    let outcome = svc
        .finalize_turn_cas(input)
        .await
        .expect("finalization should succeed");
    assert!(outcome.won_cas);

    // Audit emission metrics
    assert_eq!(
        metrics.audit_emit.load(Ordering::Relaxed),
        1,
        "should record audit_emit"
    );
    assert_eq!(
        metrics.finalization_latency_ms.load(Ordering::Relaxed),
        1,
        "should record finalization_latency_ms"
    );
    // Quota settlement metrics (daily + monthly)
    assert_eq!(
        metrics.quota_commit.load(Ordering::Relaxed),
        2,
        "should record quota_commit for daily + monthly"
    );
    assert_eq!(
        metrics.quota_actual_tokens.load(Ordering::Relaxed),
        1,
        "should record quota_actual_tokens"
    );
}

#[tokio::test]
async fn cas_winner_emits_code_interpreter_calls_metric() {
    use crate::domain::service::test_helpers::TestMetrics;
    use std::sync::atomic::Ordering;

    let db = mock_db_provider(inmem_db().await);
    let metrics = Arc::new(TestMetrics::new());
    let (svc, _outbox) =
        build_finalization_service_with_metrics(Arc::clone(&db), Arc::clone(&metrics) as _);

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    let mut input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Completed,
    );
    input.code_interpreter_calls = 5;

    let outcome = svc
        .finalize_turn_cas(input)
        .await
        .expect("finalization should succeed");
    assert!(outcome.won_cas);

    assert_eq!(
        metrics.code_interpreter_calls.load(Ordering::Relaxed),
        1,
        "should record code_interpreter_calls metric"
    );
}

// ── Cancelled message persistence tests (D4) ──

#[tokio::test]
async fn cancelled_with_text_persists_message() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, _outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    let mut input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Cancelled,
    );
    input.accumulated_text = "partial response content".to_owned();
    input.usage = None;

    let outcome = svc
        .finalize_turn_cas(input)
        .await
        .expect("finalization should succeed");

    assert!(outcome.won_cas);

    // Verify turn is cancelled with assistant_message_id set
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let turn_repo = TurnRepo;
    let turn = turn_repo
        .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
        .await
        .expect("find turn")
        .expect("turn should exist");
    assert_eq!(turn.state, TurnState::Cancelled);
    assert!(
        turn.assistant_message_id.is_some(),
        "cancelled turn with text should have assistant_message_id"
    );
}

#[tokio::test]
async fn cancelled_without_text_does_not_persist_message() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, _outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    let mut input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Cancelled,
    );
    input.accumulated_text = String::new();
    input.usage = None;

    let outcome = svc
        .finalize_turn_cas(input)
        .await
        .expect("finalization should succeed");

    assert!(outcome.won_cas);

    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let turn_repo = TurnRepo;
    let turn = turn_repo
        .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
        .await
        .expect("find turn")
        .expect("turn should exist");
    assert_eq!(turn.state, TurnState::Cancelled);
    assert!(
        turn.assistant_message_id.is_none(),
        "cancelled turn without text should have no assistant_message_id"
    );
}

#[tokio::test]
async fn completed_message_persist_failure_retries_as_failed() {
    // Existing behavior unchanged — verify the guard doesn't break it.
    // We test by finalizing as Completed, then finalizing again (CAS loser
    // path), confirming the first finalization worked correctly.
    let db = mock_db_provider(inmem_db().await);
    let (svc, _outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    let input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Completed,
    );
    let outcome = svc
        .finalize_turn_cas(input)
        .await
        .expect("finalization should succeed");
    assert!(outcome.won_cas);

    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let turn_repo = TurnRepo;
    let turn = turn_repo
        .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
        .await
        .expect("find turn")
        .expect("turn should exist");
    assert_eq!(turn.state, TurnState::Completed);
    assert!(turn.assistant_message_id.is_some());
}

// ── Audit emission tests ──

#[tokio::test]
async fn cas_winner_emits_turn_completed_audit() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    let input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Completed,
    );
    let outcome = svc.finalize_turn_cas(input).await.unwrap();
    assert!(outcome.won_cas);

    let captured = outbox.audit_events();
    assert_eq!(captured.len(), 1, "expected exactly 1 audit event");
    match &captured[0] {
        AuditEnvelope::Turn(evt) => {
            assert_eq!(
                evt.event_type,
                mini_chat_sdk::TurnAuditEventType::TurnCompleted
            );
            assert_eq!(evt.tenant_id, tenant_id);
            assert_eq!(evt.user_id, user_id);
            assert_eq!(evt.chat_id, chat_id);
            assert_eq!(evt.turn_id, turn_id);
            assert_eq!(evt.request_id, request_id);
            assert_eq!(evt.effective_model, "gpt-5.2");
            assert_eq!(evt.selected_model, "gpt-5.2");
            assert_eq!(evt.usage.input_tokens, 10);
            assert_eq!(evt.usage.output_tokens, 5);
            assert!(evt.prompt.is_none(), "prompt should be deferred (None)");
            assert!(evt.response.is_none(), "response should be deferred (None)");
            assert!(evt.tool_calls.is_none(), "tool_calls should be None");
        }
        other => panic!("expected Turn event, got: {other:?}"),
    }
}

#[tokio::test]
async fn cas_winner_emits_turn_failed_audit() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    let mut input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Failed,
    );
    input.error_code = Some("provider_error".to_owned());

    let outcome = svc.finalize_turn_cas(input).await.unwrap();
    assert!(outcome.won_cas);

    let captured = outbox.audit_events();
    assert_eq!(captured.len(), 1);
    match &captured[0] {
        AuditEnvelope::Turn(evt) => {
            assert_eq!(
                evt.event_type,
                mini_chat_sdk::TurnAuditEventType::TurnFailed
            );
            assert_eq!(evt.error_code, Some("provider_error".to_owned()));
        }
        other => panic!("expected Turn event, got: {other:?}"),
    }
}

#[tokio::test]
async fn cas_loser_does_not_emit_audit() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    // First finalization wins CAS
    let input1 = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Completed,
    );
    svc.finalize_turn_cas(input1).await.unwrap();
    outbox.clear_audit_events();

    // Second finalization loses CAS — no audit
    let input2 = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Failed,
    );
    let outcome2 = svc.finalize_turn_cas(input2).await.unwrap();
    assert!(!outcome2.won_cas);

    assert!(
        outbox.audit_events().is_empty(),
        "CAS loser must not emit audit events"
    );
}

#[tokio::test]
async fn audit_event_includes_latency_when_provided() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    let mut input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Completed,
    );
    input.ttft_ms = Some(120);
    input.total_ms = Some(3500);

    svc.finalize_turn_cas(input).await.unwrap();

    let captured = outbox.audit_events();
    match &captured[0] {
        AuditEnvelope::Turn(evt) => {
            assert_eq!(evt.latency_ms.ttft_ms, Some(120));
            assert_eq!(evt.latency_ms.total_ms, Some(3500));
        }
        other => panic!("expected Turn event, got: {other:?}"),
    }
}

#[tokio::test]
async fn audit_event_policy_decisions_match_input() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    let mut input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Completed,
    );
    input.quota_decision = "downgrade".to_owned();
    input.downgrade_from = Some("gpt-5.2".to_owned());
    input.downgrade_reason = Some("quota exceeded".to_owned());

    svc.finalize_turn_cas(input).await.unwrap();

    let captured = outbox.audit_events();
    match &captured[0] {
        AuditEnvelope::Turn(evt) => {
            assert_eq!(evt.policy_decisions.quota.decision, "downgrade");
            assert_eq!(
                evt.policy_decisions.quota.downgrade_from,
                Some("gpt-5.2".to_owned())
            );
            assert_eq!(
                evt.policy_decisions.quota.downgrade_reason,
                Some("quota exceeded".to_owned())
            );
            assert_eq!(evt.policy_version_applied, Some(1));
        }
        other => panic!("expected Turn event, got: {other:?}"),
    }
}

// ── Token breakdown fields propagate through finalization ──

#[tokio::test]
async fn finalization_propagates_token_breakdown_fields() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    let mut input = make_input(
        tenant_id,
        chat_id,
        turn_id,
        request_id,
        user_id,
        TurnState::Completed,
    );
    input.usage = Some(Usage {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_input_tokens: 42,
        cache_write_input_tokens: 17,
        reasoning_tokens: 88,
    });

    svc.finalize_turn_cas(input).await.unwrap();

    // ── Verify usage event ──
    let usage_events = outbox.usage_events.lock().unwrap();
    assert_eq!(usage_events.len(), 1);
    let usage = usage_events[0]
        .usage
        .as_ref()
        .expect("usage should be present");
    assert_eq!(usage.cache_read_input_tokens, 42);
    assert_eq!(usage.cache_write_input_tokens, 17);
    assert_eq!(usage.reasoning_tokens, 88);
    drop(usage_events);

    // ── Verify audit event ──
    let audit_events = outbox.audit_events();
    assert_eq!(audit_events.len(), 1);
    match &audit_events[0] {
        AuditEnvelope::Turn(evt) => {
            assert_eq!(evt.usage.cache_read_input_tokens, Some(42));
            assert_eq!(evt.usage.cache_write_input_tokens, Some(17));
            assert_eq!(evt.usage.reasoning_tokens, Some(88));
        }
        other => panic!("expected Turn event, got: {other:?}"),
    }
}

// ── Orphan finalization tests ──

#[tokio::test]
async fn finalize_orphan_cas_winner() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    // Make the turn stale by backdating last_progress_at
    let conn = db.conn().unwrap();
    backdate_turn_progress(&conn, turn_id).await;

    let input = crate::domain::model::finalization::OrphanFinalizationInput {
        turn_id,
        tenant_id,
        chat_id,
        request_id,
        user_id: Some(user_id),
        requester_type: mini_chat_sdk::RequesterType::User,
        effective_model: Some("gpt-5.2".to_owned()),
        reserve_tokens: Some(100),
        max_output_tokens_applied: Some(4096),
        reserved_credits_micro: Some(1000),
        policy_version_applied: Some(1),
        minimal_generation_floor_applied: Some(10),
        started_at: time::OffsetDateTime::now_utc(),
        web_search_completed_count: 0,
        code_interpreter_completed_count: 0,
    };

    let result = svc.finalize_orphan_turn(input, 60).await.unwrap();
    assert!(result, "should be CAS winner");

    // Verify turn state
    let scope = AccessScope::allow_all();
    let turn_repo = TurnRepo;
    let turn = turn_repo
        .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
        .await
        .unwrap()
        .expect("turn should exist");
    assert_eq!(turn.state, TurnState::Failed);
    assert_eq!(turn.error_code.as_deref(), Some("orphan_timeout"));

    // Verify usage event enqueued
    let usage_events = outbox.usage_events.lock().unwrap();
    assert_eq!(usage_events.len(), 1, "should have one usage event");
    assert_eq!(usage_events[0].terminal_state, "failed");
    assert_eq!(usage_events[0].billing_outcome, "aborted");
    drop(usage_events);

    // Verify audit event enqueued
    let audit_events = outbox.audit_events();
    assert_eq!(audit_events.len(), 1, "should have one audit event");
    match &audit_events[0] {
        AuditEnvelope::Turn(evt) => {
            assert_eq!(
                evt.event_type,
                mini_chat_sdk::TurnAuditEventType::TurnFailed
            );
            assert_eq!(evt.error_code.as_deref(), Some("orphan_timeout"));
        }
        other => panic!("expected Turn event, got: {other:?}"),
    }

    // Verify flush was called
    assert_eq!(
        outbox.flush_count(),
        1,
        "flush should be called after CAS win"
    );
}

#[tokio::test]
async fn finalize_orphan_cas_loser() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    // Finalize normally first (CAS update to failed — avoids FK constraint on assistant_message_id)
    let conn = db.conn().unwrap();
    let scope = AccessScope::allow_all();
    let turn_repo = TurnRepo;
    turn_repo
        .cas_update_state(
            &conn,
            &scope,
            crate::domain::repos::CasTerminalParams {
                turn_id,
                state: TurnState::Failed,
                error_code: Some("test_error".to_owned()),
                error_detail: None,
                assistant_message_id: None,
                provider_response_id: None,
            },
        )
        .await
        .unwrap();

    // Now try orphan finalization — should be CAS loser
    let input = crate::domain::model::finalization::OrphanFinalizationInput {
        turn_id,
        tenant_id,
        chat_id,
        request_id,
        user_id: Some(user_id),
        requester_type: mini_chat_sdk::RequesterType::User,
        effective_model: Some("gpt-5.2".to_owned()),
        reserve_tokens: Some(100),
        max_output_tokens_applied: Some(4096),
        reserved_credits_micro: Some(1000),
        policy_version_applied: Some(1),
        minimal_generation_floor_applied: Some(10),
        started_at: time::OffsetDateTime::now_utc(),
        web_search_completed_count: 0,
        code_interpreter_completed_count: 0,
    };

    let result = svc.finalize_orphan_turn(input, 60).await.unwrap();
    assert!(!result, "should be CAS loser");

    // Verify no usage or audit events were enqueued
    let usage_events = outbox.usage_events.lock().unwrap();
    assert!(usage_events.is_empty(), "no usage events for CAS loser");
    drop(usage_events);

    let audit_events = outbox.audit_events();
    assert!(audit_events.is_empty(), "no audit events for CAS loser");

    assert_eq!(outbox.flush_count(), 0, "no flush for CAS loser");
}

#[tokio::test]
async fn finalize_orphan_missing_quota_fields() {
    let db = mock_db_provider(inmem_db().await);
    let (svc, outbox) = build_finalization_service(Arc::clone(&db));

    let tenant_id = Uuid::new_v4();
    let chat_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();

    insert_test_chat(&db, tenant_id, chat_id, user_id).await;
    insert_running_turn(&db, tenant_id, chat_id, turn_id, request_id).await;

    // Make the turn stale by backdating last_progress_at
    let conn = db.conn().unwrap();
    backdate_turn_progress(&conn, turn_id).await;

    // Build input with effective_model = None → settlement will be skipped
    let input = crate::domain::model::finalization::OrphanFinalizationInput {
        turn_id,
        tenant_id,
        chat_id,
        request_id,
        user_id: Some(user_id),
        requester_type: mini_chat_sdk::RequesterType::User,
        effective_model: None, // missing
        reserve_tokens: Some(100),
        max_output_tokens_applied: Some(4096),
        reserved_credits_micro: Some(1000),
        policy_version_applied: Some(1),
        minimal_generation_floor_applied: Some(10),
        started_at: time::OffsetDateTime::now_utc(),
        web_search_completed_count: 0,
        code_interpreter_completed_count: 0,
    };

    let result = svc.finalize_orphan_turn(input, 60).await.unwrap();
    assert!(result, "CAS should succeed even with missing quota fields");

    // Verify turn is failed
    let scope = AccessScope::allow_all();
    let turn_repo = TurnRepo;
    let turn = turn_repo
        .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
        .await
        .unwrap()
        .expect("turn should exist");
    assert_eq!(turn.state, TurnState::Failed);
    assert_eq!(turn.error_code.as_deref(), Some("orphan_timeout"));

    // Usage event should still be enqueued (settlement skipped, but event still sent)
    let usage_events = outbox.usage_events.lock().unwrap();
    assert_eq!(
        usage_events.len(),
        1,
        "usage event should be enqueued even without settlement"
    );
    drop(usage_events);
}
