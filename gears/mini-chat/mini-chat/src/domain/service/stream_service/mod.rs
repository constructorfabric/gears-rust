#![allow(clippy::non_ascii_literal, clippy::cognitive_complexity)]

pub(super) mod provider_task;
mod types;

pub use types::{StreamError, StreamOutcome};

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext};
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
use crate::domain::stream_events::{StreamEvent, StreamStartedData, ThreadSummaryInfo};
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

/// Resolved custom function tools for a request: `(dispatch map keyed by name,
/// tool descriptors for the LLM request, system-prompt guards)`.
type ResolvedFunctionTools = (
    std::collections::HashMap<String, Arc<dyn crate::domain::ports::FunctionTool>>,
    Vec<crate::domain::llm::LlmTool>,
    Vec<String>,
);

/// Pure resolution of enabled custom function tools against a registry.
///
/// Unknown tool names are silently ignored. Extracted as a free function so it
/// can be unit-tested without constructing a `StreamService`.
fn resolve_function_tools_from(
    registry: &std::collections::HashMap<String, Arc<dyn crate::domain::ports::FunctionTool>>,
    enabled: &[String],
) -> ResolvedFunctionTools {
    let mut map = std::collections::HashMap::new();
    let mut tools = Vec::new();
    let mut guards = Vec::new();
    for name in enabled {
        if let Some(tool) = registry.get(name) {
            let def = tool.definition();
            tools.push(crate::domain::llm::LlmTool::Function {
                name: def.name,
                description: def.description,
                parameters: def.parameters,
            });
            if let Some(guard) = tool.system_prompt_guard() {
                guards.push(guard);
            }
            map.insert(name.clone(), Arc::clone(tool));
        }
    }
    (map, tools, guards)
}

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
    knowledge_search_config: crate::config::KnowledgeSearchConfig,
    knowledge_retriever: Option<Arc<dyn crate::domain::ports::KnowledgeRetriever>>,
    /// Custom function-tool registry, keyed by tool name. Resolved per-request
    /// against the model's `enabled_function_tools`.
    function_tools:
        std::collections::HashMap<String, Arc<dyn crate::domain::ports::FunctionTool>>,
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
        knowledge_search_config: crate::config::KnowledgeSearchConfig,
        knowledge_retriever: Option<Arc<dyn crate::domain::ports::KnowledgeRetriever>>,
        function_tools: std::collections::HashMap<
            String,
            Arc<dyn crate::domain::ports::FunctionTool>,
        >,
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
            knowledge_search_config,
            knowledge_retriever,
            function_tools,
        }
    }

    /// Resolve the custom function tools enabled for a model against the
    /// registry. Returns `(dispatch map, tool descriptors, system-prompt guards)`.
    /// Unknown tool names are silently ignored.
    fn resolve_function_tools(&self, enabled: &[String]) -> ResolvedFunctionTools {
        resolve_function_tools_from(&self.function_tools, enabled)
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

    /// Build [`provider_task::KnowledgeSearchParams`] for the current request,
    /// resolving the upstream alias for the given tenant.
    ///
    /// Returns `None` when knowledge search is disabled or misconfigured.
    /// Every misconfiguration path emits a `warn!` so operators can diagnose
    /// why the feature silently disabled itself.
    fn build_knowledge_search_params(
        &self,
        tenant_id_str: &str,
    ) -> Option<provider_task::KnowledgeSearchParams> {
        if !self.knowledge_search_config.enabled {
            return None;
        }
        let Some(retriever) = self.knowledge_retriever.as_ref() else {
            warn!(
                "knowledge_search enabled but retriever is not wired; disabling for this request"
            );
            return None;
        };
        let Some(ks_provider_id) = self.knowledge_search_config.provider_id.as_deref() else {
            warn!("knowledge_search.provider_id is not configured; disabling for this request");
            return None;
        };
        // Only providers that emit TerminalOutcome::ToolUse support the agentic loop.
        // Currently: OpenAI Responses API and Anthropic Messages API.
        let provider_kind = self
            .provider_resolver
            .entries()
            .get(ks_provider_id)
            .map(|e| e.kind);
        let use_search_result_blocks = match provider_kind {
            Some(crate::infra::llm::providers::ProviderKind::OpenAiResponses) => false,
            Some(crate::infra::llm::providers::ProviderKind::AnthropicMessages) => true,
            other => {
                warn!(
                    provider_id = ks_provider_id,
                    ?other,
                    "knowledge_search requires openai_responses or anthropic_messages provider; \
                     other provider kinds do not support function tool calls — disabling for this request"
                );
                return None;
            }
        };
        let Some(vector_store_id) = self.knowledge_search_config.vector_store_id.clone() else {
            warn!("knowledge_search.vector_store_id is not configured; disabling for this request");
            return None;
        };
        let Some(upstream_alias) = self
            .provider_resolver
            .upstream_alias_for(ks_provider_id, Some(tenant_id_str))
        else {
            warn!(
                provider_id = ks_provider_id,
                tenant_id = tenant_id_str,
                "knowledge_search provider has no upstream alias for this tenant; disabling for this request"
            );
            return None;
        };
        let api_version = self
            .provider_resolver
            .entries()
            .get(ks_provider_id)
            .and_then(|e| e.api_version.as_deref())
            .unwrap_or("");
        if api_version.is_empty() {
            warn!(
                provider_id = ks_provider_id,
                "knowledge_search provider has no api_version configured; disabling for this request"
            );
            return None;
        }
        Some(provider_task::KnowledgeSearchParams {
            retriever: Arc::clone(retriever),
            vector_store_id,
            upstream_alias: upstream_alias.to_owned(),
            api_version: api_version.to_owned(),
            top_k: self.knowledge_search_config.top_k,
            max_calls: self.knowledge_search_config.max_calls_per_message,
            max_chunk_chars: self.knowledge_search_config.max_chunk_chars,
            use_search_result_blocks,
        })
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
        // Resolve custom function tools enabled for this model against the registry.
        let (function_tools, extra_function_tools, extra_function_guards) =
            self.resolve_function_tools(&resolved_model.enabled_function_tools);
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

        // ── Prior context size (for accurate quota reserve) ──
        // Fetch the last assistant message's actual token counts so preflight
        // can estimate the full context size, not just the current message.
        let prior_context_tokens = self
            .message_repo
            .last_assistant_token_counts(&conn, &scope, chat_id)
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?
            .map_or(0u64, |(inp, out)| {
                u64::try_from(inp.max(0))
                    .unwrap_or(0)
                    .saturating_add(u64::try_from(out.max(0)).unwrap_or(0))
            });

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
                prior_context_tokens,
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

        // For Anthropic-backed chats, also load the
        // `provider_file_id → anthropic_file_id` map so the adapter can
        // substitute the right id into outbound `image`/`document` blocks.
        // Empty for non-Anthropic providers.
        let anthropic_file_ids = if self.provider_resolver.is_anthropic_messages(&provider_id) {
            self.attachment_repo
                .build_secondary_file_id_map(
                    &conn,
                    &scope,
                    chat_id,
                    crate::infra::db::entity::attachment::secondary_provider_kind::ANTHROPIC,
                )
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

        let mut finalization_ctx = FinalizationCtx {
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
            context_window: pf.context_window,
            assembled_context_tokens: 0, // updated after context assembly
            messages_truncated: false,   // updated after context assembly
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
            // Custom function tools also incur the flat tool surcharge (Q1).
            tools_enabled: file_search_enabled || !function_tools.is_empty(),
            web_search_enabled,
            code_interpreter_enabled,
        });
        // file_search and knowledge_search are mutually exclusive — when both
        // are configured, file_search wins (native vector-store tool, already
        // billed via the adapter). knowledge_search would otherwise double-bill
        // OpenAI/Responses traffic and silently drop on Anthropic.
        let knowledge_search_enabled = self.knowledge_search_config.enabled
            && self.knowledge_retriever.is_some()
            && !file_search_enabled;
        let (assembled, summary_info) = self
            .gather_context(
                tenant_id,
                chat_id,
                snapshot_boundary,
                &pf.system_prompt,
                &content,
                web_search_enabled,
                file_search_enabled,
                knowledge_search_enabled,
                &vector_store_ids,
                None, // file_search_filters: wired by P4-6
                pf.web_search_context_size,
                pf.file_search_max_num_results,
                ci_file_ids,
                token_budget,
                &image_file_ids,
                extra_function_tools,
                extra_function_guards,
            )
            .await?;

        finalization_ctx.assembled_context_tokens = assembled.estimated_context_tokens;
        finalization_ctx.messages_truncated = assembled.messages_truncated;

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

        emit_stream_started(&tx, request_id, message_id, summary_info).await;

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
                anthropic_file_ids,
                knowledge_search: self.build_knowledge_search_params(&tenant_id_str),
                function_tools,
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
                                        toolkit_db::DbError::Other(anyhow::Error::new(e))
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
                        .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?;

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
                            .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?;

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
                            .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?;
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
                        .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?;

                    Ok(())
                })
            })
            .await
            .map_err(|e: toolkit_db::DbError| match e {
                toolkit_db::DbError::Other(anyhow_err) => {
                    match anyhow_err.downcast::<InvalidAttachmentError>() {
                        Ok(err) => StreamError::InvalidAttachment {
                            code: "invalid_attachment".to_owned(),
                            message: err.message,
                        },
                        Err(anyhow_err) => StreamError::TurnCreationFailed {
                            source: match anyhow_err.downcast::<DomainError>() {
                                Ok(domain_err) => domain_err,
                                Err(err) => DomainError::from(toolkit_db::DbError::Other(err)),
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
        knowledge_search_enabled: bool,
        vector_store_ids: &[String],
        file_search_filters: Option<crate::domain::llm::FileSearchFilter>,
        web_search_context_size: crate::domain::llm::WebSearchContextSize,
        file_search_max_num_results: u32,
        code_interpreter_file_ids: Vec<String>,
        token_budget: Option<super::context_assembly::TokenBudget>,
        image_file_ids: &[String],
        extra_function_tools: Vec<crate::domain::llm::LlmTool>,
        extra_function_guards: Vec<String>,
    ) -> Result<
        (
            super::context_assembly::AssembledContext,
            Option<ThreadSummaryInfo>,
        ),
        StreamError,
    > {
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

        let summary_info = thread_summary.as_ref().map(|ts| ThreadSummaryInfo {
            token_estimate: u32::try_from(ts.token_estimate).unwrap_or(0),
        });

        let recent_messages = match &thread_summary {
            Some(ts) => {
                self.message_repo
                    .recent_after_boundary(
                        &conn,
                        &scope,
                        chat_id,
                        ts.frontier.created_at,
                        ts.frontier.message_id,
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

        let assembled =
            super::context_assembly::assemble_context(&super::context_assembly::ContextInput {
                system_prompt,
                web_search_guard: &self.context_config.web_search_guard,
                file_search_guard: &self.context_config.file_search_guard,
                knowledge_search_guard: &self.knowledge_search_config.guard,
                thread_summary: thread_summary.as_ref().map(|ts| ts.content.as_str()),
                recent_messages: &context_messages,
                user_message,
                web_search_enabled,
                file_search_enabled,
                knowledge_search_enabled,
                vector_store_ids,
                file_search_filters,
                web_search_context_size,
                file_search_max_num_results,
                code_interpreter_file_ids,
                token_budget,
                image_file_ids,
                extra_function_tools,
                extra_function_guards,
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
            })?;

        Ok((assembled, summary_info))
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
        // Resolve custom function tools enabled for this model against the registry.
        let (function_tools, extra_function_tools, extra_function_guards) =
            self.resolve_function_tools(&resolved_model.enabled_function_tools);
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

        let prior_context_tokens = self
            .message_repo
            .last_assistant_token_counts(&conn, &scope, chat_id)
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?
            .map_or(0u64, |(inp, out)| {
                u64::try_from(inp.max(0))
                    .unwrap_or(0)
                    .saturating_add(u64::try_from(out.max(0)).unwrap_or(0))
            });

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
                prior_context_tokens,
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
                            .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))?;

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
                                        toolkit_db::DbError::Other(anyhow::Error::new(e))
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

        let anthropic_file_ids = if self.provider_resolver.is_anthropic_messages(&provider_id) {
            self.attachment_repo
                .build_secondary_file_id_map(
                    &conn,
                    &scope,
                    chat_id,
                    crate::infra::db::entity::attachment::secondary_provider_kind::ANTHROPIC,
                )
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

        let mut finalization_ctx = FinalizationCtx {
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
            context_window: pf.context_window,
            assembled_context_tokens: 0, // updated after context assembly
            messages_truncated: false,   // updated after context assembly
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
            // Custom function tools also incur the flat tool surcharge (Q1).
            tools_enabled: file_search_enabled || !function_tools.is_empty(),
            web_search_enabled,
            code_interpreter_enabled,
        });
        // See `run_stream` for the mutual-exclusion rationale.
        let knowledge_search_enabled = self.knowledge_search_config.enabled
            && self.knowledge_retriever.is_some()
            && !file_search_enabled;
        let (assembled, summary_info) = self
            .gather_context(
                tenant_id,
                chat_id,
                snapshot_boundary,
                &pf.system_prompt,
                &content,
                web_search_enabled,
                file_search_enabled,
                knowledge_search_enabled,
                &vector_store_ids,
                None, // file_search_filters: wired by P4-6
                pf.web_search_context_size,
                pf.file_search_max_num_results,
                ci_file_ids,
                token_budget,
                &[], // retry/edit: no new image attachments
                extra_function_tools,
                extra_function_guards,
            )
            .await?;

        finalization_ctx.assembled_context_tokens = assembled.estimated_context_tokens;
        finalization_ctx.messages_truncated = assembled.messages_truncated;

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

        emit_stream_started(&tx, request_id, message_id, summary_info).await;

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
                anthropic_file_ids,
                knowledge_search: self.build_knowledge_search_params(&tenant_id_str),
                function_tools,
            },
            cancel,
            tx,
            Some(finalization_ctx),
        ))
    }
}

/// Emit `stream_started` before handing `tx` to the provider task (D3).
async fn emit_stream_started(
    tx: &mpsc::Sender<StreamEvent>,
    request_id: Uuid,
    message_id: Uuid,
    thread_summary_applied: Option<ThreadSummaryInfo>,
) {
    if tx
        .send(StreamEvent::StreamStarted(StreamStartedData {
            request_id,
            message_id,
            is_new_turn: true,
            thread_summary_applied,
        }))
        .await
        .is_err()
    {
        warn!(%request_id, "stream_started send failed (client disconnected before first event)");
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::types::{StreamTerminal, normalize_error};
    use super::*;
    use crate::domain::llm::{ToolPhase, Usage};
    use crate::domain::repos::CasTerminalParams;
    use crate::infra::db::repo::attachment_repo::AttachmentRepository as OrmAttachmentRepo;
    use crate::infra::db::repo::chat_repo::ChatRepository as OrmChatRepo;
    use crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository as OrmMessageAttachmentRepo;
    use crate::infra::db::repo::message_repo::MessageRepository as MsgRepo;
    use crate::infra::db::repo::turn_repo::TurnRepository as TurnRepo;
    use crate::infra::db::repo::vector_store_repo::VectorStoreRepository as OrmVectorStoreRepo;
    use crate::infra::llm::{
        Citation, CitationSource, ClientSseEvent, LlmMessage, LlmProvider, LlmProviderError,
        LlmRequest, NonStreaming, ProviderStream, ResponseResult, Streaming, TerminalOutcome,
        TranslatedEvent,
    };
    use futures::{StreamExt, stream};
    use mini_chat_sdk::RequesterType;
    use oagw_sdk::error::StreamingError;

    // ── Noop OutboxEnqueuer ──

    #[allow(de0309_must_have_domain_model)]
    struct NoopQuotaWarningsProvider;
    #[async_trait::async_trait]
    impl crate::domain::service::quota_settler::QuotaWarningsProvider for NoopQuotaWarningsProvider {
        async fn get_quota_warnings(
            &self,
            _scope: &toolkit_security::AccessScope,
            _tenant_id: Uuid,
            _user_id: Uuid,
        ) -> Result<
            Vec<crate::domain::stream_events::QuotaWarning>,
            crate::domain::error::DomainError,
        > {
            Ok(Vec::new())
        }
    }

    #[allow(de0309_must_have_domain_model)]
    struct NoopOutboxEnqueuer;
    #[async_trait::async_trait]
    impl crate::domain::repos::OutboxEnqueuer for NoopOutboxEnqueuer {
        async fn enqueue_usage_event(
            &self,
            _runner: &(dyn toolkit_db::secure::DBRunner + Sync),
            _event: mini_chat_sdk::UsageEvent,
        ) -> Result<(), crate::domain::error::DomainError> {
            Ok(())
        }

        async fn enqueue_attachment_cleanup(
            &self,
            _runner: &(dyn toolkit_db::secure::DBRunner + Sync),
            _event: crate::domain::repos::AttachmentCleanupEvent,
        ) -> Result<(), crate::domain::error::DomainError> {
            Ok(())
        }

        async fn enqueue_chat_cleanup(
            &self,
            _runner: &(dyn toolkit_db::secure::DBRunner + Sync),
            _event: crate::domain::repos::ChatCleanupEvent,
        ) -> Result<(), crate::domain::error::DomainError> {
            Ok(())
        }

        async fn enqueue_audit_event(
            &self,
            _runner: &(dyn toolkit_db::secure::DBRunner + Sync),
            _event: crate::domain::model::audit_envelope::AuditEnvelope,
        ) -> Result<(), crate::domain::error::DomainError> {
            Ok(())
        }

        async fn enqueue_thread_summary(
            &self,
            _: &(dyn toolkit_db::secure::DBRunner + Sync),
            _: crate::domain::repos::ThreadSummaryTaskPayload,
        ) -> Result<(), crate::domain::error::DomainError> {
            Ok(())
        }

        fn flush(&self) {}
    }

    #[test]
    fn normalize_rate_limited() {
        let err = LlmProviderError::RateLimited {
            retry_after_secs: Some(30),
        };
        let (code, _) = normalize_error(&err);
        assert_eq!(code, "rate_limited");
    }

    #[test]
    fn normalize_timeout() {
        let (code, _) = normalize_error(&LlmProviderError::Timeout);
        assert_eq!(code, "provider_timeout");
    }

    #[test]
    fn normalize_provider_error() {
        let err = LlmProviderError::ProviderError {
            code: "bad_request".into(),
            message: "something went wrong".into(),
            raw_detail: None,
        };
        let (code, msg) = normalize_error(&err);
        assert_eq!(code, "provider_error");
        assert_eq!(msg, "something went wrong");
    }

    #[test]
    fn normalize_unavailable() {
        let (code, _) = normalize_error(&LlmProviderError::ProviderUnavailable);
        assert_eq!(code, "provider_error");
    }

    #[test]
    fn normalize_invalid_response() {
        let err = LlmProviderError::InvalidResponse {
            detail: "bad json".into(),
        };
        let (code, msg) = normalize_error(&err);
        assert_eq!(code, "provider_error");
        assert_eq!(msg, "bad json");
    }

    // ── Mock LlmProvider for integration tests ──

    /// A mock LLM provider that yields predefined events and a terminal outcome.
    #[allow(de0309_must_have_domain_model)]
    struct MockProvider {
        events: std::sync::Mutex<Vec<Result<TranslatedEvent, StreamingError>>>,
    }

    impl MockProvider {
        fn completed(deltas: &[&str]) -> Self {
            let mut events: Vec<Result<TranslatedEvent, StreamingError>> = deltas
                .iter()
                .map(|text| {
                    Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                        r#type: "text",
                        content: (*text).to_owned(),
                    }))
                })
                .collect();

            let full_text: String = deltas.iter().copied().collect();
            events.push(Ok(TranslatedEvent::Terminal(TerminalOutcome::Completed {
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    reasoning_tokens: 0,
                },
                response_id: "resp-test".to_owned(),
                content: full_text,
                citations: vec![],
                raw_response: serde_json::Value::Null,
            })));

            Self {
                events: std::sync::Mutex::new(events),
            }
        }

        /// Provider that completes with citations.
        fn completed_with_citations(deltas: &[&str], citations: Vec<Citation>) -> Self {
            let mut events: Vec<Result<TranslatedEvent, StreamingError>> = deltas
                .iter()
                .map(|text| {
                    Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                        r#type: "text",
                        content: (*text).to_owned(),
                    }))
                })
                .collect();

            let full_text: String = deltas.iter().copied().collect();
            events.push(Ok(TranslatedEvent::Terminal(TerminalOutcome::Completed {
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    reasoning_tokens: 0,
                },
                response_id: "resp-test".to_owned(),
                content: full_text,
                citations,
                raw_response: serde_json::Value::Null,
            })));

            Self {
                events: std::sync::Mutex::new(events),
            }
        }

        fn failing() -> Self {
            Self {
                events: std::sync::Mutex::new(vec![Ok(TranslatedEvent::Terminal(
                    TerminalOutcome::Failed {
                        error: LlmProviderError::Timeout,
                        usage: None,
                        partial_content: String::new(),
                    },
                ))]),
            }
        }

        /// Provider that emits deltas then stops with `max_output_tokens` reason.
        fn incomplete(deltas: &[&str]) -> Self {
            let mut events: Vec<Result<TranslatedEvent, StreamingError>> = deltas
                .iter()
                .map(|text| {
                    Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                        r#type: "text",
                        content: (*text).to_owned(),
                    }))
                })
                .collect();

            events.push(Ok(TranslatedEvent::Terminal(TerminalOutcome::Incomplete {
                reason: "max_output_tokens".to_owned(),
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 4096,
                    cache_read_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    reasoning_tokens: 0,
                },
                partial_content: deltas.iter().copied().collect(),
            })));

            Self {
                events: std::sync::Mutex::new(events),
            }
        }

        /// Provider that emits `web_search` tool start/done pairs, then completes.
        fn with_web_search_calls(web_search_count: usize) -> Self {
            let mut events: Vec<Result<TranslatedEvent, StreamingError>> = Vec::new();

            // Emit a delta first so we have content
            events.push(Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                r#type: "text",
                content: "Hello".to_owned(),
            })));

            for _ in 0..web_search_count {
                events.push(Ok(TranslatedEvent::Sse(ClientSseEvent::Tool {
                    phase: ToolPhase::Start,
                    name: "web_search",
                    details: serde_json::json!({}),
                })));
                events.push(Ok(TranslatedEvent::Sse(ClientSseEvent::Tool {
                    phase: ToolPhase::Done,
                    name: "web_search",
                    details: serde_json::json!({}),
                })));
            }

            events.push(Ok(TranslatedEvent::Terminal(TerminalOutcome::Completed {
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    reasoning_tokens: 0,
                },
                response_id: "resp-test".to_owned(),
                content: "Hello".to_owned(),
                citations: vec![],
                raw_response: serde_json::Value::Null,
            })));

            Self {
                events: std::sync::Mutex::new(events),
            }
        }

        /// Provider that emits N `code_interpreter` start/done pairs, then completes.
        fn with_code_interpreter_calls(count: usize) -> Self {
            Self::with_tool_calls(&[("code_interpreter", count)])
        }

        /// Provider that emits tool start/done pairs for arbitrary tool names, then completes.
        fn with_tool_calls(calls: &[(&'static str, usize)]) -> Self {
            let mut events: Vec<Result<TranslatedEvent, StreamingError>> = Vec::new();

            events.push(Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                r#type: "text",
                content: "Hello".to_owned(),
            })));

            for &(name, count) in calls {
                for _ in 0..count {
                    events.push(Ok(TranslatedEvent::Sse(ClientSseEvent::Tool {
                        phase: ToolPhase::Start,
                        name,
                        details: serde_json::json!({}),
                    })));
                    events.push(Ok(TranslatedEvent::Sse(ClientSseEvent::Tool {
                        phase: ToolPhase::Done,
                        name,
                        details: serde_json::json!({}),
                    })));
                }
            }

            events.push(Ok(TranslatedEvent::Terminal(TerminalOutcome::Completed {
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_read_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    reasoning_tokens: 0,
                },
                response_id: "resp-test".to_owned(),
                content: "Hello".to_owned(),
                citations: vec![],
                raw_response: serde_json::Value::Null,
            })));

            Self {
                events: std::sync::Mutex::new(events),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        async fn stream(
            &self,
            _ctx: SecurityContext,
            _request: LlmRequest<Streaming>,
            _upstream_alias: &str,
            cancel: CancellationToken,
        ) -> Result<ProviderStream, LlmProviderError> {
            let events = self.events.lock().unwrap().drain(..).collect::<Vec<_>>();
            let inner = stream::iter(events);
            Ok(ProviderStream::new(inner, cancel))
        }

        async fn complete(
            &self,
            _ctx: SecurityContext,
            _request: LlmRequest<NonStreaming>,
            _upstream_alias: &str,
        ) -> Result<ResponseResult, LlmProviderError> {
            unimplemented!("not needed for streaming tests")
        }
    }

    fn mock_ctx() -> SecurityContext {
        SecurityContext::anonymous()
    }

    // ── Integration tests ──

    /// 6.5: End-to-end stream with mock provider returning deltas + completed.
    #[tokio::test]
    async fn end_to_end_completed_stream() {
        let provider: Arc<dyn LlmProvider> =
            Arc::new(MockProvider::completed(&["Hello", ", ", "world!"]));
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "test-model".into(),
                provider_model_id: "test-model".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel,
            tx,
            None,
        );

        // Collect all events from the channel
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        // Verify event sequence: 3 deltas + 1 done
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], StreamEvent::Delta(_)));
        assert!(matches!(events[1], StreamEvent::Delta(_)));
        assert!(matches!(events[2], StreamEvent::Delta(_)));
        assert!(matches!(events[3], StreamEvent::Done(_)));

        // Verify accumulated text in outcome
        let outcome = handle.await.expect("task should complete");
        assert_eq!(outcome.terminal, StreamTerminal::Completed);
        assert_eq!(outcome.accumulated_text, "Hello, world!");
        assert!(outcome.usage.is_some());
        assert_eq!(outcome.error_code, None);
        assert_eq!(outcome.provider_response_id.as_deref(), Some("resp-test"));
    }

    /// 6.5 variant: Provider fails before first event.
    #[tokio::test]
    async fn provider_error_produces_error_event() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::failing());
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "test-model".into(),
                provider_model_id: "test-model".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel,
            tx,
            None,
        );

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        // Should get exactly one Error event
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::Error(_)));

        let outcome = handle.await.expect("task should complete");
        assert_eq!(outcome.terminal, StreamTerminal::Failed);
        assert_eq!(outcome.error_code.as_deref(), Some("provider_timeout"));
    }

    /// Provider hitting `max_output_tokens` yields Incomplete outcome.
    #[tokio::test]
    async fn provider_incomplete_max_output_tokens() {
        let provider: Arc<dyn LlmProvider> =
            Arc::new(MockProvider::incomplete(&["Hello", ", wor"]));
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "test-model".into(),
                provider_model_id: "test-model".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel,
            tx,
            None,
        );

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        // 2 deltas + 1 done (incomplete maps to done event for client)
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], StreamEvent::Delta(_)));
        assert!(matches!(events[1], StreamEvent::Delta(_)));
        assert!(matches!(events[2], StreamEvent::Done(_)));

        let outcome = handle.await.expect("task should complete");
        assert_eq!(outcome.terminal, StreamTerminal::Incomplete);
        assert_eq!(outcome.accumulated_text, "Hello, wor");
        assert!(outcome.usage.is_some());
        let usage = outcome.usage.unwrap();
        assert_eq!(usage.output_tokens, 4096);
        assert_eq!(
            outcome.error_code.as_deref(),
            Some("incomplete:max_output_tokens")
        );
    }

    /// 6.6: Cancellation mid-stream.
    #[tokio::test]
    async fn cancellation_stops_stream() {
        // A provider that yields one delta then blocks until cancelled.
        #[allow(de0309_must_have_domain_model)]
        struct SlowProvider;

        #[async_trait::async_trait]
        impl LlmProvider for SlowProvider {
            async fn stream(
                &self,
                _ctx: SecurityContext,
                _request: LlmRequest<Streaming>,
                _upstream_alias: &str,
                cancel: CancellationToken,
            ) -> Result<ProviderStream, LlmProviderError> {
                let inner = stream::unfold(0u8, |state| async move {
                    if state == 0 {
                        Some((
                            Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                                r#type: "text",
                                content: "partial".to_owned(),
                            })),
                            1,
                        ))
                    } else {
                        // Block until cancelled
                        tokio::time::sleep(std::time::Duration::from_mins(1)).await;
                        None
                    }
                });
                Ok(ProviderStream::new(inner, cancel))
            }

            async fn complete(
                &self,
                _ctx: SecurityContext,
                _request: LlmRequest<NonStreaming>,
                _upstream_alias: &str,
            ) -> Result<ResponseResult, LlmProviderError> {
                unimplemented!()
            }
        }

        let provider: Arc<dyn LlmProvider> = Arc::new(SlowProvider);
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "test-model".into(),
                provider_model_id: "test-model".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel.clone(),
            tx,
            None,
        );

        // Read the first delta
        let first = rx.recv().await.expect("should get first delta");
        assert!(matches!(first, StreamEvent::Delta(_)));

        // Cancel the stream
        cancel.cancel();

        // The provider task should exit
        let outcome = handle.await.expect("task should complete");
        assert_eq!(outcome.terminal, StreamTerminal::Cancelled);
        assert_eq!(outcome.accumulated_text, "partial");
    }

    // ── Pre-stream check tests (7.6) ──

    use crate::domain::service::test_helpers::{
        MockPolicySnapshotProvider, MockThreadSummaryRepo, MockUserLimitsProvider,
        TestCatalogEntryParams, inmem_db, mock_db_provider, mock_enforcer,
        mock_thread_summary_repo, test_catalog_entry, test_security_ctx_with_id,
    };
    use crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository as OrmQuotaUsageRepo;

    /// Build a `StreamService` with real DB repos and a mock LLM provider.
    fn build_stream_service(
        db: Arc<DbProvider>,
        provider: Arc<dyn LlmProvider>,
    ) -> StreamService<
        TurnRepo,
        MsgRepo,
        OrmQuotaUsageRepo,
        OrmChatRepo,
        MockThreadSummaryRepo,
        OrmAttachmentRepo,
        OrmVectorStoreRepo,
        OrmMessageAttachmentRepo,
    > {
        build_stream_service_with_metrics(
            db,
            provider,
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
        )
    }

    /// Build a `StreamService` with real DB repos, a mock LLM provider,
    /// and an injectable metrics implementation.
    fn build_stream_service_with_metrics(
        db: Arc<DbProvider>,
        provider: Arc<dyn LlmProvider>,
        metrics: Arc<dyn crate::domain::ports::MiniChatMetricsPort>,
    ) -> StreamService<
        TurnRepo,
        MsgRepo,
        OrmQuotaUsageRepo,
        OrmChatRepo,
        MockThreadSummaryRepo,
        OrmAttachmentRepo,
        OrmVectorStoreRepo,
        OrmMessageAttachmentRepo,
    > {
        use crate::domain::service::finalization_service::FinalizationService;
        use crate::domain::service::quota_settler::QuotaSettler;

        // Mock QuotaSettler for stream service tests
        #[domain_model]
        struct MockQuotaSettler;
        #[async_trait::async_trait]
        impl QuotaSettler for MockQuotaSettler {
            async fn settle_in_tx(
                &self,
                _tx: &toolkit_db::secure::DbTx<'_>,
                _scope: &AccessScope,
                _input: crate::domain::model::quota::SettlementInput,
            ) -> Result<
                crate::domain::model::quota::SettlementOutcome,
                crate::domain::error::DomainError,
            > {
                Ok(crate::domain::model::quota::SettlementOutcome {
                    settlement_method: crate::domain::model::quota::SettlementMethod::Released,
                    actual_credits_micro: 0,
                    charged_tokens: 0,
                    overshoot_capped: false,
                })
            }
        }

        let provider_resolver = Arc::new(ProviderResolver::single_provider(provider));
        let turn_repo = Arc::new(TurnRepo);
        let message_repo = Arc::new(MsgRepo::new(toolkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }));
        let finalization = Arc::new(FinalizationService::new(
            Arc::clone(&db),
            Arc::clone(&turn_repo),
            Arc::clone(&message_repo),
            Arc::new(MockQuotaSettler) as Arc<dyn QuotaSettler>,
            Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
            Arc::clone(&metrics),
            crate::config::background::ThreadSummaryWorkerConfig::default(),
        ));

        // QuotaService with permissive defaults — model catalog includes
        // "gpt-5.2" (standard) so that preflight allows test requests.
        let quota_svc = Arc::new(crate::domain::service::QuotaService::new(
            Arc::clone(&db),
            Arc::new(OrmQuotaUsageRepo),
            Arc::new(MockPolicySnapshotProvider::new(
                mini_chat_sdk::PolicySnapshot {
                    user_id: Uuid::nil(),
                    policy_version: 1,
                    model_catalog: vec![test_catalog_entry(TestCatalogEntryParams {
                        model_id: "gpt-5.2".to_owned(),
                        provider_model_id: "gpt-5.2-2025-03-26".to_owned(),
                        display_name: "GPT 5.2".to_owned(),
                        tier: mini_chat_sdk::ModelTier::Standard,
                        enabled: true,
                        is_default: true,
                        input_tokens_credit_multiplier_micro: 1_000_000,
                        output_tokens_credit_multiplier_micro: 1_000_000,
                        multimodal_capabilities: vec![],
                        context_window: 128_000,
                        max_output_tokens: 4096,
                        description: String::new(),
                        provider_display_name: String::new(),
                        multiplier_display: "1x".to_owned(),
                        provider_id: "openai".to_owned(),
                    })],
                    kill_switches: mini_chat_sdk::KillSwitches::default(),
                },
            )),
            Arc::new(MockUserLimitsProvider::new(mini_chat_sdk::UserLimits {
                user_id: Uuid::nil(),
                policy_version: 1,
                standard: mini_chat_sdk::TierLimits {
                    limit_daily_credits_micro: 100_000_000,
                    limit_monthly_credits_micro: 1_000_000_000,
                },
                premium: mini_chat_sdk::TierLimits {
                    limit_daily_credits_micro: 50_000_000,
                    limit_monthly_credits_micro: 500_000_000,
                },
            })),
            crate::config::EstimationBudgets::default(),
            crate::config::QuotaConfig {
                overshoot_tolerance_factor: 1.10,
                ..crate::config::QuotaConfig::default()
            },
        ));

        StreamService::new(
            db,
            turn_repo,
            message_repo,
            Arc::new(OrmChatRepo::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            })),
            mock_enforcer(),
            provider_resolver,
            crate::config::StreamingConfig::default(),
            finalization,
            quota_svc,
            mock_thread_summary_repo(),
            Arc::new(crate::infra::db::repo::attachment_repo::AttachmentRepository),
            Arc::new(crate::infra::db::repo::vector_store_repo::VectorStoreRepository),
            Arc::new(crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository),
            crate::config::ContextConfig::default(),
            crate::config::RagConfig::default(),
            metrics,
            crate::config::KnowledgeSearchConfig::default(),
            None,
            std::collections::HashMap::new(),
        )
    }

    /// Insert a parent chat row (required by FK constraints).
    async fn insert_test_chat(db: &Arc<DbProvider>, tenant_id: Uuid, user_id: Uuid, chat_id: Uuid) {
        use crate::infra::db::entity::chat::{ActiveModel, Entity as ChatEntity};
        use sea_orm::Set;
        use time::OffsetDateTime;
        use toolkit_db::secure::secure_insert;

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

    fn test_resolved_model() -> ResolvedModel {
        ResolvedModel {
            model_id: "gpt-5.2".into(),
            provider_model_id: "gpt-5.2-2025-03-26".into(),
            provider_id: "openai".into(),
            display_name: "GPT 5.2".into(),
            tier: "standard".into(),
            multiplier_display: "1x".into(),
            description: None,
            multimodal_capabilities: vec![],
            context_window: 128_000,
            max_file_size_mb: 25,
            system_prompt: String::new(),
            tool_support: mini_chat_sdk::ModelToolSupport {
                web_search: false,
                file_search: false,
                image_generation: false,
                code_interpreter: false,
                mcp: false,
            },
            thread_summary_prompt: String::new(),
            max_output_tokens: 16_384,
            enabled_function_tools: Vec::new(),
        }
    }

    /// 7.6: Idempotency check — returns Replay when a completed turn exists.
    #[tokio::test]
    async fn prestream_idempotency_returns_replay_for_existing_turn() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        // Pre-create a completed turn
        let scope = AccessScope::allow_all();
        let conn = db.conn().unwrap();
        let turn_repo = TurnRepo;
        let turn = turn_repo
            .create_turn(
                &conn,
                &scope,
                CreateTurnParams {
                    id: Uuid::new_v4(),
                    tenant_id,
                    chat_id,
                    request_id,
                    requester_type: "user".to_owned(),
                    requester_user_id: None,
                    reserve_tokens: None,
                    max_output_tokens_applied: None,
                    reserved_credits_micro: None,
                    policy_version_applied: None,
                    effective_model: None,
                    minimal_generation_floor_applied: None,
                    web_search_enabled: false,
                },
            )
            .await
            .expect("create turn");

        turn_repo
            .cas_update_state(
                &conn,
                &scope,
                CasTerminalParams {
                    turn_id: turn.id,
                    state: TurnState::Completed,
                    error_code: None,
                    error_detail: None,
                    assistant_message_id: None,
                    provider_response_id: None,
                },
            )
            .await
            .expect("complete turn");

        // Now run_stream with same request_id → should get Replay
        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, _rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let err = svc
            .run_stream(
                ctx,
                chat_id,
                request_id,
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect_err("should be Replay");

        assert!(
            matches!(err, StreamError::Replay { .. }),
            "expected Replay, got: {err:?}"
        );
    }

    /// 6.2: Running turn with same `request_id` → Conflict (not Replay).
    #[tokio::test]
    async fn idempotency_running_turn_returns_conflict() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        // Pre-create a running turn (default state after create_turn)
        let scope = AccessScope::allow_all();
        let conn = db.conn().unwrap();
        let turn_repo = TurnRepo;
        turn_repo
            .create_turn(
                &conn,
                &scope,
                CreateTurnParams {
                    id: Uuid::new_v4(),
                    tenant_id,
                    chat_id,
                    request_id,
                    requester_type: "user".to_owned(),
                    requester_user_id: None,
                    reserve_tokens: None,
                    max_output_tokens_applied: None,
                    reserved_credits_micro: None,
                    policy_version_applied: None,
                    effective_model: None,
                    minimal_generation_floor_applied: None,
                    web_search_enabled: false,
                },
            )
            .await
            .expect("create turn");

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, _rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let err = svc
            .run_stream(
                ctx,
                chat_id,
                request_id,
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect_err("should be Conflict");

        match err {
            StreamError::Conflict { code, .. } => {
                assert_eq!(code, "request_id_conflict");
            }
            other => panic!("expected Conflict, got: {other:?}"),
        }
    }

    /// 6.3: Failed turn with same `request_id` → Conflict (not Replay).
    #[tokio::test]
    async fn idempotency_failed_turn_returns_conflict() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        let scope = AccessScope::allow_all();
        let conn = db.conn().unwrap();
        let turn_repo = TurnRepo;
        let turn = turn_repo
            .create_turn(
                &conn,
                &scope,
                CreateTurnParams {
                    id: Uuid::new_v4(),
                    tenant_id,
                    chat_id,
                    request_id,
                    requester_type: "user".to_owned(),
                    requester_user_id: None,
                    reserve_tokens: None,
                    max_output_tokens_applied: None,
                    reserved_credits_micro: None,
                    policy_version_applied: None,
                    effective_model: None,
                    minimal_generation_floor_applied: None,
                    web_search_enabled: false,
                },
            )
            .await
            .expect("create turn");

        turn_repo
            .cas_update_state(
                &conn,
                &scope,
                CasTerminalParams {
                    turn_id: turn.id,
                    state: TurnState::Failed,
                    error_code: Some("provider_error".to_owned()),
                    error_detail: Some("timeout".to_owned()),
                    assistant_message_id: None,
                    provider_response_id: None,
                },
            )
            .await
            .expect("fail turn");

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, _rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let err = svc
            .run_stream(
                ctx,
                chat_id,
                request_id,
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect_err("should be Conflict");

        match err {
            StreamError::Conflict { code, .. } => {
                assert_eq!(code, "request_id_conflict");
            }
            other => panic!("expected Conflict, got: {other:?}"),
        }
    }

    /// 6.4: Cancelled turn with same `request_id` → Conflict (not Replay).
    #[tokio::test]
    async fn idempotency_cancelled_turn_returns_conflict() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        let scope = AccessScope::allow_all();
        let conn = db.conn().unwrap();
        let turn_repo = TurnRepo;
        let turn = turn_repo
            .create_turn(
                &conn,
                &scope,
                CreateTurnParams {
                    id: Uuid::new_v4(),
                    tenant_id,
                    chat_id,
                    request_id,
                    requester_type: "user".to_owned(),
                    requester_user_id: None,
                    reserve_tokens: None,
                    max_output_tokens_applied: None,
                    reserved_credits_micro: None,
                    policy_version_applied: None,
                    effective_model: None,
                    minimal_generation_floor_applied: None,
                    web_search_enabled: false,
                },
            )
            .await
            .expect("create turn");

        turn_repo
            .cas_update_state(
                &conn,
                &scope,
                CasTerminalParams {
                    turn_id: turn.id,
                    state: TurnState::Cancelled,
                    error_code: None,
                    error_detail: None,
                    assistant_message_id: None,
                    provider_response_id: None,
                },
            )
            .await
            .expect("cancel turn");

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, _rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let err = svc
            .run_stream(
                ctx,
                chat_id,
                request_id,
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect_err("should be Conflict");

        match err {
            StreamError::Conflict { code, .. } => {
                assert_eq!(code, "request_id_conflict");
            }
            other => panic!("expected Conflict, got: {other:?}"),
        }
    }

    /// 7.6: Parallel turn guard — returns Conflict when a running turn exists.
    #[tokio::test]
    async fn prestream_parallel_guard_returns_conflict() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        // Pre-create a running turn for the same chat (different request_id)
        let scope = AccessScope::allow_all();
        let conn = db.conn().unwrap();
        let turn_repo = TurnRepo;
        turn_repo
            .create_turn(
                &conn,
                &scope,
                CreateTurnParams {
                    id: Uuid::new_v4(),
                    tenant_id,
                    chat_id,
                    request_id: Uuid::new_v4(), // different request
                    requester_type: "user".to_owned(),
                    requester_user_id: None,
                    reserve_tokens: None,
                    max_output_tokens_applied: None,
                    reserved_credits_micro: None,
                    policy_version_applied: None,
                    effective_model: None,
                    minimal_generation_floor_applied: None,
                    web_search_enabled: false,
                },
            )
            .await
            .expect("create running turn");

        // New request_id → passes idempotency, but hits parallel guard
        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, _rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let err = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect_err("should be Conflict");

        assert!(
            matches!(err, StreamError::Conflict { .. }),
            "expected Conflict, got: {err:?}"
        );
    }

    /// 7.6: Happy path — no existing turn, no running turns → succeeds.
    #[tokio::test]
    async fn prestream_happy_path_proceeds_to_stream() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
        let svc = build_stream_service(db, provider);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let handle = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect("should succeed");

        // Drain events
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        let outcome = handle.await.expect("task should complete");
        assert_eq!(outcome.terminal, StreamTerminal::Completed);
        assert_eq!(outcome.accumulated_text, "Hello");
    }

    // ── flatten_preflight unit tests ──

    /// `max_input_tokens` from `PreflightDecision::Allow` reaches `PreflightResult`.
    #[test]
    fn flatten_preflight_allow_propagates_max_input_tokens() {
        use crate::config::EstimationBudgets;
        use crate::domain::model::quota::PreflightDecision;

        let decision = PreflightDecision::Allow {
            effective_model: "m".to_owned(),
            effective_provider_model_id: "m-provider".to_owned(),
            reserve_tokens: 100,
            max_output_tokens_applied: 1024,
            reserved_credits_micro: 0,
            policy_version_applied: 1,
            minimal_generation_floor_applied: 50,
            system_prompt: String::new(),
            context_window: 128_000,
            max_input_tokens: 65_536,
            estimation_budgets: EstimationBudgets::default(),
            file_search_max_num_results: 4,
            max_tool_calls: 2,
            tool_support: mini_chat_sdk::ModelToolSupport {
                web_search: false,
                file_search: false,
                image_generation: false,
                code_interpreter: false,
                mcp: false,
            },
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
                reasoning_effort: None,
            },
            web_search_context_size: mini_chat_sdk::models::WebSearchContextSize::Low,
        };

        let result = flatten_preflight(decision).expect("Allow should produce Ok");
        assert_eq!(result.max_input_tokens, 65_536);
        assert_eq!(result.context_window, 128_000);
        assert_eq!(result.quota_decision, "allow");
        assert!(result.downgrade_from.is_none());
        assert!(result.downgrade_reason.is_none());
        assert_eq!(result.reserve_tokens, 100);
        assert_eq!(result.max_output_tokens_applied, 1024);
        assert_eq!(result.reserved_credits_micro, 0);
        assert_eq!(result.policy_version_applied, 1);
        assert_eq!(result.minimal_generation_floor_applied, 50);
        assert_eq!(result.system_prompt, "");
        assert_eq!(
            result.estimation_budgets.bytes_per_token_conservative,
            EstimationBudgets::default().bytes_per_token_conservative,
        );
        assert_eq!(result.file_search_max_num_results, 4);
        assert_eq!(result.max_tool_calls, 2);
        assert!((result.api_params.temperature - 0.7).abs() < f64::EPSILON);
        assert_eq!(
            result.web_search_context_size,
            mini_chat_sdk::models::WebSearchContextSize::Low,
        );
    }

    /// `max_input_tokens` from `PreflightDecision::Downgrade` reaches `PreflightResult`.
    #[test]
    fn flatten_preflight_downgrade_propagates_max_input_tokens() {
        use crate::config::EstimationBudgets;
        use crate::domain::model::quota::{DowngradeReason, PreflightDecision};

        let decision = PreflightDecision::Downgrade {
            effective_model: "m-mini".to_owned(),
            effective_provider_model_id: "m-mini-provider".to_owned(),
            reserve_tokens: 50,
            max_output_tokens_applied: 512,
            reserved_credits_micro: 0,
            policy_version_applied: 1,
            minimal_generation_floor_applied: 50,
            downgrade_from: "m".to_owned(),
            downgrade_reason: DowngradeReason::PremiumQuotaExhausted,
            system_prompt: String::new(),
            context_window: 32_000,
            max_input_tokens: 16_000,
            estimation_budgets: EstimationBudgets::default(),
            file_search_max_num_results: 4,
            max_tool_calls: 2,
            tool_support: mini_chat_sdk::ModelToolSupport {
                web_search: false,
                file_search: false,
                image_generation: false,
                code_interpreter: false,
                mcp: false,
            },
            api_params: mini_chat_sdk::ModelApiParams {
                temperature: 0.7,
                top_p: 1.0,
                frequency_penalty: 0.0,
                presence_penalty: 0.0,
                stop: vec![],
                extra_body: None,
                reasoning_effort: None,
            },
            web_search_context_size: mini_chat_sdk::models::WebSearchContextSize::Low,
        };

        let result = flatten_preflight(decision).expect("Downgrade should produce Ok");
        assert_eq!(result.max_input_tokens, 16_000);
        assert_eq!(result.context_window, 32_000);
        assert_eq!(result.quota_decision, "downgrade");
        assert_eq!(result.downgrade_from.as_deref(), Some("m"));
        assert!(result.downgrade_reason.is_some());
        assert_eq!(result.reserve_tokens, 50);
        assert_eq!(result.max_output_tokens_applied, 512);
        assert_eq!(result.reserved_credits_micro, 0);
        assert_eq!(result.policy_version_applied, 1);
        assert_eq!(result.minimal_generation_floor_applied, 50);
        assert_eq!(result.system_prompt, "");
        assert_eq!(
            result.estimation_budgets.bytes_per_token_conservative,
            EstimationBudgets::default().bytes_per_token_conservative,
        );
        assert_eq!(result.file_search_max_num_results, 4);
        assert_eq!(result.max_tool_calls, 2);
        assert!((result.api_params.temperature - 0.7).abs() < f64::EPSILON);
        assert_eq!(
            result.web_search_context_size,
            mini_chat_sdk::models::WebSearchContextSize::Low,
        );
    }

    // ── InputTooLong integration tests ──

    /// Build a `StreamService` whose model catalog sets `max_input_tokens` to
    /// `context_window` (the invariant in `test_catalog_entry`). Using a small
    /// `context_window` lets tests trigger `InputTooLong` with short content.
    fn build_stream_service_with_context_window(
        db: Arc<DbProvider>,
        provider: Arc<dyn LlmProvider>,
        context_window: u32,
    ) -> StreamService<
        TurnRepo,
        MsgRepo,
        OrmQuotaUsageRepo,
        OrmChatRepo,
        MockThreadSummaryRepo,
        OrmAttachmentRepo,
        OrmVectorStoreRepo,
        OrmMessageAttachmentRepo,
    > {
        use crate::domain::service::finalization_service::FinalizationService;
        use crate::domain::service::quota_settler::QuotaSettler;

        #[allow(de0309_must_have_domain_model)]
        struct MockQuotaSettler;
        #[async_trait::async_trait]
        impl QuotaSettler for MockQuotaSettler {
            async fn settle_in_tx(
                &self,
                _tx: &toolkit_db::secure::DbTx<'_>,
                _scope: &AccessScope,
                _input: crate::domain::model::quota::SettlementInput,
            ) -> Result<
                crate::domain::model::quota::SettlementOutcome,
                crate::domain::error::DomainError,
            > {
                Ok(crate::domain::model::quota::SettlementOutcome {
                    settlement_method: crate::domain::model::quota::SettlementMethod::Released,
                    actual_credits_micro: 0,
                    charged_tokens: 0,
                    overshoot_capped: false,
                })
            }
        }

        let metrics: Arc<dyn crate::domain::ports::MiniChatMetricsPort> =
            Arc::new(crate::domain::ports::metrics::NoopMetrics);
        let provider_resolver = Arc::new(ProviderResolver::single_provider(provider));
        let turn_repo = Arc::new(TurnRepo);
        let message_repo = Arc::new(MsgRepo::new(toolkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }));
        let finalization = Arc::new(FinalizationService::new(
            Arc::clone(&db),
            Arc::clone(&turn_repo),
            Arc::clone(&message_repo),
            Arc::new(MockQuotaSettler) as Arc<dyn QuotaSettler>,
            Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
            Arc::clone(&metrics),
            crate::config::background::ThreadSummaryWorkerConfig::default(),
        ));

        // Keep max_output_tokens well below context_window so preflight maths
        // don't overflow or reject before our InputTooLong check runs.
        #[allow(clippy::integer_division)]
        let max_output_tokens = (context_window / 4).max(1);
        let quota_svc = Arc::new(crate::domain::service::QuotaService::new(
            Arc::clone(&db),
            Arc::new(OrmQuotaUsageRepo),
            Arc::new(MockPolicySnapshotProvider::new(
                mini_chat_sdk::PolicySnapshot {
                    user_id: Uuid::nil(),
                    policy_version: 1,
                    model_catalog: vec![test_catalog_entry(TestCatalogEntryParams {
                        model_id: "gpt-5.2".to_owned(),
                        provider_model_id: "gpt-5.2-2025-03-26".to_owned(),
                        display_name: "GPT 5.2".to_owned(),
                        tier: mini_chat_sdk::ModelTier::Standard,
                        enabled: true,
                        is_default: true,
                        input_tokens_credit_multiplier_micro: 1_000_000,
                        output_tokens_credit_multiplier_micro: 1_000_000,
                        multimodal_capabilities: vec![],
                        context_window,
                        max_output_tokens,
                        description: String::new(),
                        provider_display_name: String::new(),
                        multiplier_display: "1x".to_owned(),
                        provider_id: "openai".to_owned(),
                    })],
                    kill_switches: mini_chat_sdk::KillSwitches::default(),
                },
            )),
            Arc::new(MockUserLimitsProvider::new(mini_chat_sdk::UserLimits {
                user_id: Uuid::nil(),
                policy_version: 1,
                standard: mini_chat_sdk::TierLimits {
                    limit_daily_credits_micro: 100_000_000,
                    limit_monthly_credits_micro: 1_000_000_000,
                },
                premium: mini_chat_sdk::TierLimits {
                    limit_daily_credits_micro: 50_000_000,
                    limit_monthly_credits_micro: 500_000_000,
                },
            })),
            crate::config::EstimationBudgets::default(),
            crate::config::QuotaConfig {
                overshoot_tolerance_factor: 1.10,
                ..crate::config::QuotaConfig::default()
            },
        ));

        StreamService::new(
            db,
            turn_repo,
            message_repo,
            Arc::new(OrmChatRepo::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            })),
            mock_enforcer(),
            provider_resolver,
            crate::config::StreamingConfig::default(),
            finalization,
            quota_svc,
            mock_thread_summary_repo(),
            Arc::new(crate::infra::db::repo::attachment_repo::AttachmentRepository),
            Arc::new(crate::infra::db::repo::vector_store_repo::VectorStoreRepository),
            Arc::new(crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository),
            crate::config::ContextConfig::default(),
            crate::config::RagConfig::default(),
            metrics,
            crate::config::KnowledgeSearchConfig::default(),
            None,
            std::collections::HashMap::new(),
        )
    }

    /// A message whose estimated token count exceeds `max_input_tokens` is
    /// rejected with `StreamError::InputTooLong` before any DB write.
    ///
    /// Setup: model catalog has `context_window = max_input_tokens = 500`.
    /// Content: 1500 ASCII bytes → ~523 estimated tokens (default budgets).
    ///   ceil(1500/4) + 100 = 475; with 10 % margin: ceil(475 * 110 / 100) = 523.
    ///   523 > 500 → `InputTooLong`.
    #[tokio::test]
    async fn run_stream_input_too_long_returns_error() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&[]));
        let svc = build_stream_service_with_context_window(db, provider, 500);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, _rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        // 1500 bytes → estimated ~523 tokens, which exceeds max_input_tokens=500
        let long_content = "a".repeat(1500);

        let err = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                long_content,
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect_err("should be InputTooLong");

        match err {
            StreamError::InputTooLong {
                estimated_tokens,
                max_input_tokens,
            } => {
                assert_eq!(max_input_tokens, 500);
                assert!(
                    estimated_tokens > u64::from(max_input_tokens),
                    "estimated {estimated_tokens} should exceed limit {max_input_tokens}"
                );
            }
            other => panic!("expected InputTooLong, got: {other:?}"),
        }
    }

    // ── Integration tests (8.2, 8.3) ──

    /// 8.2: Duplicate `request_id` returns `Replay` (service-level equivalent of 409).
    ///
    /// Full handler 409 mapping requires Axum test server infrastructure;
    /// this test verifies the service returns the correct `StreamError` variant
    /// that the handler maps to RFC-9457 JSON 409.
    #[tokio::test]
    async fn duplicate_request_id_returns_replay_with_turn_data() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        // First call succeeds — creates turn and streams
        let ctx1 = test_security_ctx_with_id(tenant_id, user_id);
        let (tx1, mut rx1) = mpsc::channel(32);
        let cancel1 = CancellationToken::new();
        let handle = svc
            .run_stream(
                ctx1,
                chat_id,
                request_id,
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel1,
                tx1,
            )
            .await
            .expect("first call should succeed");

        // Drain events to let the task complete
        while let Some(ev) = rx1.recv().await {
            if ev.is_terminal() {
                break;
            }
        }
        handle.await.expect("task complete");

        // Second call with same request_id → Replay with turn data
        let ctx2 = test_security_ctx_with_id(tenant_id, user_id);
        let (tx2, _rx2) = mpsc::channel(32);
        let cancel2 = CancellationToken::new();
        let err = svc
            .run_stream(
                ctx2,
                chat_id,
                request_id,
                "hello again".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel2,
                tx2,
            )
            .await
            .expect_err("should be Replay");

        match err {
            StreamError::Replay { turn } => {
                assert_eq!(turn.chat_id, chat_id);
                assert_eq!(turn.request_id, request_id);
            }
            other => panic!("expected Replay, got: {other:?}"),
        }
    }

    /// 8.3: Disconnect finalization — cancellation CAS-finalizes turn to cancelled.
    ///
    /// Simulates client disconnect by cancelling the token mid-stream,
    /// then verifies the turn was finalized to `cancelled` state in the DB.
    #[tokio::test]
    async fn disconnect_finalizes_turn_to_cancelled() {
        // Slow provider that yields one delta then blocks
        #[allow(de0309_must_have_domain_model)]
        struct SlowMockProvider;

        #[async_trait::async_trait]
        impl LlmProvider for SlowMockProvider {
            async fn stream(
                &self,
                _ctx: SecurityContext,
                _request: LlmRequest<Streaming>,
                _upstream_alias: &str,
                cancel: CancellationToken,
            ) -> Result<ProviderStream, LlmProviderError> {
                let inner = stream::unfold(0u8, |state| async move {
                    if state == 0 {
                        Some((
                            Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                                r#type: "text",
                                content: "partial".to_owned(),
                            })),
                            1,
                        ))
                    } else {
                        // Block until cancelled
                        tokio::time::sleep(std::time::Duration::from_mins(1)).await;
                        None
                    }
                });
                Ok(ProviderStream::new(inner, cancel))
            }

            async fn complete(
                &self,
                _ctx: SecurityContext,
                _request: LlmRequest<NonStreaming>,
                _upstream_alias: &str,
            ) -> Result<ResponseResult, LlmProviderError> {
                unimplemented!()
            }
        }

        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(SlowMockProvider);
        let svc = build_stream_service(db.clone(), provider);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let handle = svc
            .run_stream(
                ctx,
                chat_id,
                request_id,
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel.clone(),
                tx,
            )
            .await
            .expect("should start stream");

        // Read the stream_started event, then the first delta
        let started = rx.recv().await.expect("should get stream_started");
        assert!(matches!(started, StreamEvent::StreamStarted(_)));
        let first = rx.recv().await.expect("should get delta");
        assert!(matches!(first, StreamEvent::Delta(_)));

        // Simulate client disconnect
        cancel.cancel();

        // Wait for task to complete
        let outcome = handle.await.expect("task should complete");
        assert_eq!(outcome.terminal, StreamTerminal::Cancelled);

        // Verify the turn was CAS-finalized to cancelled in the DB
        let scope = AccessScope::for_tenant(tenant_id);
        let conn = db.conn().unwrap();
        let turn = TurnRepo
            .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
            .await
            .expect("find")
            .expect("turn should exist");

        assert_eq!(
            turn.state,
            TurnState::Cancelled,
            "turn should be cancelled after disconnect"
        );
        assert!(
            turn.completed_at.is_some(),
            "completed_at should be set after CAS finalization"
        );
        // D4: cancelled turn with accumulated text should have assistant_message_id
        assert!(
            turn.assistant_message_id.is_some(),
            "cancelled turn with partial text should have assistant_message_id"
        );
    }

    // ── Authorization tests ──

    /// Cross-tenant access: user from tenant B cannot stream on tenant A's chat.
    /// The scoped `chat_repo.get()` returns `None` (chat invisible), yielding `ChatNotFound`.
    #[tokio::test]
    async fn run_stream_cross_tenant_returns_chat_not_found() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_a = Uuid::new_v4();
        let user_a = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_a, user_a, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db, provider);

        // User from a different tenant
        let tenant_b = Uuid::new_v4();
        let ctx = test_security_ctx_with_id(tenant_b, Uuid::new_v4());
        let (tx, _rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let err = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect_err("should be ChatNotFound");

        match err {
            StreamError::ChatNotFound { chat_id: id } => {
                assert_eq!(id, chat_id);
            }
            other => panic!("expected ChatNotFound, got: {other:?}"),
        }
    }

    /// 8.6: `DoneData` uses `fctx.effective_model` / `fctx.selected_model` when they differ.
    ///
    /// Constructs a `FinalizationCtx` with `effective_model = "gpt-4o-mini"` and
    /// `selected_model = "gpt-4o"`, then verifies the Done SSE event reflects both.
    #[tokio::test]
    async fn done_data_uses_fctx_model_fields_when_they_differ() {
        use crate::domain::service::finalization_service::FinalizationService;
        use crate::domain::service::quota_settler::QuotaSettler;

        #[allow(de0309_must_have_domain_model)]
        struct NoopSettler;
        #[async_trait::async_trait]
        impl QuotaSettler for NoopSettler {
            async fn settle_in_tx(
                &self,
                _tx: &toolkit_db::secure::DbTx<'_>,
                _scope: &AccessScope,
                _input: crate::domain::model::quota::SettlementInput,
            ) -> Result<
                crate::domain::model::quota::SettlementOutcome,
                crate::domain::error::DomainError,
            > {
                Ok(crate::domain::model::quota::SettlementOutcome {
                    settlement_method: crate::domain::model::quota::SettlementMethod::Released,
                    actual_credits_micro: 0,
                    charged_tokens: 0,
                    overshoot_capped: false,
                })
            }
        }

        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let message_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        // Create a running turn in DB so that CAS finalization succeeds
        let scope = AccessScope::allow_all();
        let conn = db.conn().unwrap();
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
                    requester_user_id: Some(user_id),
                    reserve_tokens: Some(5000),
                    max_output_tokens_applied: Some(4096),
                    reserved_credits_micro: Some(250),
                    policy_version_applied: Some(1),
                    effective_model: Some("gpt-4o-mini".to_owned()),
                    minimal_generation_floor_applied: Some(50),
                    web_search_enabled: false,
                },
            )
            .await
            .expect("create turn");

        let turn_repo_arc = Arc::new(TurnRepo);
        let message_repo_arc = Arc::new(MsgRepo::new(toolkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }));
        let finalization_svc = Arc::new(FinalizationService::new(
            Arc::clone(&db),
            Arc::clone(&turn_repo_arc),
            Arc::clone(&message_repo_arc),
            Arc::new(NoopSettler) as Arc<dyn QuotaSettler>,
            Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            crate::config::background::ThreadSummaryWorkerConfig::default(),
        ));

        let fctx = FinalizationCtx {
            finalization_svc,
            db: Arc::clone(&db),
            turn_repo: Arc::clone(&turn_repo_arc),
            scope,
            turn_id,
            tenant_id,
            chat_id,
            request_id,
            user_id,
            requester_type: RequesterType::User,
            message_id,
            effective_model: "gpt-4o-mini".to_owned(),
            selected_model: "gpt-4o".to_owned(),
            reserve_tokens: 5000,
            max_output_tokens_applied: 4096,
            reserved_credits_micro: 250,
            policy_version_applied: 1,
            minimal_generation_floor_applied: 50,
            quota_decision: "downgrade".to_owned(),
            downgrade_from: Some("gpt-4o".to_owned()),
            downgrade_reason: Some("premium_exhausted".to_owned()),
            period_starts: Vec::new(),
            context_window: 128_000,
            assembled_context_tokens: 0,
            messages_truncated: false,
            provider_id: "openai".to_owned(),
            metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
            quota_warnings_provider: Arc::new(NoopQuotaWarningsProvider),
        };

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = provider_task::spawn_provider_task(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "gpt-4o-mini".into(), // effective_model passed as the model param
                provider_model_id: "gpt-4o-mini".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel,
            tx,
            Some(fctx),
        );

        // Collect events
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        let _outcome = handle.await.expect("task should complete");

        // Find the Done event and verify model fields
        let done = events
            .iter()
            .find_map(|ev| match ev {
                StreamEvent::Done(d) => Some(d),
                _ => None,
            })
            .expect("should have a Done event");

        assert_eq!(
            done.effective_model, "gpt-4o-mini",
            "effective_model should be the downgraded model"
        );
        assert_eq!(
            done.selected_model, "gpt-4o",
            "selected_model should be the user's original choice"
        );
        assert_eq!(done.quota_decision, "downgrade");
        assert_eq!(done.downgrade_from.as_deref(), Some("gpt-4o"));
        assert_eq!(done.downgrade_reason.as_deref(), Some("premium_exhausted"));
    }

    /// 8.7: Done fallback on finalization failure preserves quota fields from `fctx`.
    ///
    /// When `finalize_turn_cas` returns `Err`, the Done event must use
    /// `fctx.quota_decision`, `fctx.downgrade_from`, and `fctx.downgrade_reason`
    /// instead of hardcoding `"allow"` / `None` / `None` (issue #1364).
    #[tokio::test]
    async fn done_fallback_preserves_quota_fields_on_finalization_failure() {
        use crate::domain::service::finalization_service::FinalizationService;
        use crate::domain::service::quota_settler::QuotaSettler;

        #[allow(de0309_must_have_domain_model)]
        struct FailingSettler;
        #[async_trait::async_trait]
        impl QuotaSettler for FailingSettler {
            async fn settle_in_tx(
                &self,
                _tx: &toolkit_db::secure::DbTx<'_>,
                _scope: &AccessScope,
                _input: crate::domain::model::quota::SettlementInput,
            ) -> Result<
                crate::domain::model::quota::SettlementOutcome,
                crate::domain::error::DomainError,
            > {
                Err(crate::domain::error::DomainError::internal(
                    "injected settler failure",
                ))
            }
        }

        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let message_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let scope = AccessScope::allow_all();
        let conn = db.conn().unwrap();
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
                    requester_user_id: Some(user_id),
                    reserve_tokens: Some(5000),
                    max_output_tokens_applied: Some(4096),
                    reserved_credits_micro: Some(250),
                    policy_version_applied: Some(1),
                    effective_model: Some("gpt-4o-mini".to_owned()),
                    minimal_generation_floor_applied: Some(50),
                    web_search_enabled: false,
                },
            )
            .await
            .expect("create turn");

        let turn_repo_arc = Arc::new(TurnRepo);
        let message_repo_arc = Arc::new(MsgRepo::new(toolkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }));
        let finalization_svc = Arc::new(FinalizationService::new(
            Arc::clone(&db),
            Arc::clone(&turn_repo_arc),
            Arc::clone(&message_repo_arc),
            Arc::new(FailingSettler) as Arc<dyn QuotaSettler>,
            Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            crate::config::background::ThreadSummaryWorkerConfig::default(),
        ));

        let fctx = FinalizationCtx {
            finalization_svc,
            db: Arc::clone(&db),
            turn_repo: Arc::clone(&turn_repo_arc),
            scope,
            turn_id,
            tenant_id,
            chat_id,
            request_id,
            user_id,
            requester_type: RequesterType::User,
            message_id,
            effective_model: "gpt-4o-mini".to_owned(),
            selected_model: "gpt-4o".to_owned(),
            reserve_tokens: 5000,
            max_output_tokens_applied: 4096,
            reserved_credits_micro: 250,
            policy_version_applied: 1,
            minimal_generation_floor_applied: 50,
            quota_decision: "downgrade".to_owned(),
            downgrade_from: Some("gpt-4o".to_owned()),
            downgrade_reason: Some("premium_exhausted".to_owned()),
            period_starts: Vec::new(),
            context_window: 128_000,
            assembled_context_tokens: 0,
            messages_truncated: false,
            provider_id: "openai".to_owned(),
            metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
            quota_warnings_provider: Arc::new(NoopQuotaWarningsProvider),
        };

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let _handle = provider_task::spawn_provider_task(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "gpt-4o-mini".into(),
                provider_model_id: "gpt-4o-mini".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel,
            tx,
            Some(fctx),
        );

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        let done = events
            .iter()
            .find_map(|ev| match ev {
                StreamEvent::Done(d) => Some(d),
                _ => None,
            })
            .expect("should have a Done event even when finalization fails");

        assert_eq!(
            done.quota_decision, "downgrade",
            "fallback Done must use fctx.quota_decision, not hardcoded 'allow'"
        );
        assert_eq!(
            done.downgrade_from.as_deref(),
            Some("gpt-4o"),
            "fallback Done must use fctx.downgrade_from"
        );
        assert_eq!(
            done.downgrade_reason.as_deref(),
            Some("premium_exhausted"),
            "fallback Done must use fctx.downgrade_reason"
        );
    }

    /// 8.8: Done fallback on finalization failure preserves quota fields for the
    /// **incomplete** terminal path.
    ///
    /// Same invariant as 8.7 but triggered via `MockProvider::incomplete` so the
    /// `TerminalOutcome::Incomplete` branch is exercised.
    #[tokio::test]
    async fn done_fallback_preserves_quota_fields_on_finalization_failure_incomplete() {
        use crate::domain::service::finalization_service::FinalizationService;
        use crate::domain::service::quota_settler::QuotaSettler;

        #[allow(de0309_must_have_domain_model)]
        struct FailingSettler;
        #[async_trait::async_trait]
        impl QuotaSettler for FailingSettler {
            async fn settle_in_tx(
                &self,
                _tx: &toolkit_db::secure::DbTx<'_>,
                _scope: &AccessScope,
                _input: crate::domain::model::quota::SettlementInput,
            ) -> Result<
                crate::domain::model::quota::SettlementOutcome,
                crate::domain::error::DomainError,
            > {
                Err(crate::domain::error::DomainError::internal(
                    "injected settler failure",
                ))
            }
        }

        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let message_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let scope = AccessScope::allow_all();
        let conn = db.conn().unwrap();
        TurnRepo
            .create_turn(
                &conn,
                &scope,
                CreateTurnParams {
                    id: turn_id,
                    tenant_id,
                    chat_id,
                    request_id,
                    requester_type: "user".to_owned(),
                    requester_user_id: Some(user_id),
                    reserve_tokens: Some(5000),
                    max_output_tokens_applied: Some(4096),
                    reserved_credits_micro: Some(250),
                    policy_version_applied: Some(1),
                    effective_model: Some("gpt-4o-mini".to_owned()),
                    minimal_generation_floor_applied: Some(50),
                    web_search_enabled: false,
                },
            )
            .await
            .expect("create turn");

        let turn_repo_arc = Arc::new(TurnRepo);
        let message_repo_arc = Arc::new(MsgRepo::new(toolkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }));
        let finalization_svc = Arc::new(FinalizationService::new(
            Arc::clone(&db),
            Arc::clone(&turn_repo_arc),
            Arc::clone(&message_repo_arc),
            Arc::new(FailingSettler) as Arc<dyn QuotaSettler>,
            Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            crate::config::background::ThreadSummaryWorkerConfig::default(),
        ));

        let fctx = FinalizationCtx {
            finalization_svc,
            db: Arc::clone(&db),
            turn_repo: Arc::clone(&turn_repo_arc),
            scope,
            turn_id,
            tenant_id,
            chat_id,
            request_id,
            user_id,
            requester_type: RequesterType::User,
            message_id,
            effective_model: "gpt-4o-mini".to_owned(),
            selected_model: "gpt-4o".to_owned(),
            reserve_tokens: 5000,
            max_output_tokens_applied: 4096,
            reserved_credits_micro: 250,
            policy_version_applied: 1,
            minimal_generation_floor_applied: 50,
            quota_decision: "downgrade".to_owned(),
            downgrade_from: Some("gpt-4o".to_owned()),
            downgrade_reason: Some("premium_exhausted".to_owned()),
            period_starts: Vec::new(),
            context_window: 128_000,
            assembled_context_tokens: 0,
            messages_truncated: false,
            provider_id: "openai".to_owned(),
            metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
            quota_warnings_provider: Arc::new(NoopQuotaWarningsProvider),
        };

        let provider: Arc<dyn LlmProvider> =
            Arc::new(MockProvider::incomplete(&["Hello", ", wor"]));
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let _handle = provider_task::spawn_provider_task(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "gpt-4o-mini".into(),
                provider_model_id: "gpt-4o-mini".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel,
            tx,
            Some(fctx),
        );

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        let done = events
            .iter()
            .find_map(|ev| match ev {
                StreamEvent::Done(d) => Some(d),
                _ => None,
            })
            .expect("should have a Done event even when finalization fails on incomplete path");

        assert_eq!(
            done.quota_decision, "downgrade",
            "incomplete fallback Done must use fctx.quota_decision, not hardcoded 'allow'"
        );
        assert_eq!(
            done.downgrade_from.as_deref(),
            Some("gpt-4o"),
            "incomplete fallback Done must use fctx.downgrade_from"
        );
        assert_eq!(
            done.downgrade_reason.as_deref(),
            Some("premium_exhausted"),
            "incomplete fallback Done must use fctx.downgrade_reason"
        );
    }

    // ── Preflight wiring tests (11.x) ──

    fn make_catalog_entry(
        id: &str,
        tier: mini_chat_sdk::ModelTier,
    ) -> mini_chat_sdk::ModelCatalogEntry {
        test_catalog_entry(TestCatalogEntryParams {
            model_id: id.to_owned(),
            provider_model_id: format!("provider-{id}"),
            display_name: id.to_owned(),
            tier,
            enabled: true,
            is_default: tier == mini_chat_sdk::ModelTier::Standard,
            input_tokens_credit_multiplier_micro: 1_000_000,
            output_tokens_credit_multiplier_micro: 1_000_000,
            multimodal_capabilities: vec![],
            context_window: 128_000,
            max_output_tokens: 4096,
            description: String::new(),
            provider_display_name: String::new(),
            multiplier_display: "1x".to_owned(),
            provider_id: "openai".to_owned(),
        })
    }

    fn build_stream_service_with_catalog(
        db: Arc<DbProvider>,
        provider: Arc<dyn LlmProvider>,
        catalog: Vec<mini_chat_sdk::ModelCatalogEntry>,
        limits: mini_chat_sdk::UserLimits,
    ) -> StreamService<
        TurnRepo,
        MsgRepo,
        OrmQuotaUsageRepo,
        OrmChatRepo,
        MockThreadSummaryRepo,
        OrmAttachmentRepo,
        OrmVectorStoreRepo,
        OrmMessageAttachmentRepo,
    > {
        use crate::domain::service::finalization_service::FinalizationService;
        use crate::domain::service::quota_settler::QuotaSettler;

        #[allow(de0309_must_have_domain_model)]
        struct MockQuotaSettler;
        #[async_trait::async_trait]
        impl QuotaSettler for MockQuotaSettler {
            async fn settle_in_tx(
                &self,
                _tx: &toolkit_db::secure::DbTx<'_>,
                _scope: &AccessScope,
                _input: crate::domain::model::quota::SettlementInput,
            ) -> Result<
                crate::domain::model::quota::SettlementOutcome,
                crate::domain::error::DomainError,
            > {
                Ok(crate::domain::model::quota::SettlementOutcome {
                    settlement_method: crate::domain::model::quota::SettlementMethod::Released,
                    actual_credits_micro: 0,
                    charged_tokens: 0,
                    overshoot_capped: false,
                })
            }
        }

        let provider_resolver = Arc::new(ProviderResolver::single_provider(provider));
        let turn_repo = Arc::new(TurnRepo);
        let message_repo = Arc::new(MsgRepo::new(toolkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }));
        let finalization = Arc::new(FinalizationService::new(
            Arc::clone(&db),
            Arc::clone(&turn_repo),
            Arc::clone(&message_repo),
            Arc::new(MockQuotaSettler) as Arc<dyn QuotaSettler>,
            Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            crate::config::background::ThreadSummaryWorkerConfig::default(),
        ));

        let quota_svc = Arc::new(crate::domain::service::QuotaService::new(
            Arc::clone(&db),
            Arc::new(OrmQuotaUsageRepo),
            Arc::new(MockPolicySnapshotProvider::new(
                mini_chat_sdk::PolicySnapshot {
                    user_id: Uuid::nil(),
                    policy_version: 1,
                    model_catalog: catalog,
                    kill_switches: mini_chat_sdk::KillSwitches::default(),
                },
            )),
            Arc::new(MockUserLimitsProvider::new(limits)),
            crate::config::EstimationBudgets::default(),
            crate::config::QuotaConfig {
                overshoot_tolerance_factor: 1.10,
                ..crate::config::QuotaConfig::default()
            },
        ));

        StreamService::new(
            db,
            turn_repo,
            message_repo,
            Arc::new(OrmChatRepo::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            })),
            mock_enforcer(),
            provider_resolver,
            crate::config::StreamingConfig::default(),
            finalization,
            quota_svc,
            mock_thread_summary_repo(),
            Arc::new(crate::infra::db::repo::attachment_repo::AttachmentRepository),
            Arc::new(crate::infra::db::repo::vector_store_repo::VectorStoreRepository),
            Arc::new(crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository),
            crate::config::ContextConfig::default(),
            crate::config::RagConfig::default(),
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            crate::config::KnowledgeSearchConfig::default(),
            None,
            std::collections::HashMap::new(),
        )
    }

    fn permissive_limits() -> mini_chat_sdk::UserLimits {
        mini_chat_sdk::UserLimits {
            user_id: Uuid::nil(),
            policy_version: 1,
            standard: mini_chat_sdk::TierLimits {
                limit_daily_credits_micro: 100_000_000,
                limit_monthly_credits_micro: 1_000_000_000,
            },
            premium: mini_chat_sdk::TierLimits {
                limit_daily_credits_micro: 50_000_000,
                limit_monthly_credits_micro: 500_000_000,
            },
        }
    }

    /// 11.1: Allow path populates `FinalizationCtx` with real quota fields.
    #[tokio::test]
    async fn preflight_allow_populates_real_quota_fields() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
        let catalog = vec![make_catalog_entry(
            "gpt-5.2",
            mini_chat_sdk::ModelTier::Standard,
        )];
        let svc =
            build_stream_service_with_catalog(db.clone(), provider, catalog, permissive_limits());

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let handle = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect("should succeed");

        // Drain events
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        let _outcome = handle.await.expect("task should complete");

        // Verify Done event has allow quota_decision
        let done = events
            .iter()
            .find_map(|ev| match ev {
                StreamEvent::Done(d) => Some(d),
                _ => None,
            })
            .expect("should have a Done event");

        assert_eq!(done.quota_decision, "allow");
        assert_eq!(done.effective_model, "gpt-5.2");
        assert_eq!(done.selected_model, "gpt-5.2");
        assert!(done.downgrade_from.is_none());

        // Verify turn was created with real quota fields (not placeholder 1_000_000)
        let scope = AccessScope::allow_all();
        let conn = db.conn().unwrap();
        let turn_repo = TurnRepo;
        let turn = turn_repo
            .find_latest_turn(&conn, &scope, chat_id)
            .await
            .expect("find turn")
            .expect("turn should exist");

        assert!(
            turn.reserve_tokens.unwrap() < 1_000_000,
            "reserve_tokens should be from real preflight, not placeholder 1_000_000"
        );
        assert!(
            turn.reserved_credits_micro.unwrap() > 0,
            "reserved_credits_micro should be from real preflight, not placeholder 0"
        );
    }

    /// 11.3: Reject path returns `StreamError::QuotaExhausted`.
    #[tokio::test]
    async fn preflight_reject_returns_quota_exhausted() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
        // Exhausted limits: 0 credits for all tiers
        let limits = mini_chat_sdk::UserLimits {
            user_id: Uuid::nil(),
            policy_version: 1,
            standard: mini_chat_sdk::TierLimits {
                limit_daily_credits_micro: 0,
                limit_monthly_credits_micro: 0,
            },
            premium: mini_chat_sdk::TierLimits {
                limit_daily_credits_micro: 0,
                limit_monthly_credits_micro: 0,
            },
        };
        let catalog = vec![make_catalog_entry(
            "gpt-5.2",
            mini_chat_sdk::ModelTier::Standard,
        )];
        let svc = build_stream_service_with_catalog(db, provider, catalog, limits);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, _rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let err = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect_err("should be QuotaExhausted");

        match err {
            StreamError::QuotaExhausted {
                error_code,
                http_status,
                ..
            } => {
                assert_eq!(http_status, 429);
                assert!(!error_code.is_empty());
            }
            other => panic!("expected QuotaExhausted, got: {other:?}"),
        }
    }

    /// 11.2: Downgrade path sets `effective_model` != `selected_model`.
    #[tokio::test]
    async fn preflight_downgrade_sets_correct_model_fields() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
        // Catalog with premium model "gpt-5" and standard fallback "gpt-5-mini"
        let catalog = vec![
            make_catalog_entry("gpt-5", mini_chat_sdk::ModelTier::Premium),
            make_catalog_entry("gpt-5-mini", mini_chat_sdk::ModelTier::Standard),
        ];
        // Premium limits are 0 → forces downgrade to standard
        let limits = mini_chat_sdk::UserLimits {
            user_id: Uuid::nil(),
            policy_version: 1,
            standard: mini_chat_sdk::TierLimits {
                limit_daily_credits_micro: 100_000_000,
                limit_monthly_credits_micro: 1_000_000_000,
            },
            premium: mini_chat_sdk::TierLimits {
                limit_daily_credits_micro: 0,
                limit_monthly_credits_micro: 0,
            },
        };
        let svc = build_stream_service_with_catalog(db, provider, catalog, limits);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let handle = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "hello".into(),
                ResolvedModel {
                    model_id: "gpt-5".into(),
                    provider_model_id: "gpt-5-2025-03-26".into(),
                    provider_id: "openai".into(),
                    display_name: "GPT 5".into(),
                    tier: "premium".into(),
                    multiplier_display: "2x".into(),
                    description: None,
                    multimodal_capabilities: vec![],
                    context_window: 128_000,
                    max_file_size_mb: 25,
                    system_prompt: String::new(),
                    tool_support: mini_chat_sdk::ModelToolSupport {
                        web_search: false,
                        file_search: false,
                        image_generation: false,
                        code_interpreter: false,
                        mcp: false,
                    },
                    thread_summary_prompt: String::new(),
                    max_output_tokens: 16_384,
                    enabled_function_tools: Vec::new(),
                },
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect("should succeed (downgrade, not reject)");

        // Drain events
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        let _outcome = handle.await.expect("task should complete");

        // Verify Done event reflects downgrade
        let done = events
            .iter()
            .find_map(|ev| match ev {
                StreamEvent::Done(d) => Some(d),
                _ => None,
            })
            .expect("should have a Done event");

        assert_eq!(done.quota_decision, "downgrade");
        assert_eq!(
            done.effective_model, "gpt-5-mini",
            "should be downgraded model"
        );
        assert_eq!(
            done.selected_model, "gpt-5",
            "should be original selected model"
        );
        assert_eq!(done.downgrade_from.as_deref(), Some("gpt-5"));
    }

    /// 11.4: On downgrade the LLM provider receives the **downgraded** model's
    /// `provider_model_id`, not the originally-requested premium model's.
    ///
    /// Regression: before `effective_provider_model_id` was added to
    /// `PreflightDecision`, the provider always received the premium model's
    /// provider ID even after a quota downgrade, so users got premium
    /// responses billed at standard rates.
    #[tokio::test]
    async fn downgrade_sends_effective_provider_model_id_to_llm() {
        // ── Capturing provider that records the model from LlmRequest ──
        #[domain_model]
        struct CapturingProvider {
            captured_model: std::sync::Mutex<Option<String>>,
            inner: MockProvider,
        }

        impl CapturingProvider {
            fn new(inner: MockProvider) -> Self {
                Self {
                    captured_model: std::sync::Mutex::new(None),
                    inner,
                }
            }

            fn captured_model(&self) -> String {
                self.captured_model
                    .lock()
                    .unwrap()
                    .clone()
                    .expect("provider was never called")
            }
        }

        #[async_trait::async_trait]
        impl LlmProvider for CapturingProvider {
            async fn stream(
                &self,
                ctx: SecurityContext,
                request: LlmRequest<Streaming>,
                upstream_alias: &str,
                cancel: CancellationToken,
            ) -> Result<ProviderStream, LlmProviderError> {
                *self.captured_model.lock().unwrap() = Some(request.model().to_owned());
                self.inner
                    .stream(ctx, request, upstream_alias, cancel)
                    .await
            }

            async fn complete(
                &self,
                _ctx: SecurityContext,
                _request: LlmRequest<NonStreaming>,
                _upstream_alias: &str,
            ) -> Result<ResponseResult, LlmProviderError> {
                unimplemented!("not needed for streaming tests")
            }
        }

        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let capturing = Arc::new(CapturingProvider::new(MockProvider::completed(&["Hi"])));
        let provider: Arc<dyn LlmProvider> = Arc::clone(&capturing) as _;

        // Catalog: premium "gpt-5" (provider_model_id = "provider-gpt-5")
        //          standard "gpt-5-mini" (provider_model_id = "provider-gpt-5-mini")
        let catalog = vec![
            make_catalog_entry("gpt-5", mini_chat_sdk::ModelTier::Premium),
            make_catalog_entry("gpt-5-mini", mini_chat_sdk::ModelTier::Standard),
        ];

        // Premium limits = 0 → forces downgrade to standard
        let limits = mini_chat_sdk::UserLimits {
            user_id: Uuid::nil(),
            policy_version: 1,
            standard: mini_chat_sdk::TierLimits {
                limit_daily_credits_micro: 100_000_000,
                limit_monthly_credits_micro: 1_000_000_000,
            },
            premium: mini_chat_sdk::TierLimits {
                limit_daily_credits_micro: 0,
                limit_monthly_credits_micro: 0,
            },
        };
        let svc = build_stream_service_with_catalog(db, provider, catalog, limits);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        // Request premium "gpt-5" with its own provider_model_id.
        // After downgrade the provider must NOT see this ID.
        let handle = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "hello".into(),
                ResolvedModel {
                    model_id: "gpt-5".into(),
                    provider_model_id: "gpt-5-2025-03-26".into(),
                    provider_id: "openai".into(),
                    display_name: "GPT 5".into(),
                    tier: "premium".into(),
                    multiplier_display: "2x".into(),
                    description: None,
                    multimodal_capabilities: vec![],
                    context_window: 128_000,
                    max_file_size_mb: 25,
                    system_prompt: String::new(),
                    tool_support: mini_chat_sdk::ModelToolSupport {
                        web_search: false,
                        file_search: false,
                        image_generation: false,
                        code_interpreter: false,
                        mcp: false,
                    },
                    thread_summary_prompt: String::new(),
                    max_output_tokens: 16_384,
                    enabled_function_tools: Vec::new(),
                },
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect("should succeed (downgrade, not reject)");

        // Drain events
        while let Some(ev) = rx.recv().await {
            if ev.is_terminal() {
                break;
            }
        }
        let _outcome = handle.await.expect("task should complete");

        // The provider must have received the standard model's provider_model_id,
        // NOT the premium "gpt-5-2025-03-26" from ResolvedModel.
        assert_eq!(
            capturing.captured_model(),
            "provider-gpt-5-mini",
            "provider should receive the downgraded model's provider_model_id, \
             not the originally-requested premium model's"
        );
    }

    /// 11.5: When a chat has ready file-search documents, preflight must
    /// include `tool_surcharge_tokens` in the reservation estimate.
    ///
    /// Regression: `tools_enabled` was hardcoded to `false`, so
    /// `tool_surcharge_tokens` was never added to the token estimate,
    /// under-reserving quota for chats with attachments.
    #[tokio::test]
    async fn preflight_includes_tool_surcharge_when_documents_present() {
        use crate::domain::service::test_helpers::{
            InsertTestAttachmentParams, insert_test_attachment,
        };

        // ── Run 1: baseline — no documents ──
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id_no_docs = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id_no_docs).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hi"]));
        let catalog = vec![make_catalog_entry(
            "gpt-5.2",
            mini_chat_sdk::ModelTier::Standard,
        )];
        let svc =
            build_stream_service_with_catalog(db.clone(), provider, catalog, permissive_limits());

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let handle = svc
            .run_stream(
                ctx,
                chat_id_no_docs,
                Uuid::new_v4(),
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect("run_stream without docs should succeed");

        while let Some(ev) = rx.recv().await {
            if ev.is_terminal() {
                break;
            }
        }
        let _outcome = handle.await.expect("task should complete");

        let scope = AccessScope::allow_all();
        let conn = db.conn().unwrap();
        let turn_repo = TurnRepo;
        let turn_no_docs = turn_repo
            .find_latest_turn(&conn, &scope, chat_id_no_docs)
            .await
            .expect("find turn")
            .expect("turn should exist");
        let tokens_no_docs = turn_no_docs.reserve_tokens.unwrap();

        // ── Run 2: with a ready file-search document ──
        let chat_id_with_docs = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id_with_docs).await;

        insert_test_attachment(
            &db,
            InsertTestAttachmentParams::ready_document(tenant_id, chat_id_with_docs),
        )
        .await;

        let provider2: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hi"]));
        let catalog2 = vec![make_catalog_entry(
            "gpt-5.2",
            mini_chat_sdk::ModelTier::Standard,
        )];
        let svc2 =
            build_stream_service_with_catalog(db.clone(), provider2, catalog2, permissive_limits());

        let ctx2 = test_security_ctx_with_id(tenant_id, user_id);
        let (tx2, mut rx2) = mpsc::channel(32);
        let cancel2 = CancellationToken::new();

        let handle2 = svc2
            .run_stream(
                ctx2,
                chat_id_with_docs,
                Uuid::new_v4(),
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel2,
                tx2,
            )
            .await
            .expect("run_stream with docs should succeed");

        while let Some(ev) = rx2.recv().await {
            if ev.is_terminal() {
                break;
            }
        }
        let _outcome2 = handle2.await.expect("task should complete");

        let turn_with_docs = turn_repo
            .find_latest_turn(&conn, &scope, chat_id_with_docs)
            .await
            .expect("find turn")
            .expect("turn should exist");
        let tokens_with_docs = turn_with_docs.reserve_tokens.unwrap();

        // The default tool_surcharge_tokens is 500. With a document present,
        // reserve_tokens must be strictly higher than without.
        assert!(
            tokens_with_docs > tokens_no_docs,
            "reserve_tokens with documents ({tokens_with_docs}) should exceed \
             reserve_tokens without documents ({tokens_no_docs}) by tool_surcharge_tokens"
        );
    }

    /// Non-existent chat returns `ChatNotFound`.
    #[tokio::test]
    async fn run_stream_nonexistent_chat_returns_chat_not_found() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let bogus_chat_id = Uuid::new_v4();
        // No chat inserted

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db, provider);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, _rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let err = svc
            .run_stream(
                ctx,
                bogus_chat_id,
                Uuid::new_v4(),
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect_err("should be ChatNotFound");

        match err {
            StreamError::ChatNotFound { chat_id } => {
                assert_eq!(chat_id, bogus_chat_id);
            }
            other => panic!("expected ChatNotFound, got: {other:?}"),
        }
    }

    // ── Per-message web search call limit tests ──

    /// 6.5: Web search calls within limit — stream completes normally.
    #[tokio::test]
    async fn test_per_message_limit_not_exceeded() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_web_search_calls(2)); // 2 calls, limit is 2
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "test-model".into(),
                provider_model_id: "test-model".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel,
            tx,
            None,
        );

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        let outcome = handle.await.expect("task should not panic");
        assert_eq!(outcome.terminal, StreamTerminal::Completed);

        // Expect: 1 delta + 2*(start+done) tool events + 1 done = 6 events
        assert_eq!(events.len(), 6);
        assert!(matches!(events.last(), Some(StreamEvent::Done(_))));
        // No error events
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Error(_))));
    }

    /// 6.6: Web search calls exceed limit — stream terminates with error.
    #[tokio::test]
    async fn test_per_message_limit_exceeded() {
        // 3 web search calls but limit is 2 — the 3rd start should trigger error
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_web_search_calls(3));
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "test-model".into(),
                provider_model_id: "test-model".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel,
            tx,
            None,
        );

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        let outcome = handle.await.expect("task should not panic");
        assert_eq!(outcome.terminal, StreamTerminal::Failed);
        assert_eq!(
            outcome.error_code.as_deref(),
            Some("web_search_calls_exceeded")
        );

        // Last event should be an error
        let last = events.last().expect("should have events");
        match last {
            StreamEvent::Error(data) => {
                assert_eq!(data.code, "web_search_calls_exceeded");
            }
            other => panic!("expected Error event, got: {other:?}"),
        }
    }

    /// 6.7: Other tool calls don't count toward web search limit.
    #[tokio::test]
    async fn test_per_message_counter_ignores_other_tools() {
        // 5 file_search calls + 1 web_search call, limit is 2 — should complete
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_tool_calls(&[
            ("file_search", 5),
            ("web_search", 1),
        ]));
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "test-model".into(),
                provider_model_id: "test-model".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel,
            tx,
            None,
        );

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        let outcome = handle.await.expect("task should not panic");
        assert_eq!(outcome.terminal, StreamTerminal::Completed);
        // No error events
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Error(_))));
    }

    // ── Per-message code interpreter call limit tests ──

    /// 6.8: Code interpreter calls within limit — stream completes normally.
    #[tokio::test]
    async fn test_ci_per_message_limit_not_exceeded() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_code_interpreter_calls(2)); // 2 calls, limit is 2
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "test-model".into(),
                provider_model_id: "test-model".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel,
            tx,
            None,
        );

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        let outcome = handle.await.expect("task should not panic");
        assert_eq!(outcome.terminal, StreamTerminal::Completed);

        // Expect: 1 delta + 2*(start+done) tool events + 1 done = 6 events
        assert_eq!(events.len(), 6);
        assert!(matches!(events.last(), Some(StreamEvent::Done(_))));
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Error(_))));
    }

    /// 6.9: Code interpreter calls exceed limit — stream terminates with error.
    #[tokio::test]
    async fn test_ci_per_message_limit_exceeded() {
        // 3 code_interpreter calls but limit is 2 — the 3rd start should trigger error
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_code_interpreter_calls(3));
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "test-model".into(),
                provider_model_id: "test-model".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel,
            tx,
            None,
        );

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        let outcome = handle.await.expect("task should not panic");
        assert_eq!(outcome.terminal, StreamTerminal::Failed);
        assert_eq!(
            outcome.error_code.as_deref(),
            Some("code_interpreter_calls_exceeded")
        );

        // Last event should be an error
        let last = events.last().expect("should have events");
        match last {
            StreamEvent::Error(data) => {
                assert_eq!(data.code, "code_interpreter_calls_exceeded");
            }
            other => panic!("expected Error event, got: {other:?}"),
        }
    }

    /// 6.10: Other tool calls don't count toward code interpreter limit.
    #[tokio::test]
    async fn test_ci_per_message_counter_ignores_other_tools() {
        // 5 file_search calls + 1 code_interpreter call, limit is 2 — should complete
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_tool_calls(&[
            ("file_search", 5),
            ("code_interpreter", 1),
        ]));
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = provider_task::spawn_provider_task::<TurnRepo, MsgRepo>(
            mock_ctx(),
            provider_task::ProviderTaskConfig {
                llm: provider,
                upstream_alias: "test-alias".to_owned(),
                messages: vec![LlmMessage::user("hi")],
                system_instructions: None,
                tools: vec![],
                model: "test-model".into(),
                provider_model_id: "test-model".into(),
                max_output_tokens: 4096,
                max_tool_calls: 2,
                web_search_max_calls: 2,
                code_interpreter_max_calls: 2,
                api_params: mini_chat_sdk::ModelApiParams {
                    temperature: 0.7,
                    top_p: 1.0,
                    frequency_penalty: 0.0,
                    presence_penalty: 0.0,
                    stop: vec![],
                    extra_body: None,
                    reasoning_effort: None,
                },
                provider_file_id_map: std::collections::HashMap::new(),
                anthropic_file_ids: std::collections::HashMap::new(),
                knowledge_search: None,
                function_tools: std::collections::HashMap::new(),
            },
            cancel,
            tx,
            None,
        );

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            events.push(ev);
            if is_term {
                break;
            }
        }

        let outcome = handle.await.expect("task should not panic");
        assert_eq!(outcome.terminal, StreamTerminal::Completed);
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Error(_))));
    }

    // ── P5-I: SendMessage Attachment Validation (negative) ──

    use crate::domain::service::test_helpers::{
        InsertTestAttachmentParams, insert_test_attachment, insert_test_vector_store,
    };
    use crate::infra::db::entity::attachment::AttachmentStatus;

    /// Helper: call `run_stream` with given `attachment_ids`, expect `StreamError::InvalidAttachment`.
    async fn run_stream_expect_invalid_attachment(
        svc: &StreamService<
            TurnRepo,
            MsgRepo,
            OrmQuotaUsageRepo,
            OrmChatRepo,
            MockThreadSummaryRepo,
            OrmAttachmentRepo,
            OrmVectorStoreRepo,
            OrmMessageAttachmentRepo,
        >,
        tenant_id: Uuid,
        user_id: Uuid,
        chat_id: Uuid,
        attachment_ids: Vec<Uuid>,
    ) -> StreamError {
        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, _rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();
        svc.run_stream(
            ctx,
            chat_id,
            Uuid::new_v4(),
            "test message".into(),
            test_resolved_model(),
            false,
            attachment_ids,
            cancel,
            tx,
        )
        .await
        .expect_err("should fail with InvalidAttachment")
    }

    /// P5-I1: Nonexistent attachment UUID → `InvalidAttachment` error.
    #[tokio::test]
    async fn send_message_nonexistent_attachment_id() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        let err = run_stream_expect_invalid_attachment(
            &svc,
            tenant_id,
            user_id,
            chat_id,
            vec![Uuid::new_v4()],
        )
        .await;

        assert!(
            matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("not found")),
            "expected 'not found', got: {err:?}"
        );
    }

    /// P5-I2: Soft-deleted attachment → `InvalidAttachment` error.
    #[tokio::test]
    async fn send_message_deleted_attachment_id() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let att_id = insert_test_attachment(
            &db,
            InsertTestAttachmentParams {
                uploaded_by_user_id: user_id,
                deleted_at: Some(time::OffsetDateTime::now_utc()),
                ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
            },
        )
        .await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        let err =
            run_stream_expect_invalid_attachment(&svc, tenant_id, user_id, chat_id, vec![att_id])
                .await;

        assert!(
            matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("deleted")),
            "expected 'deleted', got: {err:?}"
        );
    }

    /// P5-I3: Pending (not ready) attachment → `InvalidAttachment` error.
    #[tokio::test]
    async fn send_message_pending_attachment_id() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let att_id = insert_test_attachment(
            &db,
            InsertTestAttachmentParams {
                uploaded_by_user_id: user_id,
                status: AttachmentStatus::Pending,
                provider_file_id: None,
                ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
            },
        )
        .await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        let err =
            run_stream_expect_invalid_attachment(&svc, tenant_id, user_id, chat_id, vec![att_id])
                .await;

        assert!(
            matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("not ready")),
            "expected 'not ready', got: {err:?}"
        );
    }

    /// P5-I4: Failed attachment → `InvalidAttachment` error.
    #[tokio::test]
    async fn send_message_failed_attachment_id() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let att_id = insert_test_attachment(
            &db,
            InsertTestAttachmentParams {
                uploaded_by_user_id: user_id,
                status: AttachmentStatus::Failed,
                provider_file_id: None,
                error_code: Some("upload_failed".to_owned()),
                ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
            },
        )
        .await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        let err =
            run_stream_expect_invalid_attachment(&svc, tenant_id, user_id, chat_id, vec![att_id])
                .await;

        assert!(
            matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("not ready")),
            "expected 'not ready', got: {err:?}"
        );
    }

    /// P5-I5: Attachment from a different chat → `InvalidAttachment` error.
    #[tokio::test]
    async fn send_message_attachment_from_different_chat() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_a = Uuid::new_v4();
        let chat_b = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_a).await;
        insert_test_chat(&db, tenant_id, user_id, chat_b).await;

        // Attachment belongs to chat_b
        let att_id = insert_test_attachment(
            &db,
            InsertTestAttachmentParams {
                uploaded_by_user_id: user_id,
                ..InsertTestAttachmentParams::ready_document(tenant_id, chat_b)
            },
        )
        .await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        // Try to use it in chat_a
        let err =
            run_stream_expect_invalid_attachment(&svc, tenant_id, user_id, chat_a, vec![att_id])
                .await;

        assert!(
            matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("does not belong")),
            "expected 'does not belong', got: {err:?}"
        );
    }

    /// P5-I6: Attachment owned by different user → `InvalidAttachment` error.
    #[tokio::test]
    async fn send_message_attachment_wrong_owner() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let other_user = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        // Attachment uploaded by other_user
        let att_id = insert_test_attachment(
            &db,
            InsertTestAttachmentParams {
                uploaded_by_user_id: other_user,
                ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
            },
        )
        .await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        let err =
            run_stream_expect_invalid_attachment(&svc, tenant_id, user_id, chat_id, vec![att_id])
                .await;

        assert!(
            matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("not owned")),
            "expected 'not owned', got: {err:?}"
        );
    }

    /// P5-I7: Duplicate attachment IDs in request → `InvalidAttachment` error.
    #[tokio::test]
    async fn send_message_duplicate_attachment_ids() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let att_id = insert_test_attachment(
            &db,
            InsertTestAttachmentParams {
                uploaded_by_user_id: user_id,
                ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
            },
        )
        .await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["hi"]));
        let svc = build_stream_service(db.clone(), provider);

        // Same UUID twice
        let err = run_stream_expect_invalid_attachment(
            &svc,
            tenant_id,
            user_id,
            chat_id,
            vec![att_id, att_id],
        )
        .await;

        assert!(
            matches!(err, StreamError::InvalidAttachment { ref message, .. } if message.contains("Duplicate")),
            "expected 'Duplicate', got: {err:?}"
        );
    }

    // ── P5-H: SendMessage with Attachments (positive) ──

    /// P5-H1: Valid `attachment_ids` → `message_attachments` persisted, stream completes.
    #[tokio::test]
    async fn send_message_with_valid_attachment_ids() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let att_id = insert_test_attachment(
            &db,
            InsertTestAttachmentParams {
                uploaded_by_user_id: user_id,
                ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
            },
        )
        .await;

        // Insert vector store so file_search can activate
        insert_test_vector_store(&db, tenant_id, chat_id, Some("vs_test123".to_owned())).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["the answer"]));
        let svc = build_stream_service(db.clone(), provider);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let result = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "summarize the doc".into(),
                test_resolved_model(),
                false,
                vec![att_id],
                cancel,
                tx,
            )
            .await;

        assert!(result.is_ok(), "run_stream should succeed: {result:?}");

        // Drain events — should complete without error
        let mut got_done = false;
        while let Some(ev) = rx.recv().await {
            if ev.is_terminal() {
                got_done = matches!(ev, StreamEvent::Done(_));
                break;
            }
        }
        assert!(got_done, "stream should complete with Done event");

        // Verify message_attachments row persisted
        let conn = db.conn().unwrap();
        let scope = AccessScope::allow_all();
        let repo = OrmMessageAttachmentRepo;
        let exists = repo
            .exists_for_attachment(&conn, &scope, att_id)
            .await
            .expect("exists_for_attachment");
        assert!(
            exists,
            "message_attachment row should exist for the attachment"
        );
    }

    /// P5-H3: Provider file citations mapped to internal UUID end-to-end.
    #[tokio::test]
    async fn send_message_citation_mapping_end_to_end() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let provider_file_id = "file-abc123";
        let att_id = insert_test_attachment(
            &db,
            InsertTestAttachmentParams {
                uploaded_by_user_id: user_id,
                provider_file_id: Some(provider_file_id.to_owned()),
                ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
            },
        )
        .await;

        insert_test_vector_store(&db, tenant_id, chat_id, Some("vs_cit1".to_owned())).await;

        // Provider returns a file citation with the provider's file_id
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed_with_citations(
            &["Kinbote City"],
            vec![Citation {
                source: CitationSource::File,
                title: "test.pdf".to_owned(),
                url: None,
                attachment_id: Some(provider_file_id.to_owned()),
                snippet: "capital of Zembla".to_owned(),
                score: Some(0.95),
                span: None,
            }],
        ));
        let svc = build_stream_service(db.clone(), provider);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let result = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "What is the capital?".into(),
                test_resolved_model(),
                false,
                vec![att_id],
                cancel,
                tx,
            )
            .await;
        assert!(result.is_ok(), "run_stream failed: {result:?}");

        let mut citation_events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            if matches!(ev, StreamEvent::Citations(_)) {
                citation_events.push(ev);
            }
            if is_term {
                break;
            }
        }

        // Should have a citations event with the internal UUID, not "file-abc123"
        assert_eq!(citation_events.len(), 1, "expected 1 citations event");
        if let StreamEvent::Citations(data) = &citation_events[0] {
            assert_eq!(data.items.len(), 1);
            let cit = &data.items[0];
            assert_eq!(
                cit.attachment_id.as_deref(),
                Some(att_id.to_string().as_str())
            );
            assert!(!cit.attachment_id.as_deref().unwrap().starts_with("file-"));
        } else {
            panic!("expected Citations event");
        }
    }

    /// P5-H4: Web citations pass through unchanged.
    #[tokio::test]
    async fn send_message_web_citations_passthrough() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let att_id = insert_test_attachment(
            &db,
            InsertTestAttachmentParams {
                uploaded_by_user_id: user_id,
                ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
            },
        )
        .await;

        insert_test_vector_store(&db, tenant_id, chat_id, Some("vs_web1".to_owned())).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed_with_citations(
            &["result"],
            vec![Citation {
                source: CitationSource::Web,
                title: "Example Page".to_owned(),
                url: Some("https://example.com/page".to_owned()),
                attachment_id: None,
                snippet: "some web content".to_owned(),
                score: None,
                span: None,
            }],
        ));
        let svc = build_stream_service(db.clone(), provider);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let result = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "search the web".into(),
                test_resolved_model(),
                false,
                vec![att_id],
                cancel,
                tx,
            )
            .await;
        assert!(result.is_ok(), "run_stream failed: {result:?}");

        let mut citation_events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let is_term = ev.is_terminal();
            if matches!(ev, StreamEvent::Citations(_)) {
                citation_events.push(ev);
            }
            if is_term {
                break;
            }
        }

        assert_eq!(citation_events.len(), 1, "expected 1 citations event");
        if let StreamEvent::Citations(data) = &citation_events[0] {
            assert_eq!(data.items.len(), 1);
            let cit = &data.items[0];
            assert!(matches!(cit.source, CitationSource::Web));
            assert_eq!(cit.url.as_deref(), Some("https://example.com/page"));
            assert_eq!(cit.title, "Example Page");
        } else {
            panic!("expected Citations event");
        }
    }

    /// P5-H2: Empty `attachment_ids` with ready docs → stream completes (unrestricted search).
    #[tokio::test]
    async fn send_message_no_attachments_with_docs() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        // Insert a ready doc (no attachment_ids passed to message)
        insert_test_attachment(
            &db,
            InsertTestAttachmentParams {
                uploaded_by_user_id: user_id,
                ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
            },
        )
        .await;

        insert_test_vector_store(&db, tenant_id, chat_id, Some("vs_test456".to_owned())).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["answer"]));
        let svc = build_stream_service(db.clone(), provider);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let result = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "question about docs".into(),
                test_resolved_model(),
                false,
                vec![], // no attachment_ids
                cancel,
                tx,
            )
            .await;

        assert!(result.is_ok(), "run_stream should succeed: {result:?}");

        let mut got_done = false;
        while let Some(ev) = rx.recv().await {
            if ev.is_terminal() {
                got_done = matches!(ev, StreamEvent::Done(_));
                break;
            }
        }
        assert!(got_done, "stream should complete with Done event");
    }

    // ── Mutation RAG wiring tests (WS2: 2.7–2.9) ──

    /// Insert a running turn row (required by mutation stream finalization CAS).
    async fn insert_running_turn(
        db: &Arc<DbProvider>,
        tenant_id: Uuid,
        user_id: Uuid,
        chat_id: Uuid,
        request_id: Uuid,
        turn_id: Uuid,
    ) {
        use crate::infra::db::entity::chat_turn::{
            ActiveModel as TurnAM, Entity as TurnEntity, TurnState,
        };
        use sea_orm::Set;
        use time::OffsetDateTime;
        use toolkit_db::secure::secure_insert;

        let now = OffsetDateTime::now_utc();
        let am = TurnAM {
            id: Set(turn_id),
            tenant_id: Set(tenant_id),
            chat_id: Set(chat_id),
            request_id: Set(request_id),
            requester_type: Set("user".to_owned()),
            requester_user_id: Set(Some(user_id)),
            state: Set(TurnState::Running),
            provider_name: Set(None),
            provider_response_id: Set(None),
            assistant_message_id: Set(None),
            error_code: Set(None),
            error_detail: Set(None),
            reserve_tokens: Set(None),
            max_output_tokens_applied: Set(None),
            reserved_credits_micro: Set(None),
            policy_version_applied: Set(None),
            effective_model: Set(None),
            minimal_generation_floor_applied: Set(None),
            web_search_enabled: Set(false),
            web_search_completed_count: Set(0),
            code_interpreter_completed_count: Set(0),
            file_search_completed_count: Set(0),
            deleted_at: Set(None),
            replaced_by_request_id: Set(None),
            started_at: Set(now),
            last_progress_at: Set(Some(now)),
            completed_at: Set(None),
            updated_at: Set(now),
        };
        let conn = db.conn().unwrap();
        secure_insert::<TurnEntity>(am, &AccessScope::allow_all(), &conn)
            .await
            .expect("insert running turn");
    }

    fn build_stream_service_with_policy(
        db: Arc<DbProvider>,
        provider: Arc<dyn LlmProvider>,
        kill_switches: mini_chat_sdk::KillSwitches,
    ) -> StreamService<
        TurnRepo,
        MsgRepo,
        OrmQuotaUsageRepo,
        OrmChatRepo,
        MockThreadSummaryRepo,
        OrmAttachmentRepo,
        OrmVectorStoreRepo,
        OrmMessageAttachmentRepo,
    > {
        use crate::domain::service::finalization_service::FinalizationService;
        use crate::domain::service::quota_settler::QuotaSettler;

        #[allow(de0309_must_have_domain_model)]
        struct MockQuotaSettler;
        #[async_trait::async_trait]
        impl QuotaSettler for MockQuotaSettler {
            async fn settle_in_tx(
                &self,
                _tx: &toolkit_db::secure::DbTx<'_>,
                _scope: &AccessScope,
                _input: crate::domain::model::quota::SettlementInput,
            ) -> Result<
                crate::domain::model::quota::SettlementOutcome,
                crate::domain::error::DomainError,
            > {
                Ok(crate::domain::model::quota::SettlementOutcome {
                    settlement_method: crate::domain::model::quota::SettlementMethod::Released,
                    actual_credits_micro: 0,
                    charged_tokens: 0,
                    overshoot_capped: false,
                })
            }
        }

        let provider_resolver = Arc::new(ProviderResolver::single_provider(provider));
        let turn_repo = Arc::new(TurnRepo);
        let message_repo = Arc::new(MsgRepo::new(toolkit_db::odata::LimitCfg {
            default: 20,
            max: 100,
        }));
        let finalization = Arc::new(FinalizationService::new(
            Arc::clone(&db),
            Arc::clone(&turn_repo),
            Arc::clone(&message_repo),
            Arc::new(MockQuotaSettler) as Arc<dyn QuotaSettler>,
            Arc::new(NoopOutboxEnqueuer) as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            crate::config::background::ThreadSummaryWorkerConfig::default(),
        ));

        let quota_svc = Arc::new(crate::domain::service::QuotaService::new(
            Arc::clone(&db),
            Arc::new(OrmQuotaUsageRepo),
            Arc::new(MockPolicySnapshotProvider::new(
                mini_chat_sdk::PolicySnapshot {
                    user_id: Uuid::nil(),
                    policy_version: 1,
                    model_catalog: vec![test_catalog_entry(TestCatalogEntryParams {
                        model_id: "gpt-5.2".to_owned(),
                        provider_model_id: "gpt-5.2-2025-03-26".to_owned(),
                        display_name: "GPT 5.2".to_owned(),
                        tier: mini_chat_sdk::ModelTier::Standard,
                        enabled: true,
                        is_default: true,
                        input_tokens_credit_multiplier_micro: 1_000_000,
                        output_tokens_credit_multiplier_micro: 1_000_000,
                        multimodal_capabilities: vec![],
                        context_window: 128_000,
                        max_output_tokens: 4096,
                        description: String::new(),
                        provider_display_name: String::new(),
                        multiplier_display: "1x".to_owned(),
                        provider_id: "openai".to_owned(),
                    })],
                    kill_switches,
                },
            )),
            Arc::new(MockUserLimitsProvider::new(permissive_limits())),
            crate::config::EstimationBudgets::default(),
            crate::config::QuotaConfig {
                overshoot_tolerance_factor: 1.10,
                ..crate::config::QuotaConfig::default()
            },
        ));

        StreamService::new(
            db,
            turn_repo,
            message_repo,
            Arc::new(OrmChatRepo::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            })),
            mock_enforcer(),
            provider_resolver,
            crate::config::StreamingConfig::default(),
            finalization,
            quota_svc,
            mock_thread_summary_repo(),
            Arc::new(crate::infra::db::repo::attachment_repo::AttachmentRepository),
            Arc::new(crate::infra::db::repo::vector_store_repo::VectorStoreRepository),
            Arc::new(crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository),
            crate::config::ContextConfig::default(),
            crate::config::RagConfig::default(),
            Arc::new(crate::domain::ports::metrics::NoopMetrics),
            crate::config::KnowledgeSearchConfig::default(),
            None,
            std::collections::HashMap::new(),
        )
    }

    /// 2.7: Mutation with attachments gets `file_search_enabled` = true and real RAG values.
    #[tokio::test]
    async fn mutation_with_attachments_gets_file_search() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;
        insert_running_turn(&db, tenant_id, user_id, chat_id, request_id, turn_id).await;

        // Insert a ready document attachment + vector store
        let _att_id = insert_test_attachment(
            &db,
            InsertTestAttachmentParams {
                uploaded_by_user_id: user_id,
                provider_file_id: Some("file-mut-001".to_owned()),
                ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
            },
        )
        .await;
        insert_test_vector_store(&db, tenant_id, chat_id, Some("vs-mut-001".to_owned())).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["retry answer"]));
        let svc =
            build_stream_service_with_policy(db, provider, mini_chat_sdk::KillSwitches::default());

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let result = svc
            .run_stream_for_mutation(
                ctx,
                chat_id,
                request_id,
                turn_id,
                "retry question".into(),
                test_resolved_model(),
                false,
                None,
                cancel,
                tx,
            )
            .await;

        assert!(result.is_ok(), "mutation stream should succeed: {result:?}");

        let mut got_done = false;
        while let Some(ev) = rx.recv().await {
            if ev.is_terminal() {
                got_done = matches!(ev, StreamEvent::Done(_));
                break;
            }
        }
        assert!(got_done, "mutation stream should complete with Done event");
    }

    /// 2.8: Mutation after all attachments deleted gets `file_search_enabled` = false.
    #[tokio::test]
    async fn mutation_no_attachments_no_file_search() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;
        insert_running_turn(&db, tenant_id, user_id, chat_id, request_id, turn_id).await;
        // No attachments inserted — simulates all deleted

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["no docs"]));
        let svc = build_stream_service(db, provider);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let result = svc
            .run_stream_for_mutation(
                ctx,
                chat_id,
                request_id,
                turn_id,
                "retry without docs".into(),
                test_resolved_model(),
                false,
                None,
                cancel,
                tx,
            )
            .await;

        assert!(
            result.is_ok(),
            "mutation without docs should succeed: {result:?}"
        );

        let mut got_done = false;
        while let Some(ev) = rx.recv().await {
            if ev.is_terminal() {
                got_done = matches!(ev, StreamEvent::Done(_));
                break;
            }
        }
        assert!(got_done, "mutation stream should complete with Done");
    }

    /// 2.9: Kill switch active during mutation forces `RetrievalMode::None`.
    #[tokio::test]
    async fn mutation_kill_switch_disables_file_search() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;
        insert_running_turn(&db, tenant_id, user_id, chat_id, request_id, turn_id).await;

        // Insert attachments + VS (would normally activate file search)
        insert_test_attachment(
            &db,
            InsertTestAttachmentParams {
                uploaded_by_user_id: user_id,
                ..InsertTestAttachmentParams::ready_document(tenant_id, chat_id)
            },
        )
        .await;
        insert_test_vector_store(&db, tenant_id, chat_id, Some("vs-kill-001".to_owned())).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["killed"]));
        // Activate file_search kill switch
        let svc = build_stream_service_with_policy(
            db,
            provider,
            mini_chat_sdk::KillSwitches {
                disable_file_search: true,
                ..Default::default()
            },
        );

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let result = svc
            .run_stream_for_mutation(
                ctx,
                chat_id,
                request_id,
                turn_id,
                "retry with kill switch".into(),
                test_resolved_model(),
                false,
                None,
                cancel,
                tx,
            )
            .await;

        assert!(
            result.is_ok(),
            "mutation with kill switch should succeed: {result:?}"
        );

        let mut got_done = false;
        while let Some(ev) = rx.recv().await {
            if ev.is_terminal() {
                got_done = matches!(ev, StreamEvent::Done(_));
                break;
            }
        }
        assert!(
            got_done,
            "mutation stream should complete despite kill switch"
        );
    }

    // ── TestMetrics — recording implementation for metric assertions ─────

    use std::sync::atomic::{AtomicU64, Ordering};

    /// Lightweight `MiniChatMetricsPort` that records counter increments
    /// and histogram observation counts via atomics. Used to verify that
    /// service code emits the expected metrics.
    #[domain_model]
    struct TestMetrics {
        stream_started: AtomicU64,
        stream_completed: AtomicU64,
        stream_failed: AtomicU64,
        stream_disconnected: AtomicU64,
        stream_incomplete: AtomicU64,
        active_streams_inc: AtomicU64,
        active_streams_dec: AtomicU64,
        ttft_provider_ms: AtomicU64,
        ttft_overhead_ms: AtomicU64,
        stream_total_latency_ms: AtomicU64,
        quota_preflight: AtomicU64,
        quota_reserve: AtomicU64,
        quota_estimated_tokens: AtomicU64,
        audit_emit: AtomicU64,
        finalization_latency_ms: AtomicU64,
        quota_commit: AtomicU64,
        cancel_requested: AtomicU64,
        cancel_effective: AtomicU64,
        time_to_abort_ms: AtomicU64,
    }

    impl TestMetrics {
        fn new() -> Self {
            Self {
                stream_started: AtomicU64::new(0),
                stream_completed: AtomicU64::new(0),
                stream_failed: AtomicU64::new(0),
                stream_disconnected: AtomicU64::new(0),
                stream_incomplete: AtomicU64::new(0),
                active_streams_inc: AtomicU64::new(0),
                active_streams_dec: AtomicU64::new(0),
                ttft_provider_ms: AtomicU64::new(0),
                ttft_overhead_ms: AtomicU64::new(0),
                stream_total_latency_ms: AtomicU64::new(0),
                quota_preflight: AtomicU64::new(0),
                quota_reserve: AtomicU64::new(0),
                quota_estimated_tokens: AtomicU64::new(0),
                audit_emit: AtomicU64::new(0),
                finalization_latency_ms: AtomicU64::new(0),
                quota_commit: AtomicU64::new(0),
                cancel_requested: AtomicU64::new(0),
                cancel_effective: AtomicU64::new(0),
                time_to_abort_ms: AtomicU64::new(0),
            }
        }
    }

    impl crate::domain::ports::MiniChatMetricsPort for TestMetrics {
        fn record_stream_started(&self, _: &str, _: &str) {
            self.stream_started.fetch_add(1, Ordering::Relaxed);
        }
        fn record_stream_completed(&self, _: &str, _: &str) {
            self.stream_completed.fetch_add(1, Ordering::Relaxed);
        }
        fn record_stream_failed(&self, _: &str, _: &str, _: &str) {
            self.stream_failed.fetch_add(1, Ordering::Relaxed);
        }
        fn record_stream_disconnected(&self, _: &str) {
            self.stream_disconnected.fetch_add(1, Ordering::Relaxed);
        }
        fn increment_active_streams(&self) {
            self.active_streams_inc.fetch_add(1, Ordering::Relaxed);
        }
        fn decrement_active_streams(&self) {
            self.active_streams_dec.fetch_add(1, Ordering::Relaxed);
        }
        fn record_ttft_provider_ms(&self, _: &str, _: &str, _: f64) {
            self.ttft_provider_ms.fetch_add(1, Ordering::Relaxed);
        }
        fn record_ttft_overhead_ms(&self, _: &str, _: &str, _: f64) {
            self.ttft_overhead_ms.fetch_add(1, Ordering::Relaxed);
        }
        fn record_stream_total_latency_ms(&self, _: &str, _: &str, _: f64) {
            self.stream_total_latency_ms.fetch_add(1, Ordering::Relaxed);
        }
        fn record_turn_mutation(&self, _: &str, _: &str) {}
        fn record_turn_mutation_latency_ms(&self, _: &str, _: f64) {}
        fn record_audit_emit(&self, _: &str) {
            self.audit_emit.fetch_add(1, Ordering::Relaxed);
        }
        fn record_finalization_latency_ms(&self, _: f64) {
            self.finalization_latency_ms.fetch_add(1, Ordering::Relaxed);
        }
        fn record_quota_preflight(&self, _: &str, _: &str, _: &str) {
            self.quota_preflight.fetch_add(1, Ordering::Relaxed);
        }
        fn record_quota_reserve(&self, _: &str) {
            self.quota_reserve.fetch_add(1, Ordering::Relaxed);
        }
        fn record_quota_commit(&self, _: &str) {
            self.quota_commit.fetch_add(1, Ordering::Relaxed);
        }
        fn record_quota_overshoot(&self, _: &str) {}
        fn record_quota_estimated_tokens(&self, _: f64) {
            self.quota_estimated_tokens.fetch_add(1, Ordering::Relaxed);
        }
        fn record_quota_actual_tokens(&self, _: f64) {}
        fn record_stream_incomplete(&self, _: &str, _: &str, _: &str) {
            self.stream_incomplete.fetch_add(1, Ordering::Relaxed);
        }
        fn record_cancel_requested(&self, _: &str) {
            self.cancel_requested.fetch_add(1, Ordering::Relaxed);
        }
        fn record_cancel_effective(&self, _: &str) {
            self.cancel_effective.fetch_add(1, Ordering::Relaxed);
        }
        fn record_time_to_abort_ms(&self, _: &str, _: f64) {
            self.time_to_abort_ms.fetch_add(1, Ordering::Relaxed);
        }
        fn record_streams_aborted(&self, _: &str) {}
        fn record_attachment_upload(&self, _: &str, _: &str) {}
        fn record_attachment_upload_bytes(&self, _: &str, _: f64) {}
        fn increment_attachments_pending(&self) {}
        fn decrement_attachments_pending(&self) {}
        fn record_image_inputs_per_turn(&self, _: u32) {}
        fn record_orphan_detected(&self, _: &str) {}
        fn record_orphan_finalized(&self, _: &str) {}
        fn record_orphan_scan_duration_seconds(&self, _: f64) {}
        fn record_code_interpreter_calls(&self, _: &str, _: u32) {}
        fn record_cleanup_completed(&self, _: &str) {}
        fn record_cleanup_failed(&self, _: &str) {}
        fn record_cleanup_retry(&self, _: &str, _: &str) {}
        fn record_cleanup_backlog(&self, _: &str, _: &str, _: i64) {}
        fn record_cleanup_vs_with_failed_attachments(&self) {}
        fn record_secondary_cleanup_skipped(&self, _: &str) {}
        fn record_thread_summary_trigger(&self, _: &str) {}
        fn record_thread_summary_execution(&self, _: &str) {}
        fn record_thread_summary_cas_conflict(&self) {}
        fn record_summary_fallback(&self) {}
        fn record_knowledge_search(&self, _: &str) {}
        fn record_knowledge_search_latency_ms(&self, _: f64) {}
        fn record_knowledge_search_chunks(&self, _: f64) {}
    }

    // ── Metric emission tests ────────────────────────────────────────────

    /// Provider that emits one delta then hangs indefinitely, allowing
    /// cancellation tests. The stream yields a single token, then `pending`
    /// forever until the cancellation token fires.
    #[domain_model]
    struct HangingProvider;

    #[async_trait::async_trait]
    impl LlmProvider for HangingProvider {
        async fn stream(
            &self,
            _ctx: SecurityContext,
            _request: LlmRequest<Streaming>,
            _upstream_alias: &str,
            cancel: CancellationToken,
        ) -> Result<ProviderStream, LlmProviderError> {
            let one_delta = stream::once(async {
                Ok(TranslatedEvent::Sse(ClientSseEvent::Delta {
                    r#type: "text",
                    content: "Hi".to_owned(),
                }))
            });
            // Chain with a stream that never resolves
            let hanging = one_delta.chain(stream::pending());
            Ok(ProviderStream::new(hanging, cancel))
        }

        async fn complete(
            &self,
            _ctx: SecurityContext,
            _request: LlmRequest<NonStreaming>,
            _upstream_alias: &str,
        ) -> Result<ResponseResult, LlmProviderError> {
            unimplemented!()
        }
    }

    /// Happy path: completed stream emits expected metric sequence.
    #[tokio::test]
    async fn metrics_emitted_on_completed_stream() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let metrics = Arc::new(TestMetrics::new());
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
        let svc = build_stream_service_with_metrics(db, provider, Arc::clone(&metrics) as _);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let handle = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect("should succeed");

        // Drain events
        while let Some(ev) = rx.recv().await {
            if ev.is_terminal() {
                break;
            }
        }
        let outcome = handle.await.expect("task should complete");
        assert_eq!(outcome.terminal, StreamTerminal::Completed);

        // Verify metric emissions
        assert_eq!(metrics.stream_started.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.stream_completed.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.stream_failed.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.active_streams_inc.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.active_streams_dec.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.ttft_provider_ms.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.ttft_overhead_ms.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.stream_total_latency_ms.load(Ordering::Relaxed), 1);
        // Preflight: 1 call (allow)
        assert_eq!(metrics.quota_preflight.load(Ordering::Relaxed), 1);
        // Reserve: 2 calls (daily + monthly)
        assert_eq!(metrics.quota_reserve.load(Ordering::Relaxed), 2);
        assert_eq!(metrics.quota_estimated_tokens.load(Ordering::Relaxed), 1);
        // Finalization: audit emit + latency
        assert_eq!(metrics.audit_emit.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.finalization_latency_ms.load(Ordering::Relaxed), 1);
    }

    /// Failure path: provider error emits `stream_failed`, not `stream_completed`.
    #[tokio::test]
    async fn metrics_emitted_on_provider_error() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let metrics = Arc::new(TestMetrics::new());
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::failing());
        let svc = build_stream_service_with_metrics(db, provider, Arc::clone(&metrics) as _);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let handle = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect("should succeed (stream spawned)");

        // Drain events — expect an error event
        while let Some(ev) = rx.recv().await {
            if ev.is_terminal() {
                break;
            }
        }
        let outcome = handle.await.expect("task should complete");
        assert_eq!(outcome.terminal, StreamTerminal::Failed);

        // Verify metric emissions
        assert_eq!(metrics.stream_started.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.stream_completed.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.stream_failed.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.active_streams_inc.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.active_streams_dec.load(Ordering::Relaxed), 1);
        // No first token → no TTFT metrics
        assert_eq!(metrics.ttft_provider_ms.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.ttft_overhead_ms.load(Ordering::Relaxed), 0);
        // Total latency is still recorded
        assert_eq!(metrics.stream_total_latency_ms.load(Ordering::Relaxed), 1);
        // Preflight still ran
        assert_eq!(metrics.quota_preflight.load(Ordering::Relaxed), 1);
    }

    /// Incomplete path: `stream_incomplete` + `stream_completed` both emitted.
    #[tokio::test]
    async fn metrics_emitted_on_incomplete_stream() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let metrics = Arc::new(TestMetrics::new());
        let provider: Arc<dyn LlmProvider> =
            Arc::new(MockProvider::incomplete(&["Partial output"]));
        let svc = build_stream_service_with_metrics(db, provider, Arc::clone(&metrics) as _);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let handle = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel,
                tx,
            )
            .await
            .expect("should succeed");

        while let Some(ev) = rx.recv().await {
            if ev.is_terminal() {
                break;
            }
        }
        let outcome = handle.await.expect("task should complete");
        assert_eq!(outcome.terminal, StreamTerminal::Incomplete);

        // stream_incomplete is a sub-counter of stream_completed (both emitted)
        assert_eq!(metrics.stream_incomplete.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.stream_completed.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.stream_failed.load(Ordering::Relaxed), 0);
        // Active streams gauge balanced
        assert_eq!(metrics.active_streams_inc.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.active_streams_dec.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.stream_total_latency_ms.load(Ordering::Relaxed), 1);
    }

    /// Cancel path: cancellation metrics emitted, `active_streams` gauge balanced.
    #[tokio::test]
    async fn metrics_emitted_on_cancelled_stream() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;

        let metrics = Arc::new(TestMetrics::new());
        let provider: Arc<dyn LlmProvider> = Arc::new(HangingProvider);
        let svc = build_stream_service_with_metrics(db, provider, Arc::clone(&metrics) as _);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, mut rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let handle = svc
            .run_stream(
                ctx,
                chat_id,
                Uuid::new_v4(),
                "hello".into(),
                test_resolved_model(),
                false,
                Vec::new(),
                cancel.clone(),
                tx,
            )
            .await
            .expect("should succeed");

        // Wait for stream_started and first delta to arrive, then cancel
        let started = rx.recv().await.expect("should get stream_started");
        assert!(matches!(started, StreamEvent::StreamStarted(_)));
        let ev = rx.recv().await.expect("should receive delta");
        assert!(
            matches!(ev, StreamEvent::Delta(_)),
            "second event should be a delta"
        );
        cancel.cancel();

        // Drain remaining events
        while rx.recv().await.is_some() {}
        let outcome = handle.await.expect("task should complete");
        assert_eq!(outcome.terminal, StreamTerminal::Cancelled);

        // Cancellation metrics
        assert_eq!(metrics.cancel_requested.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.stream_disconnected.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.cancel_effective.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.time_to_abort_ms.load(Ordering::Relaxed), 1);
        // NOT completed or failed
        assert_eq!(metrics.stream_completed.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.stream_failed.load(Ordering::Relaxed), 0);
        // Active streams gauge balanced
        assert_eq!(metrics.active_streams_inc.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.active_streams_dec.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.stream_total_latency_ms.load(Ordering::Relaxed), 1);
    }

    /// `run_stream_for_mutation` returns `InputTooLong` for oversized content
    /// and marks the already-committed turn as `Failed` so it does not stay
    /// stuck in `Running` state.
    ///
    /// Setup: model catalog has `context_window = max_input_tokens = 500`.
    /// Content: 1500 ASCII bytes → ~523 estimated tokens > 500 → `InputTooLong`.
    #[tokio::test]
    async fn run_stream_for_mutation_input_too_long_marks_turn_failed() {
        let db = mock_db_provider(inmem_db().await);
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let chat_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        insert_test_chat(&db, tenant_id, user_id, chat_id).await;
        insert_running_turn(&db, tenant_id, user_id, chat_id, request_id, turn_id).await;

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&[]));
        let svc = build_stream_service_with_context_window(db.clone(), provider, 500);

        let ctx = test_security_ctx_with_id(tenant_id, user_id);
        let (tx, _rx) = mpsc::channel(32);
        let cancel = CancellationToken::new();

        let err = svc
            .run_stream_for_mutation(
                ctx,
                chat_id,
                request_id,
                turn_id,
                "a".repeat(1500),
                test_resolved_model(),
                false,
                None,
                cancel,
                tx,
            )
            .await
            .expect_err("should be InputTooLong");

        match err {
            StreamError::InputTooLong {
                estimated_tokens,
                max_input_tokens,
            } => {
                assert_eq!(max_input_tokens, 500);
                assert!(
                    estimated_tokens > u64::from(max_input_tokens),
                    "estimated {estimated_tokens} should exceed limit {max_input_tokens}"
                );
            }
            other => panic!("expected InputTooLong, got: {other:?}"),
        }

        // The pre-committed turn must be marked Failed, not left in Running.
        let conn = db.conn().unwrap();
        let scope = AccessScope::allow_all();
        let turn = TurnRepo
            .find_by_chat_and_request_id(&conn, &scope, chat_id, request_id)
            .await
            .expect("DB query should succeed")
            .expect("turn must exist");
        assert_eq!(
            turn.state,
            TurnState::Failed,
            "turn should be marked Failed after InputTooLong"
        );
        assert_eq!(
            turn.error_code.as_deref(),
            Some("input_too_long"),
            "error_code should be set to input_too_long"
        );
    }
}

#[cfg(test)]
mod function_tool_resolution_tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::resolve_function_tools_from;
    use crate::domain::llm::{LlmFunctionDef, LlmTool};
    use crate::domain::ports::{FunctionTool, FunctionToolError};

    struct MockTool {
        name: &'static str,
        guard: Option<&'static str>,
    }

    #[async_trait]
    impl FunctionTool for MockTool {
        fn name(&self) -> &str {
            self.name
        }
        fn definition(&self) -> LlmFunctionDef {
            LlmFunctionDef {
                name: self.name.to_owned(),
                description: "mock".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
            }
        }
        fn system_prompt_guard(&self) -> Option<String> {
            self.guard.map(ToOwned::to_owned)
        }
        fn max_calls(&self) -> u32 {
            3
        }
        async fn execute(
            &self,
            _ctx: toolkit_security::SecurityContext,
            _input: serde_json::Value,
        ) -> Result<String, FunctionToolError> {
            Ok("ok".to_owned())
        }
    }

    fn registry() -> std::collections::HashMap<String, Arc<dyn FunctionTool>> {
        let mut m: std::collections::HashMap<String, Arc<dyn FunctionTool>> =
            std::collections::HashMap::new();
        m.insert(
            "exa_search".to_owned(),
            Arc::new(MockTool {
                name: "exa_search",
                guard: Some("exa guard"),
            }),
        );
        m
    }

    #[test]
    fn resolves_enabled_tool_with_def_and_guard() {
        let (map, tools, guards) =
            resolve_function_tools_from(&registry(), &["exa_search".to_owned()]);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("exa_search"));
        assert!(
            matches!(&tools[..], [LlmTool::Function { name, .. }] if name == "exa_search"),
            "should produce one exa_search Function tool"
        );
        assert_eq!(guards, vec!["exa guard".to_owned()]);
    }

    #[test]
    fn ignores_unknown_tool_names() {
        let (map, tools, guards) =
            resolve_function_tools_from(&registry(), &["nonexistent".to_owned()]);
        assert!(map.is_empty());
        assert!(tools.is_empty());
        assert!(guards.is_empty());
    }

    #[test]
    fn empty_enabled_resolves_to_empty() {
        let (map, tools, guards) = resolve_function_tools_from(&registry(), &[]);
        assert!(map.is_empty() && tools.is_empty() && guards.is_empty());
    }
}
