pub(super) mod provider_task;
mod types;

pub use types::{StreamError, StreamOutcome};

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use modkit_macros::domain_model;
use modkit_security::{AccessScope, SecurityContext};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

use crate::config::{ContextConfig, StreamingConfig};
use crate::domain::error::DomainError;
use crate::domain::models::ResolvedModel;
use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::ports::metric_labels::{decision, period};
use crate::domain::repos::{
    AttachmentRepository, CasTerminalParams, ChatRepository, CreateTurnParams,
    InsertUserMessageParams, MessageAttachmentRepository, MessageRepository, QuotaUsageRepository,
    SnapshotBoundary, ThreadSummaryRepository, TurnRepository, VectorStoreRepository,
};
use crate::domain::stream_events::{StreamEvent, StreamStartedData};
use crate::infra::db::entity::chat_turn::TurnState;
use crate::infra::llm::provider_resolver::ProviderResolver;

use super::{DbProvider, actions, resources};
use types::{
    FinalizationCtx, InvalidAttachmentError, PreflightResult, attachment_err,
    check_input_token_limit, flatten_preflight, requester_type_from_str,
};

// ════════════════════════════════════════════════════════════════════════════
// StreamService
// ════════════════════════════════════════════════════════════════════════════

/// Service handling SSE streaming and turn orchestration.
///
/// In P1 this is a stateless proxy: it builds an LLM request, streams
/// provider events through a bounded channel, and returns a `StreamOutcome`.
/// P2 adds turn persistence (pre-stream checks + CAS finalization).
#[domain_model]
#[allow(dead_code)]
pub struct StreamService<
    TR: TurnRepository + 'static,
    MR: MessageRepository + 'static,
    QR: QuotaUsageRepository + 'static,
    CR: ChatRepository,
    TSR: ThreadSummaryRepository + 'static,
    AR: AttachmentRepository + 'static,
    VSR: VectorStoreRepository + 'static,
    MAR: MessageAttachmentRepository + 'static,
> {
    db: Arc<DbProvider>,
    turn_repo: Arc<TR>,
    message_repo: Arc<MR>,
    chat_repo: Arc<CR>,
    enforcer: PolicyEnforcer,
    provider_resolver: Arc<ProviderResolver>,
    streaming_config: StreamingConfig,
    finalization: Arc<crate::domain::service::finalization_service::FinalizationService<TR, MR>>,
    quota: Arc<crate::domain::service::QuotaService<QR>>,
    thread_summary_repo: Arc<TSR>,
    attachment_repo: Arc<AR>,
    vector_store_repo: Arc<VSR>,
    message_attachment_repo: Arc<MAR>,
    context_config: ContextConfig,
    rag_config: crate::config::RagConfig,
    metrics: Arc<dyn MiniChatMetricsPort>,
}

impl<
    TR: TurnRepository + 'static,
    MR: MessageRepository + 'static,
    QR: QuotaUsageRepository + 'static,
    CR: ChatRepository,
    TSR: ThreadSummaryRepository + 'static,
    AR: AttachmentRepository + 'static,
    VSR: VectorStoreRepository + 'static,
    MAR: MessageAttachmentRepository + 'static,
> StreamService<TR, MR, QR, CR, TSR, AR, VSR, MAR>
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        db: Arc<DbProvider>,
        turn_repo: Arc<TR>,
        message_repo: Arc<MR>,
        chat_repo: Arc<CR>,
        enforcer: PolicyEnforcer,
        provider_resolver: Arc<ProviderResolver>,
        streaming_config: StreamingConfig,
        finalization: Arc<
            crate::domain::service::finalization_service::FinalizationService<TR, MR>,
        >,
        quota: Arc<crate::domain::service::QuotaService<QR>>,
        thread_summary_repo: Arc<TSR>,
        attachment_repo: Arc<AR>,
        vector_store_repo: Arc<VSR>,
        message_attachment_repo: Arc<MAR>,
        context_config: ContextConfig,
        rag_config: crate::config::RagConfig,
        metrics: Arc<dyn MiniChatMetricsPort>,
    ) -> Self {
        Self {
            db,
            turn_repo,
            message_repo,
            chat_repo,
            enforcer,
            provider_resolver,
            streaming_config,
            finalization,
            quota,
            thread_summary_repo,
            attachment_repo,
            vector_store_repo,
            message_attachment_repo,
            context_config,
            rag_config,
            metrics,
        }
    }

    /// The configured channel capacity for the provider->writer mpsc channel.
    pub(crate) fn channel_capacity(&self) -> usize {
        usize::from(self.streaming_config.sse_channel_capacity)
    }

    /// Record quota preflight decision metrics.
    fn record_preflight_metrics(
        &self,
        computed: &super::quota_service::PreflightComputed,
        selected_model: &str,
    ) {
        use crate::domain::model::quota::PreflightDecision;
        let tier = computed.effective_tier();
        match &computed.decision {
            PreflightDecision::Allow {
                effective_model, ..
            } => {
                self.metrics
                    .record_quota_preflight(decision::ALLOW, effective_model, tier);
            }
            PreflightDecision::Downgrade {
                effective_model, ..
            } => {
                self.metrics
                    .record_quota_preflight(decision::DOWNGRADE, effective_model, tier);
            }
            PreflightDecision::Reject { .. } => {
                self.metrics
                    .record_quota_preflight(decision::REJECT, selected_model, tier);
            }
        }
    }

    /// The configured ping interval in seconds.
    pub(crate) fn ping_interval_secs(&self) -> u64 {
        u64::from(self.streaming_config.sse_ping_interval_seconds)
    }

    /// Perform pre-stream checks (idempotency, parallel guard, message/turn
    /// creation) then spawn the provider task.
    ///
    /// Returns `Err(StreamError)` if pre-stream validation fails (before SSE
    /// connection opens). The handler maps these to JSON error responses.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        clippy::cognitive_complexity
    )]
    pub(crate) async fn run_stream(
        &self,
        ctx: SecurityContext,
        chat_id: Uuid,
        request_id: Uuid,
        content: String,
        resolved_model: ResolvedModel,
        web_search_enabled: bool,
        attachment_ids: Vec<Uuid>,
        cancel: CancellationToken,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<tokio::task::JoinHandle<StreamOutcome>, StreamError> {
        let has_vision_input = resolved_model
            .multimodal_capabilities
            .iter()
            .any(|c| c == "VISION_INPUT");
        let ResolvedModel {
            model_id: model,
            provider_id,
            ..
        } = resolved_model;
        let tenant_id = ctx.subject_tenant_id();
        let user_id = ctx.subject_id();

        // ── Authorization ──
        let chat_scope = self
            .enforcer
            .access_scope(&ctx, &resources::CHAT, actions::SEND_MESSAGE, Some(chat_id))
            .await?
            .ensure_owner(ctx.subject_id());

        // Non-transactional connection for pre-stream checks (D6)
        let conn = self
            .db
            .conn()
            .map_err(|e| StreamError::TurnCreationFailed {
                source: DomainError::from(e),
            })?;

        // ── Verify chat exists (scoped) ──
        self.chat_repo
            .get(&conn, &chat_scope, chat_id)
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?
            .ok_or(StreamError::ChatNotFound { chat_id })?;

        let scope = chat_scope.tenant_only();

        // ── Idempotency check (DESIGN §3.7 Check Priority Order) ──
        if let Some(existing_turn) = self
            .turn_repo
            .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?
        {
            return Err(match existing_turn.state {
                TurnState::Completed => StreamError::Replay {
                    turn: Box::new(existing_turn),
                },
                _ => StreamError::Conflict {
                    code: "request_id_conflict".to_owned(),
                    message: format!(
                        "Turn for request_id {request_id} exists with state {:?}",
                        existing_turn.state
                    ),
                },
            });
        }

        // ── Parallel turn guard ──
        if let Some(running) = self
            .turn_repo
            .find_running_by_chat_id(&conn, &scope, chat_id)
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?
        {
            return Err(StreamError::Conflict {
                code: "turn_already_running".to_owned(),
                message: format!("Chat {} already has a running turn {}", chat_id, running.id),
            });
        }

        // ── Snapshot boundary (DESIGN §ContextPlan Determinism P1) ──
        // Must be computed BEFORE persisting the user message so the boundary
        // excludes the current user message from context queries.
        let snapshot_boundary = self
            .message_repo
            .snapshot_boundary(&conn, &scope, chat_id)
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?;

        // ── Pre-preflight attachment queries (for surcharge estimation) ──
        let pre_ready_doc_count = self
            .attachment_repo
            .count_ready_documents(&conn, &scope, chat_id)
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?;
        let pre_ci_file_ids = self
            .attachment_repo
            .get_code_interpreter_file_ids(&conn, &scope, chat_id)
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?;

        // ── Pre-fetch image attachment count for guards + token estimation ──
        // Validates chat_id to prevent cross-chat attachment references.
        let image_file_ids: Vec<String> = if attachment_ids.is_empty() {
            Vec::new()
        } else {
            let rows = self
                .attachment_repo
                .get_batch(&conn, &scope, &attachment_ids)
                .await
                .map_err(|e| StreamError::TurnCreationFailed { source: e })?;
            rows.iter()
                .filter(|r| {
                    r.chat_id == chat_id
                        && r.attachment_kind
                            == crate::infra::db::entity::attachment::AttachmentKind::Image
                        && r.status == crate::infra::db::entity::attachment::AttachmentStatus::Ready
                })
                .filter_map(|r| r.provider_file_id.clone())
                .collect()
        };
        let num_images = u32::try_from(image_file_ids.len()).unwrap_or(u32::MAX);

        // ── Image count guard (before preflight, before TX) ──
        if num_images > 0 {
            let max = self.rag_config.max_images_per_message;
            if num_images > max {
                return Err(StreamError::TooManyImages {
                    count: num_images,
                    max,
                });
            }
        }

        // ── Preflight quota evaluate (external I/O, no DB writes) ──
        let selected_model = model.clone();
        let computed = self
            .quota
            .preflight_evaluate(crate::domain::model::quota::PreflightInput {
                tenant_id,
                user_id,
                selected_model: selected_model.clone(),
                utf8_bytes: content.len() as u64,
                num_images,
                tools_enabled: pre_ready_doc_count > 0,
                web_search_enabled,
                code_interpreter_enabled: !pre_ci_file_ids.is_empty(),
                max_output_tokens_cap: self.streaming_config.max_output_tokens,
            })
            .await
            .map_err(|e| match e {
                DomainError::WebSearchDisabled => StreamError::WebSearchDisabled,
                other => StreamError::TurnCreationFailed { source: other },
            })?;

        // Metrics: quota preflight decision (before flatten so rejects are counted)
        self.record_preflight_metrics(&computed, &selected_model);

        let pf = flatten_preflight(computed.decision.clone())?;

        // ── Input token limit check ──
        check_input_token_limit(&content, &pf)?;

        // ── Post-preflight image guards (kill switches + vision capability) ──
        if num_images > 0 {
            if computed.kill_switches.disable_images {
                return Err(StreamError::ImagesDisabled);
            }
            // DESIGN.md line 181: check VISION_INPUT on the effective_model.
            // DESIGN.md line 3206: P1 catalog invariant — ALL enabled models
            // MUST include VISION_INPUT (enforced at startup). Under a valid
            // P1 config, quota downgrade cannot demote to a non-vision model,
            // so checking the selected_model is sufficient. This guard is
            // defensive for future non-vision models or catalog misconfiguration.
            if !has_vision_input {
                return Err(StreamError::UnsupportedMedia);
            }
        }

        // Metrics: estimated tokens (only on allow/downgrade)
        #[allow(clippy::cast_precision_loss)]
        self.metrics
            .record_quota_estimated_tokens(pf.reserve_tokens as f64);

        // Period boundaries from the computed preflight (used by finalization for settlement)
        let period_starts = computed.periods.clone();
        let file_search_disabled = computed.kill_switches.disable_file_search;
        let has_reserve_buckets = !computed.buckets.is_empty();

        // ── Retrieval mode determination ──
        let ready_doc_count = pre_ready_doc_count;

        let retrieval_mode = crate::domain::retrieval::determine_retrieval_mode(
            file_search_disabled,
            ready_doc_count,
            &[], // P1: empty — message_doc_attachment_ids used in P2 only
        );

        // P3-6: Kill switch logging
        if file_search_disabled && ready_doc_count > 0 {
            tracing::info!(
                chat_id = %chat_id,
                ready_doc_count,
                "file_search disabled by kill switch -- {ready_doc_count} ready documents skipped"
            );
        }

        let file_search_enabled = matches!(
            retrieval_mode,
            crate::domain::retrieval::RetrievalMode::UnrestrictedChatSearch
                | crate::domain::retrieval::RetrievalMode::FilteredByAttachmentIds(_)
        );

        // Lookup vector store (if file search is active)
        let vector_store_ids: Vec<String> = if file_search_enabled {
            self.vector_store_repo
                .find_by_chat(&conn, &scope, chat_id)
                .await
                .map_err(|e| StreamError::TurnCreationFailed { source: e })?
                .and_then(|row| row.vector_store_id)
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };

        // Build provider_file_id_map for citation mapping (moved into stream task in P4-3)
        let provider_file_id_map = if file_search_enabled {
            self.attachment_repo
                .build_provider_file_id_map(&conn, &scope, chat_id)
                .await
                .map_err(|e| StreamError::TurnCreationFailed { source: e })?
        } else {
            std::collections::HashMap::new()
        };

        // ── Code interpreter file IDs ──
        let (ci_file_ids, code_interpreter_enabled) = if pf.tool_support.code_interpreter
            && !computed.kill_switches.disable_code_interpreter
        {
            let enabled = !pre_ci_file_ids.is_empty();
            (pre_ci_file_ids, enabled)
        } else {
            (Vec::new(), false)
        };

        // ── Single transaction: reserve + user message + turn ──
        let requester_type = ctx.subject_type().unwrap_or("user").to_owned();
        let turn_id = self
            .reserve_and_create_turn(
                &scope,
                &pf,
                computed,
                tenant_id,
                user_id,
                chat_id,
                request_id,
                requester_type,
                content.clone(),
                attachment_ids,
                web_search_enabled,
            )
            .await?;

        // Metrics: quota reserve committed (one per period)
        if has_reserve_buckets {
            for (period_type, _) in &period_starts {
                let label = match period_type {
                    crate::infra::db::entity::quota_usage::PeriodType::Daily => period::DAILY,
                    crate::infra::db::entity::quota_usage::PeriodType::Monthly => period::MONTHLY,
                };
                self.metrics.record_quota_reserve(label);
            }
        }

        // Pre-generate assistant message ID (sent in StreamStartedData and used in CAS)
        let message_id = Uuid::new_v4();

        let finalization_ctx = FinalizationCtx {
            finalization_svc: Arc::clone(&self.finalization),
            db: Arc::clone(&self.db),
            turn_repo: Arc::clone(&self.turn_repo),
            scope,
            turn_id,
            tenant_id,
            chat_id,
            request_id,
            user_id,
            requester_type: requester_type_from_str(ctx.subject_type()),
            message_id,
            effective_model: pf.effective_model.clone(),
            selected_model: selected_model.clone(),
            reserve_tokens: pf.reserve_tokens,
            max_output_tokens_applied: pf.max_output_tokens_applied,
            reserved_credits_micro: pf.reserved_credits_micro,
            policy_version_applied: pf.policy_version_applied,
            minimal_generation_floor_applied: pf.minimal_generation_floor_applied,
            quota_decision: pf.quota_decision,
            downgrade_from: pf.downgrade_from,
            downgrade_reason: pf.downgrade_reason,
            period_starts,
            provider_id: provider_id.clone(),
            metrics: Arc::clone(&self.metrics),
            quota_warnings_provider: Arc::clone(&self.quota)
                as Arc<dyn crate::domain::service::quota_settler::QuotaWarningsProvider>,
        };

        // ── Context assembly ──
        let token_budget = Some(super::context_assembly::TokenBudget {
            context_window: pf.context_window,
            max_output_tokens_applied: pf.max_output_tokens_applied,
            budgets: pf.estimation_budgets,
            tools_enabled: file_search_enabled,
            web_search_enabled,
            code_interpreter_enabled,
        });
        let assembled = self
            .gather_context(
                tenant_id,
                chat_id,
                snapshot_boundary,
                &pf.system_prompt,
                &content,
                web_search_enabled,
                file_search_enabled,
                &vector_store_ids,
                None, // file_search_filters: wired by P4-6
                self.streaming_config.web_search_context_size,
                pf.max_retrieved_chunks_per_turn,
                ci_file_ids,
                token_budget,
                &image_file_ids,
            )
            .await?;

        // Record image metrics
        if num_images > 0 {
            self.metrics.record_image_inputs_per_turn(num_images);
        }
        let tenant_id_str = tenant_id.to_string();
        let resolved_provider = self
            .provider_resolver
            .resolve(&provider_id, Some(&tenant_id_str))
            .map_err(|e| StreamError::TurnCreationFailed {
                source: DomainError::internal(format!("provider resolution: {e}")),
            })?;
        // Build the full OAGW proxy path: {alias}{api_path} with {model} substituted.
        // Use effective provider_model_id (may differ from requested on downgrade).
        let effective_provider_model_id = pf.effective_provider_model_id.clone();
        let api_path = resolved_provider
            .api_path
            .replace("{model}", &effective_provider_model_id);
        let proxy_path = format!("{}{api_path}", resolved_provider.upstream_alias);

        emit_stream_started(&tx, request_id, message_id).await;

        Ok(provider_task::spawn_provider_task(
            ctx,
            provider_task::ProviderTaskConfig {
                llm: resolved_provider.adapter,
                upstream_alias: proxy_path,
                messages: assembled.messages,
                system_instructions: assembled.system_instructions,
                tools: assembled.tools,
                model: pf.effective_model,
                provider_model_id: effective_provider_model_id,
                max_output_tokens: pf.max_output_tokens_applied.cast_unsigned(),
                max_tool_calls: pf.max_tool_calls,
                web_search_max_calls: self.quota.web_search_max_calls_per_message(),
                code_interpreter_max_calls: self.quota.code_interpreter_max_calls_per_message(),
                api_params: pf.api_params,
                provider_file_id_map,
            },
            cancel,
            tx,
            Some(finalization_ctx),
        ))
    }

    /// Execute quota reserve, user-message insert, and turn creation in a
    /// single DB transaction. Returns the generated `turn_id`.
    #[allow(clippy::too_many_arguments)]
    async fn reserve_and_create_turn(
        &self,
        scope: &AccessScope,
        pf: &PreflightResult,
        computed: super::quota_service::PreflightComputed,
        tenant_id: Uuid,
        user_id: Uuid,
        chat_id: Uuid,
        request_id: Uuid,
        requester_type: String,
        content: String,
        attachment_ids: Vec<Uuid>,
        web_search_enabled: bool,
    ) -> Result<Uuid, StreamError> {
        let user_msg_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();

        let message_repo = Arc::clone(&self.message_repo);
        let turn_repo = Arc::clone(&self.turn_repo);
        let quota_repo = Arc::clone(&self.quota.repo);
        let attachment_repo = Arc::clone(&self.attachment_repo);
        let message_attachment_repo = Arc::clone(&self.message_attachment_repo);
        let scope_tx = scope.clone();
        let effective_model_tx = pf.effective_model.clone();
        let reserve_tokens = pf.reserve_tokens;
        let max_output_tokens_applied = pf.max_output_tokens_applied;
        let reserved_credits_micro = pf.reserved_credits_micro;
        let policy_version_applied = pf.policy_version_applied;
        let minimal_generation_floor_applied = pf.minimal_generation_floor_applied;

        self.db
            .transaction(|tx| {
                use crate::domain::repos::IncrementReserveParams;
                Box::pin(async move {
                    // 1. Write quota reserve
                    if !computed.buckets.is_empty() {
                        let reserve_scope = AccessScope::for_tenant(computed.tenant_id);
                        for bucket in &computed.buckets {
                            for (period_type, period_start) in &computed.periods {
                                quota_repo
                                    .increment_reserve(
                                        tx,
                                        &reserve_scope,
                                        IncrementReserveParams {
                                            tenant_id: computed.tenant_id,
                                            user_id: computed.user_id,
                                            period_type: period_type.clone(),
                                            period_start: *period_start,
                                            bucket: bucket.clone(),
                                            amount_micro: computed.reserved_credits_micro,
                                        },
                                    )
                                    .await
                                    .map_err(|e| {
                                        modkit_db::DbError::Other(anyhow::Error::new(e))
                                    })?;
                            }
                        }
                    }

                    // 2. Insert user message
                    message_repo
                        .insert_user_message(
                            tx,
                            &scope_tx,
                            InsertUserMessageParams {
                                id: user_msg_id,
                                tenant_id,
                                chat_id,
                                request_id,
                                content,
                            },
                        )
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    // 2b. Validate and link attachment_ids (if any)
                    if !attachment_ids.is_empty() {
                        // Deduplicate
                        let unique_ids: Vec<Uuid> = {
                            let mut seen = std::collections::HashSet::new();
                            attachment_ids
                                .iter()
                                .filter(|id| seen.insert(**id))
                                .copied()
                                .collect()
                        };
                        if unique_ids.len() != attachment_ids.len() {
                            return Err(attachment_err("Duplicate attachment IDs in request"));
                        }

                        let rows = attachment_repo
                            .get_batch(tx, &scope_tx, &attachment_ids)
                            .await
                            .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                        if rows.len() != attachment_ids.len() {
                            let found: std::collections::HashSet<Uuid> =
                                rows.iter().map(|r| r.id).collect();
                            let missing: Vec<_> = attachment_ids
                                .iter()
                                .filter(|id| !found.contains(id))
                                .collect();
                            return Err(attachment_err(format!(
                                "Attachment(s) not found: {missing:?}"
                            )));
                        }

                        for row in &rows {
                            // Must be ready
                            if row.status
                                != crate::infra::db::entity::attachment::AttachmentStatus::Ready
                            {
                                return Err(attachment_err(format!(
                                    "Attachment {} is not ready (status: {:?})",
                                    row.id, row.status
                                )));
                            }
                            // Must not be deleted
                            if row.deleted_at.is_some() {
                                return Err(attachment_err(format!(
                                    "Attachment {} has been deleted",
                                    row.id
                                )));
                            }
                            // Must belong to this chat
                            if row.chat_id != chat_id {
                                return Err(attachment_err(format!(
                                    "Attachment {} does not belong to chat {}",
                                    row.id, chat_id
                                )));
                            }
                            // Ownership check
                            if row.uploaded_by_user_id != user_id {
                                return Err(attachment_err(format!(
                                    "Attachment {} not owned by current user",
                                    row.id
                                )));
                            }
                        }

                        // Insert message_attachments rows
                        let ma_params: Vec<crate::domain::repos::InsertMessageAttachmentParams> =
                            attachment_ids
                                .iter()
                                .map(
                                    |att_id| crate::domain::repos::InsertMessageAttachmentParams {
                                        tenant_id,
                                        chat_id,
                                        message_id: user_msg_id,
                                        attachment_id: *att_id,
                                    },
                                )
                                .collect();

                        message_attachment_repo
                            .insert_batch(tx, &scope_tx, &ma_params)
                            .await
                            .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;
                    }

                    // 3. Create turn
                    turn_repo
                        .create_turn(
                            tx,
                            &scope_tx,
                            CreateTurnParams {
                                id: turn_id,
                                tenant_id,
                                chat_id,
                                request_id,
                                requester_type,
                                requester_user_id: Some(user_id),
                                reserve_tokens: Some(reserve_tokens),
                                max_output_tokens_applied: Some(max_output_tokens_applied),
                                reserved_credits_micro: Some(reserved_credits_micro),
                                policy_version_applied: Some(policy_version_applied),
                                effective_model: Some(effective_model_tx),
                                minimal_generation_floor_applied: Some(
                                    minimal_generation_floor_applied,
                                ),
                                web_search_enabled,
                            },
                        )
                        .await
                        .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    Ok(())
                })
            })
            .await
            .map_err(|e: modkit_db::DbError| match e {
                modkit_db::DbError::Other(anyhow_err) => {
                    match anyhow_err.downcast::<InvalidAttachmentError>() {
                        Ok(err) => StreamError::InvalidAttachment {
                            code: "invalid_attachment".to_owned(),
                            message: err.message,
                        },
                        Err(anyhow_err) => StreamError::TurnCreationFailed {
                            source: match anyhow_err.downcast::<DomainError>() {
                                Ok(domain_err) => domain_err,
                                Err(err) => DomainError::from(modkit_db::DbError::Other(err)),
                            },
                        },
                    }
                }
                other => StreamError::TurnCreationFailed {
                    source: DomainError::from(other),
                },
            })?;

        Ok(turn_id)
    }

    /// Shared context assembly: thread summary lookup, recent-message fetch
    /// (bounded by snapshot boundary), and `assemble_context` call.
    #[allow(clippy::too_many_arguments)]
    async fn gather_context(
        &self,
        tenant_id: Uuid,
        chat_id: Uuid,
        snapshot_boundary: Option<SnapshotBoundary>,
        system_prompt: &str,
        user_message: &str,
        web_search_enabled: bool,
        file_search_enabled: bool,
        vector_store_ids: &[String],
        file_search_filters: Option<crate::domain::llm::FileSearchFilter>,
        web_search_context_size: crate::domain::llm::WebSearchContextSize,
        file_search_max_num_results: u32,
        code_interpreter_file_ids: Vec<String>,
        token_budget: Option<super::context_assembly::TokenBudget>,
        image_file_ids: &[String],
    ) -> Result<super::context_assembly::AssembledContext, StreamError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| StreamError::TurnCreationFailed {
                source: DomainError::from(e),
            })?;
        let scope = AccessScope::for_tenant(tenant_id);

        let thread_summary = self
            .thread_summary_repo
            .get_latest(&conn, &scope, chat_id)
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?;

        let recent_messages = match &thread_summary {
            Some(ts) => {
                self.message_repo
                    .recent_after_boundary(
                        &conn,
                        &scope,
                        chat_id,
                        ts.boundary_created_at,
                        ts.boundary_message_id,
                        self.context_config.recent_messages_limit,
                        snapshot_boundary,
                    )
                    .await
            }
            None => {
                self.message_repo
                    .recent_for_context(
                        &conn,
                        &scope,
                        chat_id,
                        self.context_config.recent_messages_limit,
                        snapshot_boundary,
                    )
                    .await
            }
        }
        .map_err(|e| StreamError::TurnCreationFailed { source: e })?;

        // Map ORM models → domain ContextMessage (decouples context assembly from infra).
        let context_messages: Vec<crate::domain::llm::ContextMessage> = recent_messages
            .iter()
            .map(|m| crate::domain::llm::ContextMessage {
                role: match m.role {
                    crate::infra::db::entity::message::MessageRole::User => {
                        crate::domain::llm::Role::User
                    }
                    crate::infra::db::entity::message::MessageRole::Assistant => {
                        crate::domain::llm::Role::Assistant
                    }
                    crate::infra::db::entity::message::MessageRole::System => {
                        crate::domain::llm::Role::System
                    }
                },
                content: m.content.clone(),
            })
            .collect();

        super::context_assembly::assemble_context(&super::context_assembly::ContextInput {
            system_prompt,
            web_search_guard: &self.context_config.web_search_guard,
            file_search_guard: &self.context_config.file_search_guard,
            thread_summary: thread_summary.as_ref().map(|ts| ts.content.as_str()),
            recent_messages: &context_messages,
            user_message,
            web_search_enabled,
            file_search_enabled,
            vector_store_ids,
            file_search_filters,
            web_search_context_size,
            file_search_max_num_results,
            code_interpreter_file_ids,
            token_budget,
            image_file_ids,
        })
        .map_err(|e| StreamError::ContextBudgetExceeded {
            required_tokens: match &e {
                super::context_assembly::ContextAssemblyError::BudgetExceeded {
                    required_tokens,
                    ..
                } => *required_tokens,
            },
            available_tokens: match &e {
                super::context_assembly::ContextAssemblyError::BudgetExceeded {
                    available_tokens,
                    ..
                } => *available_tokens,
            },
        })
    }

    /// Run streaming for an already-created turn (used by retry/edit mutations).
    ///
    /// The mutation transaction has already created the turn (state=running) and
    /// user message. This method does quota preflight, writes reserves, resolves
    /// the provider, and spawns the streaming task.
    ///
    /// Per design D3: mutation transaction commits first, streaming runs post-commit.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        clippy::cognitive_complexity
    )]
    pub(crate) async fn run_stream_for_mutation(
        &self,
        ctx: SecurityContext,
        chat_id: Uuid,
        request_id: Uuid,
        turn_id: Uuid,
        content: String,
        resolved_model: ResolvedModel,
        web_search_enabled: bool,
        snapshot_boundary: Option<SnapshotBoundary>,
        cancel: CancellationToken,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<tokio::task::JoinHandle<StreamOutcome>, StreamError> {
        let model = resolved_model.model_id;
        let provider_id = resolved_model.provider_id;
        let tenant_id = ctx.subject_tenant_id();
        let user_id = ctx.subject_id();
        let scope = AccessScope::for_tenant(tenant_id);

        // ── Pre-preflight attachment queries (for surcharge estimation) ──
        let conn = self
            .db
            .conn()
            .map_err(|e| StreamError::TurnCreationFailed {
                source: DomainError::from(e),
            })?;
        let pre_ready_doc_count = self
            .attachment_repo
            .count_ready_documents(&conn, &scope, chat_id)
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?;
        let pre_ci_file_ids = self
            .attachment_repo
            .get_code_interpreter_file_ids(&conn, &scope, chat_id)
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?;

        // ── Preflight quota evaluate ────────────────────────────────────
        let selected_model = model;
        let computed = self
            .quota
            .preflight_evaluate(crate::domain::model::quota::PreflightInput {
                tenant_id,
                user_id,
                selected_model: selected_model.clone(),
                utf8_bytes: content.len() as u64,
                num_images: 0,
                tools_enabled: pre_ready_doc_count > 0,
                web_search_enabled,
                code_interpreter_enabled: !pre_ci_file_ids.is_empty(),
                max_output_tokens_cap: self.streaming_config.max_output_tokens,
            })
            .await
            .map_err(|e| match e {
                DomainError::WebSearchDisabled => StreamError::WebSearchDisabled,
                other => StreamError::TurnCreationFailed { source: other },
            })?;

        // Metrics: quota preflight decision (before flatten so rejects are counted)
        self.record_preflight_metrics(&computed, &selected_model);

        let pf = flatten_preflight(computed.decision.clone())?;

        // ── Input token limit check ──
        // The turn is already committed (created by mutate_for_stream). If the
        // message exceeds max_input_tokens we mark it Failed before returning so
        // the turn does not stay stuck in Running state.
        if let Err(too_long) = check_input_token_limit(&content, &pf) {
            let detail = match &too_long {
                StreamError::InputTooLong {
                    estimated_tokens,
                    max_input_tokens,
                } => Some(format!(
                    "estimated {estimated_tokens} tokens, limit {max_input_tokens}"
                )),
                _ => None,
            };
            if let Err(e) = self
                .turn_repo
                .cas_update_state(
                    &conn,
                    &scope,
                    CasTerminalParams {
                        turn_id,
                        state: TurnState::Failed,
                        error_code: Some("input_too_long".to_owned()),
                        error_detail: detail,
                        assistant_message_id: None,
                        provider_response_id: None,
                    },
                )
                .await
            {
                warn!(
                    %turn_id,
                    error = %e,
                    "failed to mark turn as Failed after InputTooLong check"
                );
            }
            return Err(too_long);
        }

        // Metrics: estimated tokens (only on allow/downgrade)
        #[allow(clippy::cast_precision_loss)]
        self.metrics
            .record_quota_estimated_tokens(pf.reserve_tokens as f64);

        let period_starts = computed.periods.clone();
        let file_search_disabled = computed.kill_switches.disable_file_search;
        let disable_code_interpreter = computed.kill_switches.disable_code_interpreter;

        // ── Persist preflight fields + write quota reserves atomically ──
        // Both must be visible together so the orphan watchdog can settle
        // quota correctly if the pod crashes after this point.
        let quota_repo = Arc::clone(&self.quota.repo);
        let turn_repo_tx = Arc::clone(&self.turn_repo);
        let computed_for_tx = computed;
        let has_reserves = !computed_for_tx.buckets.is_empty();
        let preflight_params = crate::domain::repos::UpdatePreflightParams {
            turn_id,
            reserve_tokens: pf.reserve_tokens,
            max_output_tokens_applied: pf.max_output_tokens_applied,
            reserved_credits_micro: pf.reserved_credits_micro,
            policy_version_applied: pf.policy_version_applied,
            effective_model: pf.effective_model.clone(),
            minimal_generation_floor_applied: pf.minimal_generation_floor_applied,
        };
        let scope_for_tx = scope.clone();

        {
            self.db
                .transaction(|txn| {
                    use crate::domain::repos::IncrementReserveParams;
                    Box::pin(async move {
                        // 1. Backfill preflight fields on the turn row.
                        turn_repo_tx
                            .update_preflight_fields(txn, &scope_for_tx, preflight_params)
                            .await
                            .map_err(|e| modkit_db::DbError::Other(anyhow::Error::new(e)))?;

                        // 2. Write quota reserves.
                        let reserve_scope = AccessScope::for_tenant(computed_for_tx.tenant_id);
                        for bucket in &computed_for_tx.buckets {
                            for (period_type, period_start) in &computed_for_tx.periods {
                                quota_repo
                                    .increment_reserve(
                                        txn,
                                        &reserve_scope,
                                        IncrementReserveParams {
                                            tenant_id: computed_for_tx.tenant_id,
                                            user_id: computed_for_tx.user_id,
                                            period_type: period_type.clone(),
                                            period_start: *period_start,
                                            bucket: bucket.clone(),
                                            amount_micro: computed_for_tx.reserved_credits_micro,
                                        },
                                    )
                                    .await
                                    .map_err(|e| {
                                        modkit_db::DbError::Other(anyhow::Error::new(e))
                                    })?;
                            }
                        }
                        Ok(())
                    })
                })
                .await
                .map_err(|e| StreamError::TurnCreationFailed {
                    source: DomainError::database(e.to_string()),
                })?;

            // Metrics: quota reserve committed (one per period, only when reserves exist)
            if has_reserves {
                for (period_type, _) in &period_starts {
                    let label = match period_type {
                        crate::infra::db::entity::quota_usage::PeriodType::Daily => period::DAILY,
                        crate::infra::db::entity::quota_usage::PeriodType::Monthly => {
                            period::MONTHLY
                        }
                    };
                    self.metrics.record_quota_reserve(label);
                }
            }
        }

        // ── Retrieval mode determination ──
        let ready_doc_count = pre_ready_doc_count;

        let retrieval_mode = crate::domain::retrieval::determine_retrieval_mode(
            file_search_disabled,
            ready_doc_count,
            &[],
        );

        if file_search_disabled && ready_doc_count > 0 {
            tracing::info!(
                chat_id = %chat_id,
                ready_doc_count,
                "file_search disabled by kill switch during mutation -- {ready_doc_count} ready documents skipped"
            );
        }

        let file_search_enabled = matches!(
            retrieval_mode,
            crate::domain::retrieval::RetrievalMode::UnrestrictedChatSearch
                | crate::domain::retrieval::RetrievalMode::FilteredByAttachmentIds(_)
        );

        let vector_store_ids: Vec<String> = if file_search_enabled {
            self.vector_store_repo
                .find_by_chat(&conn, &scope, chat_id)
                .await
                .map_err(|e| StreamError::TurnCreationFailed { source: e })?
                .and_then(|row| row.vector_store_id)
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };

        let provider_file_id_map = if file_search_enabled {
            self.attachment_repo
                .build_provider_file_id_map(&conn, &scope, chat_id)
                .await
                .map_err(|e| StreamError::TurnCreationFailed { source: e })?
        } else {
            std::collections::HashMap::new()
        };

        // ── Code interpreter file IDs ──
        let (ci_file_ids, code_interpreter_enabled) =
            if pf.tool_support.code_interpreter && !disable_code_interpreter {
                let enabled = !pre_ci_file_ids.is_empty();
                (pre_ci_file_ids, enabled)
            } else {
                (Vec::new(), false)
            };

        // ── Build finalization context + resolve provider + spawn ────────
        let message_id = Uuid::new_v4();

        let finalization_ctx = FinalizationCtx {
            finalization_svc: Arc::clone(&self.finalization),
            db: Arc::clone(&self.db),
            turn_repo: Arc::clone(&self.turn_repo),
            scope: scope.clone(),
            turn_id,
            tenant_id,
            chat_id,
            request_id,
            user_id,
            requester_type: requester_type_from_str(ctx.subject_type()),
            message_id,
            effective_model: pf.effective_model.clone(),
            selected_model: selected_model.clone(),
            reserve_tokens: pf.reserve_tokens,
            max_output_tokens_applied: pf.max_output_tokens_applied,
            reserved_credits_micro: pf.reserved_credits_micro,
            policy_version_applied: pf.policy_version_applied,
            minimal_generation_floor_applied: pf.minimal_generation_floor_applied,
            quota_decision: pf.quota_decision,
            downgrade_from: pf.downgrade_from,
            downgrade_reason: pf.downgrade_reason,
            period_starts,
            provider_id: provider_id.clone(),
            metrics: Arc::clone(&self.metrics),
            quota_warnings_provider: Arc::clone(&self.quota)
                as Arc<dyn crate::domain::service::quota_settler::QuotaWarningsProvider>,
        };

        // ── Context assembly ──
        let token_budget = Some(super::context_assembly::TokenBudget {
            context_window: pf.context_window,
            max_output_tokens_applied: pf.max_output_tokens_applied,
            budgets: pf.estimation_budgets,
            tools_enabled: file_search_enabled,
            web_search_enabled,
            code_interpreter_enabled,
        });
        let assembled = self
            .gather_context(
                tenant_id,
                chat_id,
                snapshot_boundary,
                &pf.system_prompt,
                &content,
                web_search_enabled,
                file_search_enabled,
                &vector_store_ids,
                None, // file_search_filters: wired by P4-6
                self.streaming_config.web_search_context_size,
                pf.max_retrieved_chunks_per_turn,
                ci_file_ids,
                token_budget,
                &[], // retry/edit: no new image attachments
            )
            .await?;

        let tenant_id_str = tenant_id.to_string();
        let resolved_provider = self
            .provider_resolver
            .resolve(&provider_id, Some(&tenant_id_str))
            .map_err(|e| StreamError::TurnCreationFailed {
                source: DomainError::internal(format!("provider resolution: {e}")),
            })?;
        let effective_provider_model_id = pf.effective_provider_model_id.clone();
        let api_path = resolved_provider
            .api_path
            .replace("{model}", &effective_provider_model_id);
        let proxy_path = format!("{}{api_path}", resolved_provider.upstream_alias);

        emit_stream_started(&tx, request_id, message_id).await;

        Ok(provider_task::spawn_provider_task(
            ctx,
            provider_task::ProviderTaskConfig {
                llm: resolved_provider.adapter,
                upstream_alias: proxy_path,
                messages: assembled.messages,
                system_instructions: assembled.system_instructions,
                tools: assembled.tools,
                model: pf.effective_model,
                provider_model_id: effective_provider_model_id,
                max_output_tokens: pf.max_output_tokens_applied.cast_unsigned(),
                max_tool_calls: pf.max_tool_calls,
                web_search_max_calls: self.quota.web_search_max_calls_per_message(),
                code_interpreter_max_calls: self.quota.code_interpreter_max_calls_per_message(),
                api_params: pf.api_params,
                provider_file_id_map,
            },
            cancel,
            tx,
            Some(finalization_ctx),
        ))
    }
}

/// Emit `stream_started` before handing `tx` to the provider task (D3).
async fn emit_stream_started(tx: &mpsc::Sender<StreamEvent>, request_id: Uuid, message_id: Uuid) {
    if tx
        .send(StreamEvent::StreamStarted(StreamStartedData {
            request_id,
            message_id,
            is_new_turn: true,
        }))
        .await
        .is_err()
    {
        warn!(%request_id, "stream_started send failed (client disconnected before first event)");
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "mod_tests.rs"]
mod tests;
