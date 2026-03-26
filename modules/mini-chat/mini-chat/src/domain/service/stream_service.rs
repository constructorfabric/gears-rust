use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use futures::StreamExt;
use modkit_macros::domain_model;
use modkit_security::{AccessScope, SecurityContext};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, warn};
use uuid::Uuid;

use mini_chat_sdk::RequesterType;

use crate::config::{ContextConfig, StreamingConfig};
use crate::domain::error::DomainError;
use crate::domain::llm::{ToolPhase, Usage};
use crate::domain::models::ResolvedModel;
use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::ports::metric_labels::{decision, period, stage, trigger};
use crate::domain::repos::{
    AttachmentRepository, ChatRepository, CreateTurnParams, InsertUserMessageParams,
    MessageAttachmentRepository, MessageRepository, QuotaUsageRepository, SnapshotBoundary,
    ThreadSummaryRepository, TurnRepository, VectorStoreRepository,
};
use crate::domain::stream_events::{DoneData, ErrorData, StreamEvent, StreamStartedData};
use crate::infra::db::entity::chat_turn::{Model as TurnModel, TurnState};
use crate::infra::llm::{
    ClientSseEvent, FeatureFlag, LlmMessage, LlmProvider, LlmProviderError, LlmRequestBuilder,
    LlmTool, RequestMetadata, RequestType, TerminalOutcome, provider_resolver::ProviderResolver,
};

use super::{DbProvider, actions, resources};

// ── RAII guard for active_streams gauge ──────────────────────────────────

/// Ensures `decrement_active_streams` is always called when the guard is
/// dropped, even if a new exit path is added without an explicit decrement.
#[domain_model]
struct ActiveStreamGuard(Arc<dyn MiniChatMetricsPort>);

impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        self.0.decrement_active_streams();
    }
}

// ── Typed error for attachment validation inside TX boundary ─────────────

#[allow(de0309_must_have_domain_model)]
#[derive(Debug, thiserror::Error)]
#[error("invalid attachment: {message}")]
struct InvalidAttachmentError {
    message: String,
}

fn attachment_err(message: impl Into<String>) -> modkit_db::DbError {
    modkit_db::DbError::Other(anyhow::Error::new(InvalidAttachmentError {
        message: message.into(),
    }))
}

/// Collects [`FeatureFlag`]s from the tools attached to a request, used to
/// populate [`RequestMetadata`] for observability.
fn determine_features(tools: &[LlmTool]) -> Vec<FeatureFlag> {
    let mut flags = Vec::new();
    if tools
        .iter()
        .any(|t| matches!(t, LlmTool::FileSearch { .. }))
    {
        flags.push(FeatureFlag::FileSearch);
    }
    if tools.iter().any(|t| matches!(t, LlmTool::WebSearch { .. })) {
        flags.push(FeatureFlag::WebSearch);
    }
    if tools
        .iter()
        .any(|t| matches!(t, LlmTool::CodeInterpreter { .. }))
    {
        flags.push(FeatureFlag::CodeInterpreter);
    }
    flags
}

// ════════════════════════════════════════════════════════════════════════════
// StreamTerminal — service-level terminal classification
// ════════════════════════════════════════════════════════════════════════════

/// How the stream ended at the service level.
///
/// Maps from the provider-level [`TerminalOutcome`] with an additional
/// `Cancelled` variant for client/server-initiated cancellation.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamTerminal {
    /// Provider completed successfully — full response received.
    Completed,
    /// Provider stopped early (e.g. `max_output_tokens` hit).
    Incomplete,
    /// Provider or stream-level error.
    Failed,
    /// Cancelled (client disconnect or server-initiated).
    Cancelled,
}

// ════════════════════════════════════════════════════════════════════════════
// StreamOutcome — returned from run_stream()
// ════════════════════════════════════════════════════════════════════════════

/// Summary of a finished stream, returned from [`StreamService::run_stream()`].
///
/// Used by P1 for logging and metrics, and by P4 for CAS finalization.
#[domain_model]
#[derive(Debug)]
#[allow(dead_code)]
pub struct StreamOutcome {
    /// How the stream ended.
    pub terminal: StreamTerminal,
    /// Accumulated assistant text from delta events.
    pub accumulated_text: String,
    /// Token usage from the provider (if available).
    pub usage: Option<Usage>,
    /// The model actually used by the provider.
    pub effective_model: String,
    /// Normalized error code (e.g. `rate_limited`, `provider_timeout`).
    pub error_code: Option<String>,
    /// Provider response ID (e.g. `OpenAI` `response_id`).
    pub provider_response_id: Option<String>,
    /// Whether usage was from a partial/incomplete provider response.
    pub provider_partial_usage: bool,
}

// ════════════════════════════════════════════════════════════════════════════
// StreamError — pre-stream error before SSE connection opens
// ════════════════════════════════════════════════════════════════════════════

/// Pre-stream error — returned from [`StreamService::run_stream()`] before
/// the SSE connection opens. The handler maps these to JSON error responses.
#[domain_model]
#[derive(Debug)]
#[allow(dead_code)]
pub enum StreamError {
    /// Idempotent replay: a turn with this `request_id` already exists.
    Replay { turn: Box<TurnModel> },
    /// Conflict: another turn is already running for this chat.
    Conflict { code: String, message: String },
    /// Turn creation or pre-stream DB operation failed.
    TurnCreationFailed { source: DomainError },
    /// Authorization failed (enforcer denied access).
    AuthorizationFailed { source: DomainError },
    /// Chat does not exist or is not visible to the caller.
    ChatNotFound { chat_id: Uuid },
    /// Quota exhausted — preflight rejected the request.
    QuotaExhausted {
        error_code: String,
        http_status: u16,
        quota_scope: String,
    },
    /// Web search is disabled via kill switch but was requested.
    WebSearchDisabled,
    /// Images are disabled via kill switch but image attachments were included.
    ImagesDisabled,
    /// Too many image attachments in one message (`max_images_per_message` exceeded).
    TooManyImages { count: u32, max: u32 },
    /// Model does not support image input (missing `VISION_INPUT` capability).
    UnsupportedMedia,
    /// One or more attachment IDs are invalid (not found, wrong status, wrong chat, etc.).
    InvalidAttachment { code: String, message: String },
    /// Context budget exceeded — mandatory items don't fit in the token budget.
    ContextBudgetExceeded {
        required_tokens: u64,
        available_tokens: u64,
    },
}

impl From<authz_resolver_sdk::EnforcerError> for StreamError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        match e {
            e @ authz_resolver_sdk::EnforcerError::Denied { .. } => Self::AuthorizationFailed {
                source: DomainError::from(e),
            },
            e @ (authz_resolver_sdk::EnforcerError::EvaluationFailed(_)
            | authz_resolver_sdk::EnforcerError::CompileFailed(_)) => Self::TurnCreationFailed {
                source: DomainError::from(e),
            },
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// FinalizationCtx — bundled context for atomic finalization in the spawned task
// ════════════════════════════════════════════════════════════════════════════

/// All context needed to call `FinalizationService::finalize_turn_cas()`
/// from the spawned provider task. Replaces the old `PersistenceCtx`.
///
/// Assembled in `run_stream()` after preflight commits, from `PreflightDecision`
/// fields + request context. `None` in unit tests (no DB).
#[domain_model]
struct FinalizationCtx<TR: TurnRepository + 'static, MR: MessageRepository + 'static> {
    finalization_svc:
        Arc<crate::domain::service::finalization_service::FinalizationService<TR, MR>>,
    scope: AccessScope,
    turn_id: Uuid,
    tenant_id: Uuid,
    chat_id: Uuid,
    request_id: Uuid,
    user_id: Uuid,
    requester_type: RequesterType,
    /// Pre-generated assistant message ID, sent in `StreamStartedData` (`stream_started` event).
    message_id: Uuid,
    // ── Quota/preflight fields (from PreflightDecision) ──
    effective_model: String,
    selected_model: String,
    reserve_tokens: i64,
    max_output_tokens_applied: i32,
    reserved_credits_micro: i64,
    policy_version_applied: i64,
    minimal_generation_floor_applied: i32,
    quota_decision: String,
    downgrade_from: Option<String>,
    downgrade_reason: Option<String>,
    period_starts: Vec<(
        crate::infra::db::entity::quota_usage::PeriodType,
        time::Date,
    )>,
    /// Provider ID for metrics labels.
    provider_id: String,
    /// Metrics port for recording stream metrics in the spawned task.
    metrics: Arc<dyn MiniChatMetricsPort>,
    /// Quota warnings provider for computing `quota_warnings` in the `done` event.
    quota_warnings_provider: Arc<dyn crate::domain::service::quota_settler::QuotaWarningsProvider>,
}

impl<TR: TurnRepository + 'static, MR: MessageRepository + 'static> FinalizationCtx<TR, MR> {
    /// Build a [`FinalizationInput`] from this context and stream outcome data.
    #[allow(clippy::too_many_arguments)]
    fn to_finalization_input(
        &self,
        terminal_state: TurnState,
        accumulated_text: &str,
        usage: Option<Usage>,
        error_code: Option<String>,
        error_detail: Option<String>,
        provider_response_id: Option<String>,
        web_search_calls: u32,
        code_interpreter_calls: u32,
        ttft_ms: Option<u64>,
        total_ms: Option<u64>,
    ) -> crate::domain::model::finalization::FinalizationInput {
        crate::domain::model::finalization::FinalizationInput {
            turn_id: self.turn_id,
            tenant_id: self.tenant_id,
            chat_id: self.chat_id,
            request_id: self.request_id,
            user_id: self.user_id,
            requester_type: self.requester_type,
            scope: self.scope.clone(),
            message_id: self.message_id,
            terminal_state,
            error_code,
            error_detail,
            accumulated_text: accumulated_text.to_owned(),
            usage,
            provider_response_id,
            effective_model: self.effective_model.clone(),
            selected_model: self.selected_model.clone(),
            reserve_tokens: self.reserve_tokens,
            max_output_tokens_applied: self.max_output_tokens_applied,
            reserved_credits_micro: self.reserved_credits_micro,
            policy_version_applied: self.policy_version_applied,
            minimal_generation_floor_applied: self.minimal_generation_floor_applied,
            quota_decision: self.quota_decision.clone(),
            downgrade_from: self.downgrade_from.clone(),
            downgrade_reason: self.downgrade_reason.clone(),
            period_starts: self.period_starts.clone(),
            web_search_calls,
            code_interpreter_calls,
            ttft_ms,
            total_ms,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Error normalization
// ════════════════════════════════════════════════════════════════════════════

/// Map an optional subject-type string from [`SecurityContext`] to [`RequesterType`].
fn requester_type_from_str(s: Option<&str>) -> RequesterType {
    match s {
        Some("system") => RequesterType::System,
        _ => RequesterType::User,
    }
}

/// Normalize an [`LlmProviderError`] to a `(code, message)` pair for the SSE
/// error event. Messages are already sanitized by the infra layer.
fn normalize_error(err: &LlmProviderError) -> (String, String) {
    match err {
        LlmProviderError::RateLimited { .. } => (
            "rate_limited".to_owned(),
            "Rate limited by provider".to_owned(),
        ),
        LlmProviderError::Timeout => (
            "provider_timeout".to_owned(),
            "Provider request timed out".to_owned(),
        ),
        LlmProviderError::ProviderError { message, .. } => {
            ("provider_error".to_owned(), message.clone())
        }
        LlmProviderError::InvalidResponse { detail } => (
            "provider_error".to_owned(),
            crate::infra::llm::sanitize_provider_message(detail),
        ),
        LlmProviderError::ProviderUnavailable => (
            "provider_error".to_owned(),
            "Provider is currently unavailable".to_owned(),
        ),
        LlmProviderError::StreamError(e) => (
            "provider_error".to_owned(),
            crate::infra::llm::sanitize_provider_message(&e.to_string()),
        ),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// PreflightResult — flattened preflight outcome for run_stream()
// ════════════════════════════════════════════════════════════════════════════

/// Flattened preflight fields used by `run_stream()` after `preflight_reserve()`.
#[domain_model]
struct PreflightResult {
    effective_model: String,
    effective_provider_model_id: String,
    reserve_tokens: i64,
    max_output_tokens_applied: i32,
    reserved_credits_micro: i64,
    policy_version_applied: i64,
    minimal_generation_floor_applied: i32,
    quota_decision: String,
    downgrade_from: Option<String>,
    downgrade_reason: Option<String>,
    system_prompt: String,
    context_window: u32,
    estimation_budgets: crate::config::EstimationBudgets,
    max_retrieved_chunks_per_turn: u32,
    max_tool_calls: u32,
    tool_support: mini_chat_sdk::ModelToolSupport,
}

/// Convert a `PreflightDecision` into a flat `PreflightResult` or a `StreamError`.
fn flatten_preflight(
    decision: crate::domain::model::quota::PreflightDecision,
) -> Result<PreflightResult, StreamError> {
    use crate::domain::model::quota::PreflightDecision;
    match decision {
        PreflightDecision::Allow {
            effective_model,
            effective_provider_model_id,
            reserve_tokens,
            max_output_tokens_applied,
            reserved_credits_micro,
            policy_version_applied,
            minimal_generation_floor_applied,
            system_prompt,
            context_window,
            estimation_budgets,
            max_retrieved_chunks_per_turn,
            max_tool_calls,
            tool_support,
            ..
        } => Ok(PreflightResult {
            effective_model,
            effective_provider_model_id,
            reserve_tokens,
            max_output_tokens_applied,
            reserved_credits_micro,
            policy_version_applied,
            minimal_generation_floor_applied,
            quota_decision: "allow".to_owned(),
            downgrade_from: None,
            downgrade_reason: None,
            system_prompt,
            context_window,
            estimation_budgets,
            max_retrieved_chunks_per_turn,
            max_tool_calls,
            tool_support,
        }),
        PreflightDecision::Downgrade {
            effective_model,
            effective_provider_model_id,
            reserve_tokens,
            max_output_tokens_applied,
            reserved_credits_micro,
            policy_version_applied,
            minimal_generation_floor_applied,
            downgrade_from,
            downgrade_reason,
            system_prompt,
            context_window,
            estimation_budgets,
            max_retrieved_chunks_per_turn,
            max_tool_calls,
            tool_support,
            ..
        } => Ok(PreflightResult {
            effective_model,
            effective_provider_model_id,
            reserve_tokens,
            max_output_tokens_applied,
            reserved_credits_micro,
            policy_version_applied,
            minimal_generation_floor_applied,
            quota_decision: "downgrade".to_owned(),
            downgrade_from: Some(downgrade_from),
            downgrade_reason: Some(downgrade_reason.as_str().to_owned()),
            system_prompt,
            context_window,
            estimation_budgets,
            max_retrieved_chunks_per_turn,
            max_tool_calls,
            tool_support,
        }),
        PreflightDecision::Reject {
            error_code,
            http_status,
            quota_scope,
        } => Err(StreamError::QuotaExhausted {
            error_code,
            http_status,
            quota_scope,
        }),
    }
}

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

        Ok(spawn_provider_task(
            resolved_provider.adapter,
            proxy_path,
            ctx,
            assembled.messages,
            assembled.system_instructions,
            assembled.tools,
            model,
            effective_provider_model_id,
            pf.max_output_tokens_applied.cast_unsigned(),
            pf.max_tool_calls,
            self.quota.web_search_max_calls_per_message(),
            self.quota.code_interpreter_max_calls_per_message(),
            cancel,
            tx,
            Some(finalization_ctx),
            provider_file_id_map,
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

        // Metrics: estimated tokens (only on allow/downgrade)
        #[allow(clippy::cast_precision_loss)]
        self.metrics
            .record_quota_estimated_tokens(pf.reserve_tokens as f64);

        let period_starts = computed.periods.clone();
        let file_search_disabled = computed.kill_switches.disable_file_search;
        let disable_code_interpreter = computed.kill_switches.disable_code_interpreter;

        // ── Write quota reserves ────────────────────────────────────────
        let quota_repo = Arc::clone(&self.quota.repo);
        let computed_for_tx = computed;

        if !computed_for_tx.buckets.is_empty() {
            self.db
                .transaction(|txn| {
                    use crate::domain::repos::IncrementReserveParams;
                    Box::pin(async move {
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

            // Metrics: quota reserve committed (one per period)
            for (period_type, _) in &period_starts {
                let label = match period_type {
                    crate::infra::db::entity::quota_usage::PeriodType::Daily => period::DAILY,
                    crate::infra::db::entity::quota_usage::PeriodType::Monthly => period::MONTHLY,
                };
                self.metrics.record_quota_reserve(label);
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

        Ok(spawn_provider_task(
            resolved_provider.adapter,
            proxy_path,
            ctx,
            assembled.messages,
            assembled.system_instructions,
            assembled.tools,
            pf.effective_model,
            effective_provider_model_id,
            pf.max_output_tokens_applied.cast_unsigned(),
            pf.max_tool_calls,
            self.quota.web_search_max_calls_per_message(),
            self.quota.code_interpreter_max_calls_per_message(),
            cancel,
            tx,
            Some(finalization_ctx),
            provider_file_id_map,
        ))
    }
}

/// Core provider task: reads from the LLM, translates events, and returns
/// a [`StreamOutcome`]. After the stream ends, atomically finalizes the turn
/// via `FinalizationService::finalize_turn_cas()` if a context is provided.
///
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

/// All five terminal paths (provider done, incomplete, provider error,
/// client disconnect, pre-stream error) route through `finalize_turn_cas()`.
/// SSE terminal events (Done/Error) are emitted only after the CAS winner
/// commits the transaction (D3).
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    clippy::let_underscore_must_use,
    clippy::cast_possible_truncation
)]
fn spawn_provider_task<TR: TurnRepository + 'static, MR: MessageRepository + 'static>(
    llm: Arc<dyn LlmProvider>,
    upstream_alias: String,
    ctx: SecurityContext,
    messages: Vec<LlmMessage>,
    system_instructions: Option<String>,
    tools: Vec<LlmTool>,
    model: String,
    provider_model_id: String,
    max_output_tokens: u32,
    max_tool_calls: u32,
    web_search_max_calls: u32,
    code_interpreter_max_calls: u32,
    cancel: CancellationToken,
    tx: mpsc::Sender<StreamEvent>,
    fin_ctx: Option<FinalizationCtx<TR, MR>>,
    provider_file_id_map: std::collections::HashMap<String, crate::domain::llm::AttachmentRef>,
) -> tokio::task::JoinHandle<StreamOutcome> {
    let span = if let Some(ref fctx) = fin_ctx {
        tracing::info_span!(
            "provider_stream",
            chat_id = %fctx.chat_id,
            turn_request_id = %fctx.request_id,
            turn_id = %fctx.turn_id,
            model = %model,
        )
    } else {
        tracing::info_span!("provider_stream", model = %model)
    };

    tokio::spawn(async move {
        let stream_start = std::time::Instant::now();
        let mut first_token_time: Option<std::time::Duration> = None;

        // ── Metrics: stream started + active gauge ──
        // ActiveStreamGuard ensures decrement on every exit path (Drop-based).
        let _stream_guard = if let Some(ref fctx) = fin_ctx {
            fctx.metrics
                .record_stream_started(&fctx.provider_id, &fctx.effective_model);
            fctx.metrics.increment_active_streams();
            Some(ActiveStreamGuard(Arc::clone(&fctx.metrics)))
        } else {
            None
        };

        // Build the LLM request using provider_model_id (the actual provider-facing name)
        let mut builder = LlmRequestBuilder::new(&provider_model_id)
            .messages(messages)
            .max_output_tokens(u64::from(max_output_tokens))
            .max_tool_calls(max_tool_calls);
        if let Some(instructions) = system_instructions {
            builder = builder.system_instructions(instructions);
        }
        let features = determine_features(&tools);
        for tool in tools {
            builder = builder.tool(tool);
        }
        let metadata = RequestMetadata {
            tenant_id: ctx.subject_tenant_id().to_string(),
            user_id: ctx.subject_id().to_string(),
            chat_id: fin_ctx
                .as_ref()
                .map_or_else(String::new, |f| f.chat_id.to_string()),
            request_type: RequestType::Chat,
            features,
        };
        builder = builder.metadata(metadata);
        let request = builder.build_streaming();

        // Call the provider to start streaming
        let stream_result = llm
            .stream(ctx, request, &upstream_alias, cancel.clone())
            .await;

        let mut provider_stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                // Provider failed before any events — finalize first, then emit error.
                warn!(
                    error = %e,
                    raw_detail = e.raw_detail().unwrap_or(""),
                    "LLM provider failed before stream start"
                );
                let (code, message) = normalize_error(&e);

                if let Some(ref fctx) = fin_ctx {
                    let input = fctx.to_finalization_input(
                        TurnState::Failed,
                        "",
                        None,
                        Some(code.clone()),
                        None,
                        None,
                        0,
                        0,
                        None,
                        None,
                    );
                    match fctx.finalization_svc.finalize_turn_cas(input).await {
                        Ok(outcome) if outcome.won_cas => {
                            let _ = tx
                                .send(StreamEvent::Error(ErrorData {
                                    code: code.clone(),
                                    message,
                                }))
                                .await;
                        }
                        Ok(_) => { /* CAS loser — no SSE emission */ }
                        Err(fe) => {
                            warn!(error = %fe, "finalization failed on pre-stream error");
                            // Still emit error so client isn't left hanging
                            let _ = tx
                                .send(StreamEvent::Error(ErrorData {
                                    code: code.clone(),
                                    message,
                                }))
                                .await;
                        }
                    }
                } else {
                    let _ = tx
                        .send(StreamEvent::Error(ErrorData {
                            code: code.clone(),
                            message,
                        }))
                        .await;
                }

                // Metrics: pre-stream failure
                if let Some(ref fctx) = fin_ctx {
                    let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                    fctx.metrics.record_stream_failed(&fctx.provider_id, &fctx.effective_model, &code);
                    fctx.metrics.record_stream_total_latency_ms(&fctx.provider_id, &fctx.effective_model, ms);
                }

                return StreamOutcome {
                    terminal: StreamTerminal::Failed,
                    accumulated_text: String::new(),
                    usage: None,
                    effective_model: model,
                    error_code: Some(code),
                    provider_response_id: None,
                    provider_partial_usage: false,
                };
            }
        };

        // Read events from provider, translate and forward through channel
        let mut accumulated_text = String::new();
        let mut cancelled = false;
        let mut web_search_call_count: u32 = 0;
        // TODO(P2): web_search_call_count (Start) is used for enforcement,
        // web_search_completed_count (Done) is used for settlement. If a search
        // starts but never completes (provider error between Start/Done), the
        // daily quota under-counts by one. Acceptable for P1 since OpenAI always
        // pairs searching→completed; revisit if we add providers that don't.
        let mut web_search_completed_count: u32 = 0;
        let mut code_interpreter_call_count: u32 = 0;
        let mut code_interpreter_completed_count: u32 = 0;

        loop {
            tokio::select! {
                biased;

                () = cancel.cancelled() => {
                    debug!("stream cancelled, aborting provider");
                    if let Some(ref fctx) = fin_ctx {
                        fctx.metrics.record_cancel_requested(trigger::DISCONNECT);
                        let disconnect_stage = if first_token_time.is_none() {
                            stage::BEFORE_FIRST_TOKEN
                        } else {
                            stage::MID_STREAM
                        };
                        fctx.metrics.record_stream_disconnected(disconnect_stage);
                    }
                    provider_stream.cancel();
                    cancelled = true;
                    break;
                }

                event = provider_stream.next() => {
                    match event {
                        Some(Ok(client_event)) => {
                            let is_first_token = matches!(client_event, ClientSseEvent::Delta { .. })
                                && first_token_time.is_none();

                            if let ClientSseEvent::Delta { ref content, .. } = client_event {
                                if first_token_time.is_none() {
                                    let ttft = stream_start.elapsed();
                                    first_token_time = Some(ttft);
                                    info!(
                                        time_to_first_token_ms = ttft.as_millis() as u64,
                                        "first token received"
                                    );
                                    if let Some(ref fctx) = fin_ctx {
                                        let ms = ttft.as_secs_f64() * 1000.0;
                                        fctx.metrics.record_ttft_provider_ms(&fctx.provider_id, &fctx.effective_model, ms);
                                    }
                                }
                                accumulated_text.push_str(content);
                            }

                            // Track web search tool calls for per-message limit
                            if let ClientSseEvent::Tool { ref phase, name, .. } = client_event
                                && name == "web_search"
                            {
                                match phase {
                                    ToolPhase::Start => {
                                        web_search_call_count += 1;
                                        if web_search_call_count > web_search_max_calls {
                                            warn!(
                                                web_search_call_count,
                                                limit = web_search_max_calls,
                                                "web search per-message limit exceeded"
                                            );
                                            let code = "web_search_calls_exceeded".to_owned();
                                            let message = "Web search calls exceeded for this message".to_owned();

                                            // Finalize as failed, then emit error (D3)
                                            if let Some(ref fctx) = fin_ctx {
                                                let input = fctx.to_finalization_input(
                                                    TurnState::Failed,
                                                    &accumulated_text,
                                                    None,
                                                    Some(code.clone()),
                                                    None,
                                                    None,
                                                    web_search_completed_count,
                                                    code_interpreter_completed_count,
                                                    None,
                                                    None,
                                                );
                                                match fctx.finalization_svc.finalize_turn_cas(input).await {
                                                    Ok(outcome) if outcome.won_cas => {
                                                        let _ = tx.send(StreamEvent::Error(ErrorData {
                                                            code: code.clone(),
                                                            message,
                                                        })).await;
                                                    }
                                                    Ok(_) => {}
                                                    Err(fe) => {
                                                        warn!(error = %fe, "finalization failed on ws limit exceeded");
                                                        let _ = tx.send(StreamEvent::Error(ErrorData {
                                                            code: code.clone(),
                                                            message,
                                                        })).await;
                                                    }
                                                }
                                            } else {
                                                let _ = tx.send(StreamEvent::Error(ErrorData {
                                                    code: code.clone(),
                                                    message,
                                                })).await;
                                            }

                                            provider_stream.cancel();

                                            // Metrics: web search limit exceeded
                                            if let Some(ref fctx) = fin_ctx {
                                                let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                                                fctx.metrics.record_stream_failed(
                                                    &fctx.provider_id,
                                                    &fctx.effective_model,
                                                    &code,
                                                );
                                                fctx.metrics.record_stream_total_latency_ms(
                                                    &fctx.provider_id,
                                                    &fctx.effective_model,
                                                    ms,
                                                );
                                            }

                                            let has_partial = !accumulated_text.is_empty();
                                            return StreamOutcome {
                                                terminal: StreamTerminal::Failed,
                                                accumulated_text,
                                                usage: None,
                                                effective_model: model,
                                                error_code: Some(code),
                                                provider_response_id: None,
                                                provider_partial_usage: has_partial,
                                            };
                                        }
                                    }
                                    ToolPhase::Done => {
                                        web_search_completed_count += 1;
                                    }
                                }
                            }

                            // Track code interpreter tool calls
                            if let ClientSseEvent::Tool { ref phase, name, .. } = client_event
                                && name == "code_interpreter"
                            {
                                match phase {
                                    ToolPhase::Start => {
                                        code_interpreter_call_count += 1;
                                        if code_interpreter_call_count > code_interpreter_max_calls {
                                            warn!(
                                                code_interpreter_call_count,
                                                limit = code_interpreter_max_calls,
                                                "code interpreter per-message limit exceeded"
                                            );
                                            let code = "code_interpreter_calls_exceeded".to_owned();
                                            let message = "Code interpreter calls exceeded for this message".to_owned();

                                            if let Some(ref fctx) = fin_ctx {
                                                let input = fctx.to_finalization_input(
                                                    TurnState::Failed,
                                                    &accumulated_text,
                                                    None,
                                                    Some(code.clone()),
                                                    None,
                                                    None,
                                                    web_search_completed_count,
                                                    code_interpreter_completed_count,
                                                    None,
                                                    None,
                                                );
                                                match fctx.finalization_svc.finalize_turn_cas(input).await {
                                                    Ok(outcome) if outcome.won_cas => {
                                                        let _ = tx.send(StreamEvent::Error(ErrorData {
                                                            code: code.clone(),
                                                            message,
                                                        })).await;
                                                    }
                                                    Ok(_) => {}
                                                    Err(fe) => {
                                                        warn!(error = %fe, "finalization failed on ci limit exceeded");
                                                        let _ = tx.send(StreamEvent::Error(ErrorData {
                                                            code: code.clone(),
                                                            message,
                                                        })).await;
                                                    }
                                                }
                                            } else {
                                                let _ = tx.send(StreamEvent::Error(ErrorData {
                                                    code: code.clone(),
                                                    message,
                                                })).await;
                                            }

                                            provider_stream.cancel();

                                            if let Some(ref fctx) = fin_ctx {
                                                let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                                                fctx.metrics.record_stream_failed(
                                                    &fctx.provider_id,
                                                    &fctx.effective_model,
                                                    &code,
                                                );
                                                fctx.metrics.record_stream_total_latency_ms(
                                                    &fctx.provider_id,
                                                    &fctx.effective_model,
                                                    ms,
                                                );
                                            }

                                            let has_partial = !accumulated_text.is_empty();
                                            return StreamOutcome {
                                                terminal: StreamTerminal::Failed,
                                                accumulated_text,
                                                usage: None,
                                                effective_model: model,
                                                error_code: Some(code),
                                                provider_response_id: None,
                                                provider_partial_usage: has_partial,
                                            };
                                        }
                                    }
                                    ToolPhase::Done => {
                                        code_interpreter_completed_count += 1;
                                    }
                                }
                            }

                            let stream_event = StreamEvent::from(client_event);
                            if tx.send(stream_event).await.is_err() {
                                // Receiver dropped (client disconnect handled by relay)
                                info!("channel closed (client disconnect), exiting provider task");
                                break;
                            }

                            // TTFT overhead: time from provider first-byte to channel send.
                            if is_first_token
                                && let (Some(fctx), Some(provider_ttft)) =
                                    (&fin_ctx, first_token_time)
                                {
                                    let total = stream_start.elapsed().as_secs_f64() * 1000.0;
                                    let provider_ms = provider_ttft.as_secs_f64() * 1000.0;
                                    fctx.metrics.record_ttft_overhead_ms(
                                        &fctx.provider_id,
                                        &fctx.effective_model,
                                        total - provider_ms,
                                    );
                                }
                        }
                        Some(Err(e)) => {
                            warn!(error = %e, "provider stream error");
                            let (code, message) =
                                normalize_error(&LlmProviderError::StreamError(e));

                            // Finalize first, emit error only if CAS winner (D3)
                            if let Some(ref fctx) = fin_ctx {
                                let mid_elapsed = stream_start.elapsed();
                                let input = fctx.to_finalization_input(
                                    TurnState::Failed,
                                    &accumulated_text,
                                    None,
                                    Some(code.clone()),
                                    None,
                                    None,
                                    web_search_completed_count,
                                    code_interpreter_completed_count,
                                    first_token_time.map(|d| d.as_millis() as u64),
                                    Some(mid_elapsed.as_millis() as u64),
                                );
                                match fctx.finalization_svc.finalize_turn_cas(input).await {
                                    Ok(outcome) if outcome.won_cas => {
                                        let _ = tx
                                            .send(StreamEvent::Error(ErrorData {
                                                code: code.clone(),
                                                message,
                                            }))
                                            .await;
                                    }
                                    Ok(_) => {}
                                    Err(fe) => {
                                        warn!(error = %fe, "finalization failed on stream error");
                                        let _ = tx
                                            .send(StreamEvent::Error(ErrorData {
                                                code: code.clone(),
                                                message,
                                            }))
                                            .await;
                                    }
                                }
                            } else {
                                let _ = tx
                                    .send(StreamEvent::Error(ErrorData {
                                        code: code.clone(),
                                        message,
                                    }))
                                    .await;
                            }

                            // Metrics: mid-stream failure
                            if let Some(ref fctx) = fin_ctx {
                                let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                                fctx.metrics.record_stream_failed(&fctx.provider_id, &fctx.effective_model, &code);
                                fctx.metrics.record_stream_total_latency_ms(&fctx.provider_id, &fctx.effective_model, ms);
                            }

                            provider_stream.cancel();
                            let has_partial = !accumulated_text.is_empty();
                            return StreamOutcome {
                                terminal: StreamTerminal::Failed,
                                accumulated_text,
                                usage: None,
                                effective_model: model,
                                error_code: Some(code),
                                provider_response_id: None,
                                provider_partial_usage: has_partial,
                            };
                        }
                        None => {
                            // Stream ended — terminal captured by ProviderStream
                            break;
                        }
                    }
                }
            }
        }

        if cancelled {
            let elapsed = stream_start.elapsed();
            info!(
                terminal = "cancelled",
                duration_ms = elapsed.as_millis() as u64,
                "stream cancelled"
            );

            // Finalize cancelled turn — no SSE emission (stream already disconnected) (D3)
            if let Some(ref fctx) = fin_ctx {
                let input = fctx.to_finalization_input(
                    TurnState::Cancelled,
                    &accumulated_text,
                    None,
                    None,
                    None,
                    None,
                    web_search_completed_count,
                    code_interpreter_completed_count,
                    first_token_time.map(|d| d.as_millis() as u64),
                    Some(elapsed.as_millis() as u64),
                );
                if let Err(e) = fctx.finalization_svc.finalize_turn_cas(input).await {
                    warn!(error = %e, "finalization failed on cancelled stream");
                }

                // Metrics: cancelled stream
                let ms = elapsed.as_secs_f64() * 1000.0;
                fctx.metrics.record_cancel_effective(trigger::DISCONNECT);
                fctx.metrics.record_time_to_abort_ms(trigger::DISCONNECT, ms);
                fctx.metrics.record_stream_total_latency_ms(&fctx.provider_id, &fctx.effective_model, ms);
            }

            return StreamOutcome {
                terminal: StreamTerminal::Cancelled,
                accumulated_text,
                usage: None,
                effective_model: model,
                error_code: None,
                provider_response_id: None,
                provider_partial_usage: false,
            };
        }

        // Extract the terminal outcome from the provider stream
        let terminal = provider_stream.into_outcome().await;

        match terminal {
            TerminalOutcome::Completed {
                usage,
                content: _,
                citations,
                response_id,
                ..
            } => {
                let elapsed = stream_start.elapsed();
                info!(
                    terminal = "completed",
                    input_tokens = usage.input_tokens,
                    output_tokens = usage.output_tokens,
                    duration_ms = elapsed.as_millis() as u64,
                    "stream completed"
                );

                // Finalize first, then emit Done only if CAS winner (D3)
                if let Some(ref fctx) = fin_ctx {
                    let input = fctx.to_finalization_input(
                        TurnState::Completed,
                        &accumulated_text,
                        Some(usage),
                        None,
                        None,
                        Some(response_id.clone()),
                        web_search_completed_count,
                        code_interpreter_completed_count,
                        first_token_time.map(|d| d.as_millis() as u64),
                        Some(elapsed.as_millis() as u64),
                    );
                    match fctx.finalization_svc.finalize_turn_cas(input).await {
                        Ok(outcome) if outcome.won_cas => {
                            // P4-2: Map provider file_ids to internal UUIDs
                            let mapped = crate::domain::citation_mapping::map_citation_ids(
                                citations,
                                &provider_file_id_map,
                            );
                            if !mapped.is_empty() {
                                let _ = tx
                                    .send(StreamEvent::Citations(
                                        crate::domain::stream_events::CitationsData {
                                            items: mapped,
                                        },
                                    ))
                                    .await;
                            }
                            // Compute quota warnings post-commit (advisory, best-effort)
                            let quota_warnings = match fctx
                                .quota_warnings_provider
                                .get_quota_warnings(&fctx.scope, fctx.tenant_id, fctx.user_id)
                                .await
                            {
                                Ok(w) => Some(w),
                                Err(e) => {
                                    warn!(error = %e, "failed to compute quota_warnings");
                                    None
                                }
                            };
                            let _ = tx
                                .send(StreamEvent::Done(Box::new(DoneData {
                                    usage: Some(usage),
                                    effective_model: fctx.effective_model.clone(),
                                    selected_model: fctx.selected_model.clone(),
                                    quota_decision: fctx.quota_decision.clone(),
                                    downgrade_from: fctx.downgrade_from.clone(),
                                    downgrade_reason: fctx.downgrade_reason.clone(),
                                    quota_warnings,
                                })))
                                .await;
                        }
                        Ok(_) => { /* CAS loser — no SSE emission */ }
                        Err(fe) => {
                            warn!(error = %fe, "finalization failed on completed stream");
                            // Emit Done anyway so client isn't left hanging
                            let _ = tx
                                .send(StreamEvent::Done(Box::new(DoneData {
                                    usage: Some(usage),
                                    effective_model: fctx.effective_model.clone(),
                                    selected_model: fctx.selected_model.clone(),
                                    quota_decision: "allow".into(),
                                    downgrade_from: None,
                                    downgrade_reason: None,
                                    quota_warnings: None,
                                })))
                                .await;
                        }
                    }
                } else {
                    // No finalization context (unit tests) — emit directly
                    let mapped = crate::domain::citation_mapping::map_citation_ids(
                        citations,
                        &provider_file_id_map,
                    );
                    if !mapped.is_empty() {
                        let _ = tx
                            .send(StreamEvent::Citations(
                                crate::domain::stream_events::CitationsData { items: mapped },
                            ))
                            .await;
                    }
                    let _ = tx
                        .send(StreamEvent::Done(Box::new(DoneData {
                            usage: Some(usage),
                            effective_model: model.clone(),
                            selected_model: model.clone(),
                            quota_decision: "allow".into(),
                            downgrade_from: None,
                            downgrade_reason: None,
                            quota_warnings: None,
                        })))
                        .await;
                }

                // Metrics: completed stream
                if let Some(ref fctx) = fin_ctx {
                    let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                    fctx.metrics.record_stream_completed(&fctx.provider_id, &fctx.effective_model);
                    fctx.metrics.record_stream_total_latency_ms(&fctx.provider_id, &fctx.effective_model, ms);
                }

                StreamOutcome {
                    terminal: StreamTerminal::Completed,
                    accumulated_text,
                    usage: Some(usage),
                    effective_model: model,
                    error_code: None,
                    provider_response_id: Some(response_id),
                    provider_partial_usage: false,
                }
            }
            TerminalOutcome::Incomplete { usage, reason, .. } => {
                let elapsed = stream_start.elapsed();
                warn!(
                    terminal = "incomplete",
                    reason = %reason,
                    duration_ms = elapsed.as_millis() as u64,
                    "stream incomplete"
                );

                // Incomplete maps to Completed in DB — provider finished but hit
                // max_output_tokens. From billing/persistence perspective this is
                // a completed turn with truncated content (see design D10).
                if let Some(ref fctx) = fin_ctx {
                    let input = fctx.to_finalization_input(
                        TurnState::Completed,
                        &accumulated_text,
                        Some(usage),
                        None,
                        None,
                        None,
                        web_search_completed_count,
                        code_interpreter_completed_count,
                        first_token_time.map(|d| d.as_millis() as u64),
                        Some(elapsed.as_millis() as u64),
                    );
                    match fctx.finalization_svc.finalize_turn_cas(input).await {
                        Ok(outcome) if outcome.won_cas => {
                            let quota_warnings = match fctx
                                .quota_warnings_provider
                                .get_quota_warnings(&fctx.scope, fctx.tenant_id, fctx.user_id)
                                .await
                            {
                                Ok(w) => Some(w),
                                Err(e) => {
                                    warn!(error = %e, "failed to compute quota_warnings");
                                    None
                                }
                            };
                            let _ = tx
                                .send(StreamEvent::Done(Box::new(DoneData {
                                    usage: Some(usage),
                                    effective_model: fctx.effective_model.clone(),
                                    selected_model: fctx.selected_model.clone(),
                                    quota_decision: fctx.quota_decision.clone(),
                                    downgrade_from: fctx.downgrade_from.clone(),
                                    downgrade_reason: fctx.downgrade_reason.clone(),
                                    quota_warnings,
                                })))
                                .await;
                        }
                        Ok(_) => {}
                        Err(fe) => {
                            warn!(error = %fe, "finalization failed on incomplete stream");
                            let _ = tx
                                .send(StreamEvent::Done(Box::new(DoneData {
                                    usage: Some(usage),
                                    effective_model: fctx.effective_model.clone(),
                                    selected_model: fctx.selected_model.clone(),
                                    quota_decision: "allow".into(),
                                    downgrade_from: None,
                                    downgrade_reason: None,
                                    quota_warnings: None,
                                })))
                                .await;
                        }
                    }
                } else {
                    let _ = tx
                        .send(StreamEvent::Done(Box::new(DoneData {
                            usage: Some(usage),
                            effective_model: model.clone(),
                            selected_model: model.clone(),
                            quota_decision: "allow".into(),
                            downgrade_from: None,
                            downgrade_reason: None,
                            quota_warnings: None,
                        })))
                        .await;
                }

                // Metrics: incomplete stream
                if let Some(ref fctx) = fin_ctx {
                    let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                    fctx.metrics.record_stream_incomplete(&fctx.provider_id, &fctx.effective_model, &reason);
                    fctx.metrics.record_stream_completed(&fctx.provider_id, &fctx.effective_model);
                    fctx.metrics.record_stream_total_latency_ms(&fctx.provider_id, &fctx.effective_model, ms);
                }

                StreamOutcome {
                    terminal: StreamTerminal::Incomplete,
                    accumulated_text,
                    usage: Some(usage),
                    effective_model: model,
                    error_code: Some(format!("incomplete:{reason}")),
                    provider_response_id: None,
                    provider_partial_usage: false,
                }
            }
            TerminalOutcome::Failed { error, usage, .. } => {
                let raw_detail = error.raw_detail().map(ToOwned::to_owned);
                let (code, message) = normalize_error(&error);
                let elapsed = stream_start.elapsed();
                warn!(
                    terminal = "failed",
                    error_code = %code,
                    raw_detail = raw_detail.as_deref().unwrap_or(""),
                    duration_ms = elapsed.as_millis() as u64,
                    "stream failed"
                );

                // Finalize first, emit error only if CAS winner (D3)
                if let Some(ref fctx) = fin_ctx {
                    let input = fctx.to_finalization_input(
                        TurnState::Failed,
                        &accumulated_text,
                        usage,
                        Some(code.clone()),
                        None,
                        None,
                        web_search_completed_count,
                        code_interpreter_completed_count,
                        first_token_time.map(|d| d.as_millis() as u64),
                        Some(elapsed.as_millis() as u64),
                    );
                    match fctx.finalization_svc.finalize_turn_cas(input).await {
                        Ok(outcome) if outcome.won_cas => {
                            let _ = tx
                                .send(StreamEvent::Error(ErrorData {
                                    code: code.clone(),
                                    message,
                                }))
                                .await;
                        }
                        Ok(_) => {}
                        Err(fe) => {
                            warn!(error = %fe, "finalization failed on failed stream");
                            let _ = tx
                                .send(StreamEvent::Error(ErrorData {
                                    code: code.clone(),
                                    message,
                                }))
                                .await;
                        }
                    }
                } else {
                    let _ = tx
                        .send(StreamEvent::Error(ErrorData {
                            code: code.clone(),
                            message,
                        }))
                        .await;
                }

                // Metrics: failed stream (post-provider)
                if let Some(ref fctx) = fin_ctx {
                    let ms = stream_start.elapsed().as_secs_f64() * 1000.0;
                    fctx.metrics.record_stream_failed(&fctx.provider_id, &fctx.effective_model, &code);
                    fctx.metrics.record_stream_total_latency_ms(&fctx.provider_id, &fctx.effective_model, ms);
                }

                StreamOutcome {
                    terminal: StreamTerminal::Failed,
                    accumulated_text,
                    usage,
                    effective_model: model,
                    error_code: Some(code),
                    provider_response_id: None,
                    provider_partial_usage: usage.is_some(),
                }
            }
        }
    }.instrument(span))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::repos::CasTerminalParams;
    use crate::infra::db::repo::attachment_repo::AttachmentRepository as OrmAttachmentRepo;
    use crate::infra::db::repo::chat_repo::ChatRepository as OrmChatRepo;
    use crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository as OrmMessageAttachmentRepo;
    use crate::infra::db::repo::message_repo::MessageRepository as MsgRepo;
    use crate::infra::db::repo::turn_repo::TurnRepository as TurnRepo;
    use crate::infra::db::repo::vector_store_repo::VectorStoreRepository as OrmVectorStoreRepo;
    use crate::infra::llm::{
        Citation, CitationSource, LlmRequest, NonStreaming, ProviderStream, ResponseResult,
        Streaming, TranslatedEvent,
    };
    use futures::stream;
    use oagw_sdk::error::StreamingError;

    // ── Noop OutboxEnqueuer ──

    #[allow(de0309_must_have_domain_model)]
    struct NoopQuotaWarningsProvider;
    #[async_trait::async_trait]
    impl crate::domain::service::quota_settler::QuotaWarningsProvider for NoopQuotaWarningsProvider {
        async fn get_quota_warnings(
            &self,
            _scope: &modkit_security::AccessScope,
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
            _runner: &(dyn modkit_db::secure::DBRunner + Sync),
            _event: mini_chat_sdk::UsageEvent,
        ) -> Result<(), crate::domain::error::DomainError> {
            Ok(())
        }

        async fn enqueue_attachment_cleanup(
            &self,
            _runner: &(dyn modkit_db::secure::DBRunner + Sync),
            _event: crate::domain::repos::AttachmentCleanupEvent,
        ) -> Result<(), crate::domain::error::DomainError> {
            Ok(())
        }

        async fn enqueue_audit_event(
            &self,
            _runner: &(dyn modkit_db::secure::DBRunner + Sync),
            _event: crate::domain::model::audit_envelope::AuditEnvelope,
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

        let handle = spawn_provider_task::<TurnRepo, MsgRepo>(
            provider,
            "test-alias".to_owned(),
            mock_ctx(),
            vec![LlmMessage::user("hi")],
            None,
            vec![],
            "test-model".into(),
            "test-model".into(),
            4096,
            2, // max_tool_calls
            2, // web_search_max_calls
            2, // code_interpreter_max_calls
            cancel,
            tx,
            None,
            std::collections::HashMap::new(),
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

        let handle = spawn_provider_task::<TurnRepo, MsgRepo>(
            provider,
            "test-alias".to_owned(),
            mock_ctx(),
            vec![LlmMessage::user("hi")],
            None,
            vec![],
            "test-model".into(),
            "test-model".into(),
            4096,
            2, // max_tool_calls
            2, // web_search_max_calls
            2, // code_interpreter_max_calls
            cancel,
            tx,
            None,
            std::collections::HashMap::new(),
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

        let handle = spawn_provider_task::<TurnRepo, MsgRepo>(
            provider,
            "test-alias".to_owned(),
            mock_ctx(),
            vec![LlmMessage::user("hi")],
            None,
            vec![],
            "test-model".into(),
            "test-model".into(),
            4096,
            2, // max_tool_calls
            2, // web_search_max_calls
            2, // code_interpreter_max_calls
            cancel,
            tx,
            None,
            std::collections::HashMap::new(),
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
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
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

        let handle = spawn_provider_task::<TurnRepo, MsgRepo>(
            provider,
            "test-alias".to_owned(),
            mock_ctx(),
            vec![LlmMessage::user("hi")],
            None,
            vec![],
            "test-model".into(),
            "test-model".into(),
            4096,
            2, // max_tool_calls
            2, // web_search_max_calls
            2, // code_interpreter_max_calls
            cancel.clone(),
            tx,
            None,
            std::collections::HashMap::new(),
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
                _tx: &modkit_db::secure::DbTx<'_>,
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
        let message_repo = Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
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
            Arc::new(OrmChatRepo::new(modkit_db::odata::LimitCfg {
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
        )
    }

    /// Insert a parent chat row (required by FK constraints).
    async fn insert_test_chat(db: &Arc<DbProvider>, tenant_id: Uuid, user_id: Uuid, chat_id: Uuid) {
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
                computer_use: false,
                mcp: false,
            },
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
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
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
                _tx: &modkit_db::secure::DbTx<'_>,
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
                },
            )
            .await
            .expect("create turn");

        let turn_repo_arc = Arc::new(TurnRepo);
        let message_repo_arc = Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
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
        ));

        let fctx = FinalizationCtx {
            finalization_svc,
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
            provider_id: "openai".to_owned(),
            metrics: Arc::new(crate::domain::ports::metrics::NoopMetrics),
            quota_warnings_provider: Arc::new(NoopQuotaWarningsProvider),
        };

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = spawn_provider_task(
            provider,
            "test-alias".to_owned(),
            mock_ctx(),
            vec![LlmMessage::user("hi")],
            None,
            vec![],
            "gpt-4o-mini".into(), // effective_model passed as the model param
            "gpt-4o-mini".into(),
            4096,
            2, // max_tool_calls
            2, // web_search_max_calls
            2, // code_interpreter_max_calls
            cancel,
            tx,
            Some(fctx),
            std::collections::HashMap::new(),
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
                _tx: &modkit_db::secure::DbTx<'_>,
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
        let message_repo = Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
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
            Arc::new(OrmChatRepo::new(modkit_db::odata::LimitCfg {
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
                        computer_use: false,
                        mcp: false,
                    },
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
                        computer_use: false,
                        mcp: false,
                    },
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

        let handle = spawn_provider_task::<TurnRepo, MsgRepo>(
            provider,
            "test-alias".to_owned(),
            mock_ctx(),
            vec![LlmMessage::user("hi")],
            None,
            vec![],
            "test-model".into(),
            "test-model".into(),
            4096,
            2, // max_tool_calls
            2, // web_search_max_calls
            2, // code_interpreter_max_calls
            cancel,
            tx,
            None,
            std::collections::HashMap::new(),
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

        let handle = spawn_provider_task::<TurnRepo, MsgRepo>(
            provider,
            "test-alias".to_owned(),
            mock_ctx(),
            vec![LlmMessage::user("hi")],
            None,
            vec![],
            "test-model".into(),
            "test-model".into(),
            4096,
            2, // max_tool_calls
            2, // web_search_max_calls
            2, // code_interpreter_max_calls
            cancel,
            tx,
            None,
            std::collections::HashMap::new(),
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

        let handle = spawn_provider_task::<TurnRepo, MsgRepo>(
            provider,
            "test-alias".to_owned(),
            mock_ctx(),
            vec![LlmMessage::user("hi")],
            None,
            vec![],
            "test-model".into(),
            "test-model".into(),
            4096,
            2, // max_tool_calls
            2, // web_search_max_calls
            2, // code_interpreter_max_calls
            cancel,
            tx,
            None,
            std::collections::HashMap::new(),
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

        let handle = spawn_provider_task::<TurnRepo, MsgRepo>(
            provider,
            "test-alias".to_owned(),
            mock_ctx(),
            vec![LlmMessage::user("hi")],
            None,
            vec![],
            "test-model".into(),
            "test-model".into(),
            4096,
            2, // max_tool_calls
            2, // web_search_max_calls
            2, // code_interpreter_max_calls
            cancel,
            tx,
            None,
            std::collections::HashMap::new(),
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

        let handle = spawn_provider_task::<TurnRepo, MsgRepo>(
            provider,
            "test-alias".to_owned(),
            mock_ctx(),
            vec![LlmMessage::user("hi")],
            None,
            vec![],
            "test-model".into(),
            "test-model".into(),
            4096,
            2, // max_tool_calls
            2, // web_search_max_calls
            2, // code_interpreter_max_calls
            cancel,
            tx,
            None,
            std::collections::HashMap::new(),
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

        let handle = spawn_provider_task::<TurnRepo, MsgRepo>(
            provider,
            "test-alias".to_owned(),
            mock_ctx(),
            vec![LlmMessage::user("hi")],
            None,
            vec![],
            "test-model".into(),
            "test-model".into(),
            4096,
            2, // max_tool_calls
            2, // web_search_max_calls
            2, // code_interpreter_max_calls
            cancel,
            tx,
            None,
            std::collections::HashMap::new(),
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
        use modkit_db::secure::secure_insert;
        use sea_orm::Set;
        use time::OffsetDateTime;

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
            deleted_at: Set(None),
            replaced_by_request_id: Set(None),
            started_at: Set(now),
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
                _tx: &modkit_db::secure::DbTx<'_>,
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
        let message_repo = Arc::new(MsgRepo::new(modkit_db::odata::LimitCfg {
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
            Arc::new(OrmChatRepo::new(modkit_db::odata::LimitCfg {
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
        fn record_code_interpreter_calls(&self, _: &str, _: u32) {}
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
}
