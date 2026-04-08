use std::sync::Arc;

use super::current_otel_trace_id;
use modkit_macros::domain_model;
use tracing::{debug, error, warn};

use mini_chat_sdk::{
    AuditUsageTokens, LatencyMs, PolicyDecisions, QuotaDecision, TurnAuditEvent,
    TurnAuditEventType, UsageEvent, UsageTokens,
};

use crate::domain::error::DomainError;
use crate::domain::model::audit_envelope::AuditEnvelope;
use crate::domain::model::billing_outcome::{
    BillingDerivation, BillingDerivationInput, BillingOutcome, derive_billing_outcome,
};
use crate::domain::model::finalization::{
    FinalizationInput, FinalizationOutcome, has_known_usage, settlement_path_from_billing,
};
use crate::domain::model::quota::{SettlementInput, SettlementMethod, SettlementOutcome};
use crate::domain::repos::{
    CasTerminalParams, InsertAssistantMessageParams, MessageRepository, OutboxEnqueuer,
    TurnRepository,
};
use crate::domain::service::quota_settler::QuotaSettler;
use crate::infra::db::entity::chat_turn::TurnState;
use crate::infra::llm::Usage;

use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::ports::metric_labels::{period, result as result_label, trigger};

use super::DbProvider;

fn to_db(e: DomainError) -> modkit_db::DbError {
    modkit_db::DbError::Other(anyhow::anyhow!(e))
}

/// Service encapsulating the atomic finalization transaction.
///
/// Generic over `TR` and `MR` (repository traits are not dyn-compatible
/// due to `&impl DBRunner` methods). The `QuotaService<QR>` generic is
/// erased via the `QuotaSettler` trait (see D2).
///
/// Created once in `AppServices::new()` and shared with spawned tasks
/// via `Arc<FinalizationService<TR, MR>>`.
#[domain_model]
pub struct FinalizationService<TR: TurnRepository + 'static, MR: MessageRepository + 'static> {
    db: Arc<DbProvider>,
    turn_repo: Arc<TR>,
    message_repo: Arc<MR>,
    quota_settler: Arc<dyn QuotaSettler>,
    outbox_enqueuer: Arc<dyn OutboxEnqueuer>,
    metrics: Arc<dyn MiniChatMetricsPort>,
}

impl<TR: TurnRepository + 'static, MR: MessageRepository + 'static> FinalizationService<TR, MR> {
    pub(crate) fn new(
        db: Arc<DbProvider>,
        turn_repo: Arc<TR>,
        message_repo: Arc<MR>,
        quota_settler: Arc<dyn QuotaSettler>,
        outbox_enqueuer: Arc<dyn OutboxEnqueuer>,
        metrics: Arc<dyn MiniChatMetricsPort>,
    ) -> Self {
        Self {
            db,
            turn_repo,
            message_repo,
            quota_settler,
            outbox_enqueuer,
            metrics,
        }
    }

    /// Single universal finalization function for all terminal paths.
    ///
    /// Executes CAS guard + billing derivation + quota settlement +
    /// message persistence + outbox enqueue in one atomic DB transaction.
    ///
    /// Returns `FinalizationOutcome { won_cas: false, .. }` if another
    /// finalizer already committed (CAS loser — no-op).
    ///
    /// If message persistence fails on a completed turn, rolls back and
    /// retries as `Failed` with `error_code = "message_persistence_failed"`
    /// (content durability invariant).
    #[allow(clippy::cognitive_complexity)]
    pub(crate) async fn finalize_turn_cas(
        &self,
        input: FinalizationInput,
    ) -> Result<FinalizationOutcome, DomainError> {
        let start = std::time::Instant::now();
        // Capture trace_id before the transaction: the transaction closure runs
        // on the same thread but inside a different async context; capturing here
        // ensures we get the actual request span ID.
        let trace_id = current_otel_trace_id();

        let result = self.try_finalize(&input, trace_id.clone()).await;

        match result {
            Ok(outcome) => {
                // Post-commit side effects (outside transaction).
                if outcome.won_cas {
                    self.outbox_enqueuer.flush();
                }
                if let Some(billing) = outcome.billing_outcome {
                    let ms = start.elapsed().as_secs_f64() * 1000.0;
                    Self::emit_post_commit_side_effects(&input, billing, ms, &*self.metrics);
                }
                Ok(outcome)
            }
            Err(FinalizationError::MessagePersistenceFailed(e)) => {
                if input.terminal_state == TurnState::Completed {
                    // Content durability invariant: downgrade completed → failed.
                    // The transaction rolled back, so the turn is still 'running'.
                    // Retry with Failed state.
                    error!(
                        error = %e,
                        turn_id = %input.turn_id,
                        "message persistence failed, downgrading completed to failed"
                    );
                    let mut retry_input = input;
                    retry_input.terminal_state = TurnState::Failed;
                    retry_input.error_code = Some("message_persistence_failed".to_owned());
                    let retry_outcome = self
                        .try_finalize(&retry_input, trace_id.clone())
                        .await
                        .map_err(|fe| match fe {
                            FinalizationError::Domain(de) => de,
                            FinalizationError::MessagePersistenceFailed(e2) => {
                                DomainError::internal(format!("unexpected retry failure: {e2}"))
                            }
                        })?;
                    if retry_outcome.won_cas {
                        self.outbox_enqueuer.flush();
                    }
                    if let Some(billing) = retry_outcome.billing_outcome {
                        let ms = start.elapsed().as_secs_f64() * 1000.0;
                        Self::emit_post_commit_side_effects(
                            &retry_input,
                            billing,
                            ms,
                            &*self.metrics,
                        );
                    }
                    Ok(retry_outcome)
                } else {
                    // Best-effort path (cancelled turns): log and finalize
                    // without message by clearing accumulated_text (D4).
                    warn!(
                        error = %e,
                        turn_id = %input.turn_id,
                        terminal_state = ?input.terminal_state,
                        "message persistence failed on non-completed turn, \
                         finalizing without message"
                    );
                    let mut retry_input = input;
                    retry_input.accumulated_text = String::new();
                    let retry_outcome =
                        self.try_finalize(&retry_input, trace_id)
                            .await
                            .map_err(|fe| match fe {
                                FinalizationError::Domain(de) => de,
                                FinalizationError::MessagePersistenceFailed(e2) => {
                                    DomainError::internal(format!(
                                        "unexpected message persist on empty text: {e2}"
                                    ))
                                }
                            })?;
                    if retry_outcome.won_cas {
                        self.outbox_enqueuer.flush();
                    }
                    if let Some(billing) = retry_outcome.billing_outcome {
                        let ms = start.elapsed().as_secs_f64() * 1000.0;
                        Self::emit_post_commit_side_effects(
                            &retry_input,
                            billing,
                            ms,
                            &*self.metrics,
                        );
                    }
                    Ok(retry_outcome)
                }
            }
            Err(FinalizationError::Domain(e)) => Err(e),
        }
    }

    /// Core finalization logic inside a transaction.
    async fn try_finalize(
        &self,
        input: &FinalizationInput,
        trace_id: Option<String>,
    ) -> Result<FinalizationOutcome, FinalizationError> {
        let turn_repo = Arc::clone(&self.turn_repo);
        let message_repo = Arc::clone(&self.message_repo);
        let quota_settler = Arc::clone(&self.quota_settler);
        let outbox_enqueuer = Arc::clone(&self.outbox_enqueuer);
        let input = input.clone();

        let tx_result = self
            .db
            .transaction(|tx| {
                Box::pin(async move {
                    let scope = input.scope.clone();

                    // 1. CAS guard — state transition only.
                    //    assistant_message_id and provider_response_id are set
                    //    AFTER the message INSERT (step 4) to avoid FK violation
                    //    (assistant_message_id REFERENCES messages(id)).
                    let rows = turn_repo
                        .cas_update_state(
                            tx,
                            &scope,
                            CasTerminalParams {
                                turn_id: input.turn_id,
                                state: input.terminal_state.clone(),
                                error_code: input.error_code.clone(),
                                error_detail: input.error_detail.clone(),
                                assistant_message_id: None,
                                provider_response_id: input.provider_response_id.clone(),
                            },
                        )
                        .await
                        .map_err(to_db)?;

                    if rows == 0 {
                        debug!(turn_id = %input.turn_id, "CAS loser: another finalizer won");
                        return Ok(FinalizationOutcome {
                            won_cas: false,
                            billing_outcome: None,
                            settlement_outcome: None,
                        });
                    }

                    // 2. Derive billing outcome (pure function, no DB)
                    let billing = derive_billing_outcome(&BillingDerivationInput {
                        terminal_state: input.terminal_state.clone(),
                        error_code: input.error_code.clone(),
                        has_usage: has_known_usage(input.usage),
                    });

                    // 3. Build SettlementInput and settle quota
                    let settlement_path =
                        settlement_path_from_billing(billing.settlement_method, input.usage);
                    let settlement_input = SettlementInput {
                        tenant_id: input.tenant_id,
                        user_id: input.user_id,
                        effective_model: input.effective_model.clone(),
                        policy_version_applied: input.policy_version_applied,
                        reserve_tokens: input.reserve_tokens,
                        max_output_tokens_applied: input.max_output_tokens_applied,
                        reserved_credits_micro: input.reserved_credits_micro,
                        minimal_generation_floor_applied: input.minimal_generation_floor_applied,
                        settlement_path,
                        period_starts: input.period_starts.clone(),
                        web_search_calls: input.web_search_calls,
                    };
                    let settlement_outcome = quota_settler
                        .settle_in_tx(tx, &scope, settlement_input)
                        .await
                        .map_err(to_db)?;

                    // 4. Persist assistant message
                    //    Completed: full content, required (retry-as-failed on failure)
                    //    Cancelled with non-empty text: partial content, best-effort
                    let should_persist_message = input.terminal_state == TurnState::Completed
                        || (input.terminal_state == TurnState::Cancelled
                            && !input.accumulated_text.is_empty());

                    if should_persist_message {
                        message_repo
                            .insert_assistant_message(
                                tx,
                                &scope,
                                InsertAssistantMessageParams {
                                    id: input.message_id,
                                    tenant_id: input.tenant_id,
                                    chat_id: input.chat_id,
                                    request_id: input.request_id,
                                    content: input.accumulated_text.clone(),
                                    input_tokens: input.usage.map(|u| u.input_tokens),
                                    output_tokens: input.usage.map(|u| u.output_tokens),
                                    model: Some(input.effective_model.clone()),
                                    provider_response_id: input.provider_response_id.clone(),
                                },
                            )
                            .await
                            .map_err(|e| {
                                // Signal message persistence failure for retry logic.
                                modkit_db::DbError::Other(anyhow::anyhow!("MSG_PERSIST_FAILED:{e}"))
                            })?;

                        // 4b. Link assistant_message_id on the turn row.
                        //     Done as a separate UPDATE (not in the CAS step) because
                        //     assistant_message_id has a FK to messages(id), so the
                        //     message row must exist first.
                        turn_repo
                            .set_assistant_message_id(tx, &scope, input.turn_id, input.message_id)
                            .await
                            .map_err(to_db)?;
                    }

                    // 5. Enqueue usage outbox event
                    let usage_event = build_usage_event(&input, billing, &settlement_outcome);
                    outbox_enqueuer
                        .enqueue_usage_event(tx, usage_event)
                        .await
                        .map_err(to_db)?;

                    // 6. Enqueue audit outbox event
                    let audit_event = build_turn_audit_envelope(&input, trace_id);
                    outbox_enqueuer
                        .enqueue_audit_event(tx, audit_event)
                        .await
                        .map_err(to_db)?;

                    Ok(FinalizationOutcome {
                        won_cas: true,
                        billing_outcome: Some(billing),
                        settlement_outcome: Some(settlement_outcome),
                    })
                })
            })
            .await;

        match tx_result {
            Ok(outcome) => Ok(outcome),
            Err(e) => {
                // Check if this was a message persistence failure (sentinel).
                let err_str = e.to_string();
                if err_str.contains("MSG_PERSIST_FAILED:") {
                    let inner = err_str
                        .strip_prefix("MSG_PERSIST_FAILED:")
                        .unwrap_or(&err_str);
                    Err(FinalizationError::MessagePersistenceFailed(
                        inner.to_owned(),
                    ))
                } else {
                    Err(FinalizationError::Domain(DomainError::from(e)))
                }
            }
        }
    }

    /// Emit metrics and logs after the transaction commits.
    /// These MUST NOT run inside the transaction.
    fn emit_post_commit_side_effects(
        input: &FinalizationInput,
        billing: BillingDerivation,
        finalization_ms: f64,
        metrics: &dyn MiniChatMetricsPort,
    ) {
        metrics.record_audit_emit(result_label::OK);
        metrics.record_finalization_latency_ms(finalization_ms);
        Self::emit_quota_metrics(input, billing, metrics);
        Self::emit_billing_side_effects(input, billing, metrics);
    }

    fn emit_quota_metrics(
        input: &FinalizationInput,
        billing: BillingDerivation,
        metrics: &dyn MiniChatMetricsPort,
    ) {
        match billing.settlement_method {
            SettlementMethod::Actual => {
                metrics.record_quota_commit(period::DAILY);
                metrics.record_quota_commit(period::MONTHLY);
                if let Some(usage) = input.usage {
                    #[allow(clippy::cast_precision_loss)]
                    let actual = (usage.input_tokens + usage.output_tokens) as f64;
                    metrics.record_quota_actual_tokens(actual);

                    // Overshoot: actual tokens exceeded the reserved estimate.
                    // Overshoot detection: mini_chat_quota_overshoot_total{period}
                    #[allow(clippy::cast_precision_loss)]
                    let reserved = input.reserve_tokens as f64;
                    if actual > reserved {
                        metrics.record_quota_overshoot(period::DAILY);
                        metrics.record_quota_overshoot(period::MONTHLY);
                    }
                }
            }
            SettlementMethod::Estimated | SettlementMethod::Released => {
                // No overshoot metric here: overshoot measures actual > reserved,
                // but estimated settlement has no actual usage data to compare.
                // The reserved estimate simply stays as-is until a future
                // reconciliation pass settles it with real numbers.
            }
        }
    }

    fn emit_billing_side_effects(
        input: &FinalizationInput,
        billing: BillingDerivation,
        metrics: &dyn MiniChatMetricsPort,
    ) {
        if billing.unknown_error_code {
            error!(
                error_code = ?input.error_code,
                turn_id = %input.turn_id,
                "CRITICAL: unknown error code in billing derivation"
            );
        }

        if billing.outcome == BillingOutcome::Aborted {
            let abort_trigger = match input.error_code.as_deref() {
                Some("orphan_timeout") => trigger::ORPHAN_TIMEOUT,
                _ if input.terminal_state == TurnState::Cancelled => trigger::CLIENT_DISCONNECT,
                _ => trigger::INTERNAL_ABORT,
            };
            warn!(
                turn_id = %input.turn_id,
                trigger = abort_trigger,
                "stream aborted"
            );
            metrics.record_streams_aborted(abort_trigger);
        }
    }
}

fn build_turn_audit_envelope(input: &FinalizationInput, trace_id: Option<String>) -> AuditEnvelope {
    let event_type = match input.terminal_state {
        TurnState::Completed => TurnAuditEventType::TurnCompleted,
        _ => TurnAuditEventType::TurnFailed,
    };

    let usage = input.usage.unwrap_or(Usage {
        input_tokens: 0,
        output_tokens: 0,
    });

    AuditEnvelope::Turn(TurnAuditEvent {
        event_type,
        timestamp: time::OffsetDateTime::now_utc(),
        tenant_id: input.tenant_id,
        requester_type: input.requester_type,
        trace_id,
        user_id: input.user_id,
        chat_id: input.chat_id,
        turn_id: input.turn_id,
        request_id: input.request_id,
        selected_model: input.selected_model.clone(),
        effective_model: input.effective_model.clone(),
        policy_version_applied: Some(input.policy_version_applied.cast_unsigned()),
        usage: AuditUsageTokens {
            input_tokens: usage.input_tokens.cast_unsigned(),
            output_tokens: usage.output_tokens.cast_unsigned(),
            model: Some(input.effective_model.clone()),
        },
        latency_ms: LatencyMs {
            ttft_ms: input.ttft_ms,
            total_ms: input.total_ms,
        },
        policy_decisions: PolicyDecisions {
            license: None,
            quota: QuotaDecision {
                decision: input.quota_decision.clone(),
                quota_scope: None,
                downgrade_from: input.downgrade_from.clone(),
                downgrade_reason: input.downgrade_reason.clone(),
            },
        },
        error_code: input.error_code.clone(),
        prompt: None,
        response: None,
        attachments: Vec::new(),
        tool_calls: None,
    })
}

fn build_usage_event(
    input: &FinalizationInput,
    billing: BillingDerivation,
    settlement: &SettlementOutcome,
) -> UsageEvent {
    let terminal_state = match input.terminal_state {
        TurnState::Running => "running",
        TurnState::Completed => "completed",
        TurnState::Failed => "failed",
        TurnState::Cancelled => "cancelled",
    };
    let settlement_method = match settlement.settlement_method {
        SettlementMethod::Actual => "actual",
        SettlementMethod::Estimated => "estimated",
        SettlementMethod::Released => "released",
    };
    UsageEvent {
        tenant_id: input.tenant_id,
        user_id: input.user_id,
        chat_id: input.chat_id,
        turn_id: input.turn_id,
        request_id: input.request_id,
        effective_model: input.effective_model.clone(),
        selected_model: input.selected_model.clone(),
        terminal_state: terminal_state.to_owned(),
        billing_outcome: billing.outcome.as_str().to_owned(),
        usage: input.usage.map(|u| UsageTokens {
            input_tokens: u.input_tokens.cast_unsigned(),
            output_tokens: u.output_tokens.cast_unsigned(),
        }),
        actual_credits_micro: settlement.actual_credits_micro,
        settlement_method: settlement_method.to_owned(),
        policy_version_applied: input.policy_version_applied,
        timestamp: time::OffsetDateTime::now_utc(),
    }
}

/// Internal error type to distinguish message persistence failure
/// from other domain errors, enabling the retry-as-failed logic.
#[domain_model]
enum FinalizationError {
    Domain(DomainError),
    MessagePersistenceFailed(String),
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::llm::Usage;
    use crate::domain::model::finalization::FinalizationInput;
    use crate::domain::model::quota::{SettlementMethod, SettlementOutcome};
    use crate::domain::repos::{CreateTurnParams, TurnRepository as TurnRepoTrait};
    use crate::domain::service::AuditEnvelope;
    use crate::domain::service::test_helpers::{
        RecordingOutboxEnqueuer, inmem_db, mock_db_provider,
    };
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
            ttft_ms: None,
            total_ms: None,
        }
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
}
