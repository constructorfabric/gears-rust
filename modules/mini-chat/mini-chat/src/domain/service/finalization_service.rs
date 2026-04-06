use std::sync::Arc;

use super::current_otel_trace_id;
use modkit_macros::domain_model;
use tracing::{debug, error, warn};
use uuid::Uuid;

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
                        code_interpreter_calls: input.code_interpreter_calls,
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
                                    cache_read_input_tokens: input
                                        .usage
                                        .map(|u| u.cache_read_input_tokens),
                                    cache_write_input_tokens: input
                                        .usage
                                        .map(|u| u.cache_write_input_tokens),
                                    reasoning_tokens: input.usage.map(|u| u.reasoning_tokens),
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

                if input.code_interpreter_calls > 0 {
                    metrics.record_code_interpreter_calls(
                        &input.effective_model,
                        input.code_interpreter_calls,
                    );
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

    /// Finalize an orphan turn within a single database transaction.
    ///
    /// Uses the orphan-specific CAS guard that re-checks all predicates
    /// (`state`=running, `deleted_at` IS NULL, `last_progress_at` <= cutoff).
    /// Reuses billing derivation, quota settlement, and outbox enqueue.
    ///
    /// Returns `Ok(true)` if CAS won (turn finalized), `Ok(false)` if CAS lost.
    pub(crate) async fn finalize_orphan_turn(
        &self,
        input: crate::domain::model::finalization::OrphanFinalizationInput,
        timeout_secs: u64,
    ) -> Result<bool, DomainError> {
        use crate::domain::model::billing_outcome::{
            BillingDerivationInput, derive_billing_outcome,
        };
        use crate::domain::model::quota::SettlementPath;
        use crate::infra::db::entity::quota_usage::PeriodType;
        let start = std::time::Instant::now();
        let turn_repo = Arc::clone(&self.turn_repo);
        let quota_settler = Arc::clone(&self.quota_settler);
        let outbox_enqueuer = Arc::clone(&self.outbox_enqueuer);

        let tx_result = self
            .db
            .transaction(|tx| {
                Box::pin(async move {
                    // 1. CAS finalize orphan (re-checks all predicates)
                    let rows = turn_repo
                        .cas_finalize_orphan(tx, input.turn_id, timeout_secs)
                        .await
                        .map_err(to_db)?;

                    if rows == 0 {
                        debug!(
                            turn_id = %input.turn_id,
                            "orphan CAS loser: turn already finalized or progress renewed"
                        );
                        return Ok(false);
                    }

                    // 2. Derive billing outcome (pure function)
                    let billing = derive_billing_outcome(&BillingDerivationInput {
                        terminal_state: TurnState::Failed,
                        error_code: Some("orphan_timeout".to_owned()),
                        has_usage: false,
                    });

                    // 3. Settle quota if all required fields are present.
                    let quota_fields = input
                        .effective_model
                        .as_ref()
                        .zip(input.user_id)
                        .zip(input.reserve_tokens)
                        .zip(input.policy_version_applied)
                        .map(|(((em, uid), rt), pv)| (em.clone(), uid, rt, pv));

                    let settlement_outcome = if let Some((
                        effective_model,
                        user_id_val,
                        reserve_tokens,
                        policy_version_applied,
                    )) = quota_fields
                    {
                        let day = input.started_at.date();
                        // replace_day(1) is infallible for any valid Date, but the
                        // `time` crate returns Result so we propagate defensively.
                        let month_start = day
                            .replace_day(1)
                            .map_err(|e| to_db(DomainError::internal(e.to_string())))?;
                        let period_starts =
                            vec![(PeriodType::Daily, day), (PeriodType::Monthly, month_start)];

                        let settlement_input = SettlementInput {
                            tenant_id: input.tenant_id,
                            user_id: user_id_val,
                            effective_model,
                            policy_version_applied,
                            reserve_tokens,
                            max_output_tokens_applied: input.max_output_tokens_applied.unwrap_or(0),
                            reserved_credits_micro: input.reserved_credits_micro.unwrap_or(0),
                            minimal_generation_floor_applied: input
                                .minimal_generation_floor_applied
                                .unwrap_or(0),
                            settlement_path: SettlementPath::Estimated,
                            period_starts,
                            web_search_calls: input.web_search_completed_count,
                            code_interpreter_calls: input.code_interpreter_completed_count,
                        };

                        let scope = modkit_security::AccessScope::allow_all();
                        let outcome = quota_settler
                            .settle_in_tx(tx, &scope, settlement_input)
                            .await
                            .map_err(to_db)?;
                        Some(outcome)
                    } else {
                        warn!(
                            turn_id = %input.turn_id,
                            "orphan turn missing quota fields, skipping settlement"
                        );
                        None
                    };

                    // 4. Build and enqueue usage event
                    let now = time::OffsetDateTime::now_utc();
                    let effective_model_str = input.effective_model.clone().unwrap_or_default();
                    // Nil UUID for system requesters or turns without a user — the turn
                    // row may have NULL requester_user_id for system-initiated turns.
                    // Acceptable because billing settles by tenant, not user.
                    let user_id = input.user_id.unwrap_or(Uuid::nil());

                    let settlement_method_str =
                        settlement_outcome.as_ref().map_or("estimated", |s| {
                            match s.settlement_method {
                                SettlementMethod::Actual => "actual",
                                SettlementMethod::Estimated => "estimated",
                                SettlementMethod::Released => "released",
                            }
                        });

                    let actual_credits_micro = settlement_outcome
                        .as_ref()
                        .map_or(0, |s| s.actual_credits_micro);

                    let usage_event = UsageEvent {
                        tenant_id: input.tenant_id,
                        user_id,
                        chat_id: input.chat_id,
                        turn_id: input.turn_id,
                        request_id: input.request_id,
                        effective_model: effective_model_str.clone(),
                        selected_model: effective_model_str.clone(),
                        terminal_state: "failed".to_owned(),
                        billing_outcome: billing.outcome.as_str().to_owned(),
                        usage: None,
                        actual_credits_micro,
                        settlement_method: settlement_method_str.to_owned(),
                        policy_version_applied: input.policy_version_applied.unwrap_or(0),
                        web_search_calls: input.web_search_completed_count,
                        code_interpreter_calls: input.code_interpreter_completed_count,
                        timestamp: now,
                    };
                    outbox_enqueuer
                        .enqueue_usage_event(tx, usage_event)
                        .await
                        .map_err(to_db)?;

                    // 5. Build and enqueue audit event
                    let audit_event = AuditEnvelope::Turn(TurnAuditEvent {
                        event_type: TurnAuditEventType::TurnFailed,
                        timestamp: now,
                        tenant_id: input.tenant_id,
                        requester_type: input.requester_type,
                        trace_id: None,
                        user_id,
                        chat_id: input.chat_id,
                        turn_id: input.turn_id,
                        request_id: input.request_id,
                        selected_model: effective_model_str.clone(),
                        effective_model: effective_model_str.clone(),
                        policy_version_applied: input
                            .policy_version_applied
                            .map(i64::cast_unsigned),
                        usage: AuditUsageTokens {
                            input_tokens: 0,
                            output_tokens: 0,
                            model: Some(effective_model_str),
                            cache_read_input_tokens: Some(0),
                            cache_write_input_tokens: Some(0),
                            reasoning_tokens: Some(0),
                        },
                        latency_ms: LatencyMs {
                            ttft_ms: None,
                            total_ms: None,
                        },
                        policy_decisions: PolicyDecisions {
                            license: None,
                            quota: QuotaDecision {
                                decision: "unknown".to_owned(),
                                quota_scope: None,
                                downgrade_from: None,
                                downgrade_reason: None,
                            },
                        },
                        error_code: Some("orphan_timeout".to_owned()),
                        prompt: None,
                        response: None,
                        attachments: Vec::new(),
                        tool_calls: None,
                    });
                    outbox_enqueuer
                        .enqueue_audit_event(tx, audit_event)
                        .await
                        .map_err(to_db)?;

                    Ok(true)
                })
            })
            .await
            .map_err(DomainError::from)?;

        // Post-commit side effects (outside transaction).
        if tx_result {
            self.outbox_enqueuer.flush();
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            self.metrics.record_audit_emit(result_label::OK);
            self.metrics.record_finalization_latency_ms(ms);
            self.metrics.record_streams_aborted(trigger::ORPHAN_TIMEOUT);
        }

        Ok(tx_result)
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
        cache_read_input_tokens: 0,
        cache_write_input_tokens: 0,
        reasoning_tokens: 0,
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
            cache_read_input_tokens: Some(usage.cache_read_input_tokens.cast_unsigned()),
            cache_write_input_tokens: Some(usage.cache_write_input_tokens.cast_unsigned()),
            reasoning_tokens: Some(usage.reasoning_tokens.cast_unsigned()),
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
            cache_read_input_tokens: u.cache_read_input_tokens.cast_unsigned(),
            cache_write_input_tokens: u.cache_write_input_tokens.cast_unsigned(),
            reasoning_tokens: u.reasoning_tokens.cast_unsigned(),
        }),
        actual_credits_micro: settlement.actual_credits_micro,
        settlement_method: settlement_method.to_owned(),
        policy_version_applied: input.policy_version_applied,
        web_search_calls: input.web_search_calls,
        code_interpreter_calls: input.code_interpreter_calls,
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
#[path = "finalization_service_tests.rs"]
mod tests;
