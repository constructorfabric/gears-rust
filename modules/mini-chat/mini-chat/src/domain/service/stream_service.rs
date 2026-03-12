use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use futures::StreamExt;
use modkit_macros::domain_model;
use modkit_security::{AccessScope, SecurityContext};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, warn};
use uuid::Uuid;

use crate::config::StreamingConfig;
use crate::domain::error::DomainError;
use crate::domain::models::ResolvedModel;
use crate::domain::repos::{
    ChatRepository, CreateTurnParams, InsertUserMessageParams, MessageRepository,
    QuotaUsageRepository, TurnRepository,
};
use crate::domain::stream_events::{DoneData, ErrorData, StreamEvent};
use crate::infra::db::entity::chat_turn::{Model as TurnModel, TurnState};
use crate::infra::llm::{
    ClientSseEvent, LlmMessage, LlmProvider, LlmProviderError, LlmRequestBuilder, LlmTool,
    TerminalOutcome, Usage, provider_resolver::ProviderResolver, request::ContentPart,
};

use super::retrieval_scope::RetrievalScope;
use super::{DbProvider, actions, resources};

/// Pre-resolved attachment context passed into the stream pipeline.
/// Computed by the handler from `validate_send_message_attachments` + `resolve_retrieval_scope`.
#[derive(Debug, Clone, Default)]
#[allow(clippy::redundant_pub_crate)]
pub(crate) struct StreamAttachmentContext {
    /// LLM tools to include (e.g. `file_search` with `vector_store_ids`).
    pub tools: Vec<LlmTool>,
    /// Provider file IDs for image attachments (multimodal input).
    pub image_file_ids: Vec<String>,
    /// Whether `file_search` is active (for preflight surcharge).
    pub file_search_enabled: bool,
    /// Number of images (for preflight estimation).
    pub num_images: u32,
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
    /// Pre-generated assistant message ID, also sent in `DoneData`.
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
}

impl<TR: TurnRepository + 'static, MR: MessageRepository + 'static> FinalizationCtx<TR, MR> {
    /// Build a [`FinalizationInput`] from this context and stream outcome data.
    fn to_finalization_input(
        &self,
        terminal_state: TurnState,
        accumulated_text: &str,
        usage: Option<Usage>,
        error_code: Option<String>,
        error_detail: Option<String>,
        provider_response_id: Option<String>,
    ) -> crate::domain::model::finalization::FinalizationInput {
        crate::domain::model::finalization::FinalizationInput {
            turn_id: self.turn_id,
            tenant_id: self.tenant_id,
            chat_id: self.chat_id,
            request_id: self.request_id,
            user_id: self.user_id,
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
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Error normalization
// ════════════════════════════════════════════════════════════════════════════

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
    reserve_tokens: i64,
    max_output_tokens_applied: i32,
    reserved_credits_micro: i64,
    policy_version_applied: i64,
    minimal_generation_floor_applied: i32,
    quota_decision: String,
    downgrade_from: Option<String>,
    downgrade_reason: Option<String>,
}

/// Convert a `PreflightDecision` into a flat `PreflightResult` or a `StreamError`.
fn flatten_preflight(
    decision: crate::domain::model::quota::PreflightDecision,
) -> Result<PreflightResult, StreamError> {
    use crate::domain::model::quota::PreflightDecision;
    match decision {
        PreflightDecision::Allow {
            effective_model,
            reserve_tokens,
            max_output_tokens_applied,
            reserved_credits_micro,
            policy_version_applied,
            minimal_generation_floor_applied,
        } => Ok(PreflightResult {
            effective_model,
            reserve_tokens,
            max_output_tokens_applied,
            reserved_credits_micro,
            policy_version_applied,
            minimal_generation_floor_applied,
            quota_decision: "allow".to_owned(),
            downgrade_from: None,
            downgrade_reason: None,
        }),
        PreflightDecision::Downgrade {
            effective_model,
            reserve_tokens,
            max_output_tokens_applied,
            reserved_credits_micro,
            policy_version_applied,
            minimal_generation_floor_applied,
            downgrade_from,
            downgrade_reason,
        } => Ok(PreflightResult {
            effective_model,
            reserve_tokens,
            max_output_tokens_applied,
            reserved_credits_micro,
            policy_version_applied,
            minimal_generation_floor_applied,
            quota_decision: "downgrade".to_owned(),
            downgrade_from: Some(downgrade_from),
            downgrade_reason: Some(downgrade_reason.as_str().to_owned()),
        }),
        PreflightDecision::Reject {
            error_code,
            http_status,
            ..
        } => Err(StreamError::QuotaExhausted {
            error_code,
            http_status,
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
}

impl<
    TR: TurnRepository + 'static,
    MR: MessageRepository + 'static,
    QR: QuotaUsageRepository + 'static,
    CR: ChatRepository,
> StreamService<TR, MR, QR, CR>
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
        }
    }

    /// The configured channel capacity for the provider->writer mpsc channel.
    pub(crate) fn channel_capacity(&self) -> usize {
        usize::from(self.streaming_config.sse_channel_capacity)
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_stream(
        &self,
        ctx: SecurityContext,
        chat_id: Uuid,
        request_id: Uuid,
        content: String,
        resolved_model: ResolvedModel,
        attachment_ctx: StreamAttachmentContext,
        cancel: CancellationToken,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<tokio::task::JoinHandle<StreamOutcome>, StreamError> {
        let ResolvedModel {
            model_id: model,
            provider_model_id,
            provider_id,
            ..
        } = resolved_model;
        let tenant_id = ctx.subject_tenant_id();
        let user_id = ctx.subject_id();

        // ── Authorization ──
        let scope = self
            .enforcer
            .access_scope(&ctx, &resources::CHAT, actions::SEND_MESSAGE, Some(chat_id))
            .await?;

        // Non-transactional connection for pre-stream checks (D6)
        let conn = self
            .db
            .conn()
            .map_err(|e| StreamError::TurnCreationFailed {
                source: DomainError::from(e),
            })?;

        // ── Verify chat exists (scoped) ──
        self.chat_repo
            .get(&conn, &scope, chat_id)
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?
            .ok_or(StreamError::ChatNotFound { chat_id })?;

        let scope = scope.tenant_only();

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

        // ── Preflight quota evaluate (external I/O, no DB writes) ──
        let selected_model = model.clone();
        let computed = self
            .quota
            .preflight_evaluate(crate::domain::model::quota::PreflightInput {
                tenant_id,
                user_id,
                selected_model: selected_model.clone(),
                utf8_bytes: content.len() as u64,
                num_images: attachment_ctx.num_images,
                tools_enabled: !attachment_ctx.tools.is_empty(),
                web_search_enabled: false,
                file_search_enabled: attachment_ctx.file_search_enabled,
                max_output_tokens_cap: self.streaming_config.max_output_tokens,
            })
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?;

        let pf = flatten_preflight(computed.decision.clone())?;
        // Period boundaries from the computed preflight (used by finalization for settlement)
        let period_starts = computed.periods.clone();

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
            )
            .await?;

        // Pre-generate assistant message ID (sent in DoneData and used in CAS)
        let message_id = Uuid::new_v4();

        let finalization_ctx = FinalizationCtx {
            finalization_svc: Arc::clone(&self.finalization),
            scope,
            turn_id,
            tenant_id,
            chat_id,
            request_id,
            user_id,
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
        };

        let resolved_provider = self.provider_resolver.resolve(&provider_id).map_err(|e| {
            StreamError::TurnCreationFailed {
                source: DomainError::internal(format!("provider resolution: {e}")),
            }
        })?;
        let api_path = resolved_provider
            .api_path
            .replace("{model}", &provider_model_id);
        let proxy_path = format!("{}{api_path}", resolved_provider.upstream_alias);

        Ok(spawn_provider_task(
            resolved_provider.adapter,
            proxy_path,
            ctx,
            content,
            model,
            provider_model_id,
            pf.max_output_tokens_applied.cast_unsigned(),
            cancel,
            tx,
            Some(finalization_ctx),
            attachment_ctx.tools,
            attachment_ctx.image_file_ids,
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
    ) -> Result<Uuid, StreamError> {
        let user_msg_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();

        let message_repo = Arc::clone(&self.message_repo);
        let turn_repo = Arc::clone(&self.turn_repo);
        let quota_repo = Arc::clone(&self.quota.repo);
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
            .map_err(|e| StreamError::TurnCreationFailed {
                source: match e {
                    modkit_db::DbError::Other(err) => match err.downcast::<DomainError>() {
                        Ok(domain_err) => domain_err,
                        Err(err) => DomainError::from(modkit_db::DbError::Other(err)),
                    },
                    other => DomainError::from(other),
                },
            })?;

        Ok(turn_id)
    }

    /// Run streaming for an already-created turn (used by retry/edit mutations).
    ///
    /// The mutation transaction has already created the turn (state=running) and
    /// user message. This method does quota preflight, writes reserves, resolves
    /// the provider, and spawns the streaming task.
    ///
    /// Per design D3: mutation transaction commits first, streaming runs post-commit.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_stream_for_mutation(
        &self,
        ctx: SecurityContext,
        chat_id: Uuid,
        request_id: Uuid,
        turn_id: Uuid,
        content: String,
        resolved_model: ResolvedModel,
        cancel: CancellationToken,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<tokio::task::JoinHandle<StreamOutcome>, StreamError> {
        let model = resolved_model.model_id;
        let provider_model_id = resolved_model.provider_model_id;
        let provider_id = resolved_model.provider_id;
        let tenant_id = ctx.subject_tenant_id();
        let user_id = ctx.subject_id();
        let scope = AccessScope::for_tenant(tenant_id);

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
                tools_enabled: false,
                web_search_enabled: false,
                file_search_enabled: false, // TODO: set from RetrievalScope
                max_output_tokens_cap: self.streaming_config.max_output_tokens,
            })
            .await
            .map_err(|e| StreamError::TurnCreationFailed { source: e })?;

        let pf = flatten_preflight(computed.decision.clone())?;
        let period_starts = computed.periods.clone();

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
        }

        // ── Build finalization context + resolve provider + spawn ────────
        let message_id = Uuid::new_v4();

        let finalization_ctx = FinalizationCtx {
            finalization_svc: Arc::clone(&self.finalization),
            scope,
            turn_id,
            tenant_id,
            chat_id,
            request_id,
            user_id,
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
        };

        let resolved_provider = self.provider_resolver.resolve(&provider_id).map_err(|e| {
            StreamError::TurnCreationFailed {
                source: DomainError::internal(format!("provider resolution: {e}")),
            }
        })?;
        let api_path = resolved_provider
            .api_path
            .replace("{model}", &provider_model_id);
        let proxy_path = format!("{}{api_path}", resolved_provider.upstream_alias);

        Ok(spawn_provider_task(
            resolved_provider.adapter,
            proxy_path,
            ctx,
            content,
            pf.effective_model,
            provider_model_id,
            pf.max_output_tokens_applied.cast_unsigned(),
            cancel,
            tx,
            Some(finalization_ctx),
            Vec::new(), // TODO: populate tools from retrieval scope
            Vec::new(), // TODO: populate image_file_ids from attachment_ids
        ))
    }
}

/// Core provider task: reads from the LLM, translates events, and returns
/// a [`StreamOutcome`]. After the stream ends, atomically finalizes the turn
/// via `FinalizationService::finalize_turn_cas()` if a context is provided.
///
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
    content: String,
    model: String,
    provider_model_id: String,
    max_output_tokens: u32,
    cancel: CancellationToken,
    tx: mpsc::Sender<StreamEvent>,
    fin_ctx: Option<FinalizationCtx<TR, MR>>,
    tools: Vec<LlmTool>,
    image_file_ids: Vec<String>,
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
        let msg_id_str = fin_ctx.as_ref().map(|p| p.message_id.to_string());

        // Build the LLM request using provider_model_id (the actual provider-facing name).
        // If image file_ids are present, build a multimodal user message.
        let user_message = if image_file_ids.is_empty() {
            LlmMessage::user(&content)
        } else {
            let mut parts = vec![ContentPart::Text { text: content.clone() }];
            for file_id in &image_file_ids {
                parts.push(ContentPart::Image {
                    file_id: file_id.clone(),
                });
            }
            LlmMessage {
                role: crate::infra::llm::request::Role::User,
                content: parts,
            }
        };

        let mut builder = LlmRequestBuilder::new(&provider_model_id)
            .message(user_message)
            .max_output_tokens(u64::from(max_output_tokens));

        for tool in tools {
            builder = builder.tool(tool);
        }

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

        loop {
            tokio::select! {
                biased;

                () = cancel.cancelled() => {
                    debug!("stream cancelled, aborting provider");
                    provider_stream.cancel();
                    cancelled = true;
                    break;
                }

                event = provider_stream.next() => {
                    match event {
                        Some(Ok(client_event)) => {
                            if let ClientSseEvent::Delta { ref content, .. } = client_event {
                                if first_token_time.is_none() {
                                    let ttft = stream_start.elapsed();
                                    first_token_time = Some(ttft);
                                    info!(
                                        time_to_first_token_ms = ttft.as_millis() as u64,
                                        "first token received"
                                    );
                                }
                                accumulated_text.push_str(content);
                            }
                            let stream_event = StreamEvent::from(client_event);
                            if tx.send(stream_event).await.is_err() {
                                // Receiver dropped (client disconnect handled by relay)
                                info!("channel closed (client disconnect), exiting provider task");
                                break;
                            }
                        }
                        Some(Err(e)) => {
                            warn!(error = %e, "provider stream error");
                            let (code, message) =
                                normalize_error(&LlmProviderError::StreamError(e));

                            // Finalize first, emit error only if CAS winner (D3)
                            if let Some(ref fctx) = fin_ctx {
                                let input = fctx.to_finalization_input(
                                    TurnState::Failed,
                                    &accumulated_text,
                                    None,
                                    Some(code.clone()),
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
                );
                if let Err(e) = fctx.finalization_svc.finalize_turn_cas(input).await {
                    warn!(error = %e, "finalization failed on cancelled stream");
                }
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
                    );
                    match fctx.finalization_svc.finalize_turn_cas(input).await {
                        Ok(outcome) if outcome.won_cas => {
                            if !citations.is_empty() {
                                let _ = tx
                                    .send(StreamEvent::Citations(
                                        crate::domain::stream_events::CitationsData {
                                            items: citations,
                                        },
                                    ))
                                    .await;
                            }
                            let _ = tx
                                .send(StreamEvent::Done(Box::new(DoneData {
                                    message_id: msg_id_str.clone(),
                                    usage: Some(usage),
                                    effective_model: fctx.effective_model.clone(),
                                    selected_model: fctx.selected_model.clone(),
                                    quota_decision: fctx.quota_decision.clone(),
                                    downgrade_from: fctx.downgrade_from.clone(),
                                    downgrade_reason: fctx.downgrade_reason.clone(),
                                })))
                                .await;
                        }
                        Ok(_) => { /* CAS loser — no SSE emission */ }
                        Err(fe) => {
                            warn!(error = %fe, "finalization failed on completed stream");
                            // Emit Done anyway so client isn't left hanging
                            let _ = tx
                                .send(StreamEvent::Done(Box::new(DoneData {
                                    message_id: msg_id_str.clone(),
                                    usage: Some(usage),
                                    effective_model: fctx.effective_model.clone(),
                                    selected_model: fctx.selected_model.clone(),
                                    quota_decision: "allow".into(),
                                    downgrade_from: None,
                                    downgrade_reason: None,
                                })))
                                .await;
                        }
                    }
                } else {
                    // No finalization context (unit tests) — emit directly
                    if !citations.is_empty() {
                        let _ = tx
                            .send(StreamEvent::Citations(
                                crate::domain::stream_events::CitationsData { items: citations },
                            ))
                            .await;
                    }
                    let _ = tx
                        .send(StreamEvent::Done(Box::new(DoneData {
                            message_id: msg_id_str.clone(),
                            usage: Some(usage),
                            effective_model: model.clone(),
                            selected_model: model.clone(),
                            quota_decision: "allow".into(),
                            downgrade_from: None,
                            downgrade_reason: None,
                        })))
                        .await;
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
                    );
                    match fctx.finalization_svc.finalize_turn_cas(input).await {
                        Ok(outcome) if outcome.won_cas => {
                            let _ = tx
                                .send(StreamEvent::Done(Box::new(DoneData {
                                    message_id: msg_id_str.clone(),
                                    usage: Some(usage),
                                    effective_model: fctx.effective_model.clone(),
                                    selected_model: fctx.selected_model.clone(),
                                    quota_decision: fctx.quota_decision.clone(),
                                    downgrade_from: fctx.downgrade_from.clone(),
                                    downgrade_reason: fctx.downgrade_reason.clone(),
                                })))
                                .await;
                        }
                        Ok(_) => {}
                        Err(fe) => {
                            warn!(error = %fe, "finalization failed on incomplete stream");
                            let _ = tx
                                .send(StreamEvent::Done(Box::new(DoneData {
                                    message_id: msg_id_str.clone(),
                                    usage: Some(usage),
                                    effective_model: fctx.effective_model.clone(),
                                    selected_model: fctx.selected_model.clone(),
                                    quota_decision: "allow".into(),
                                    downgrade_from: None,
                                    downgrade_reason: None,
                                })))
                                .await;
                        }
                    }
                } else {
                    let _ = tx
                        .send(StreamEvent::Done(Box::new(DoneData {
                            message_id: msg_id_str.clone(),
                            usage: Some(usage),
                            effective_model: model.clone(),
                            selected_model: model.clone(),
                            quota_decision: "allow".into(),
                            downgrade_from: None,
                            downgrade_reason: None,
                        })))
                        .await;
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
    use crate::infra::db::repo::chat_repo::ChatRepository as OrmChatRepo;
    use crate::infra::db::repo::message_repo::MessageRepository as MsgRepo;
    use crate::infra::db::repo::turn_repo::TurnRepository as TurnRepo;
    use crate::infra::llm::{
        LlmRequest, NonStreaming, ProviderStream, ResponseResult, Streaming, TranslatedEvent,
    };
    use futures::stream;
    use oagw_sdk::error::StreamingError;

    // ── Noop OutboxEnqueuer ──

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
                },
                partial_content: deltas.iter().copied().collect(),
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
            "hi".into(),
            "test-model".into(),
            "test-model".into(),
            4096,
            cancel,
            tx,
            None,
            Vec::new(),
            Vec::new(),
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
            "hi".into(),
            "test-model".into(),
            "test-model".into(),
            4096,
            cancel,
            tx,
            None,
            Vec::new(),
            Vec::new(),
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
            "hi".into(),
            "test-model".into(),
            "test-model".into(),
            4096,
            cancel,
            tx,
            None,
            Vec::new(),
            Vec::new(),
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
            "hi".into(),
            "test-model".into(),
            "test-model".into(),
            4096,
            cancel.clone(),
            tx,
            None,
            Vec::new(),
            Vec::new(),
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
        MockPolicySnapshotProvider, MockUserLimitsProvider, TestCatalogEntryParams, inmem_db,
        mock_db_provider, mock_enforcer, test_catalog_entry, test_security_ctx_with_id,
    };
    use crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository as OrmQuotaUsageRepo;

    /// Build a `StreamService` with real DB repos and a mock LLM provider.
    fn build_stream_service(
        db: Arc<DbProvider>,
        provider: Arc<dyn LlmProvider>,
    ) -> StreamService<TurnRepo, MsgRepo, OrmQuotaUsageRepo, OrmChatRepo> {
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
                StreamAttachmentContext::default(),
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
                StreamAttachmentContext::default(),
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
                StreamAttachmentContext::default(),
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
                StreamAttachmentContext::default(),
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
                StreamAttachmentContext::default(),
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
                StreamAttachmentContext::default(),
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
                StreamAttachmentContext::default(),
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
                StreamAttachmentContext::default(),
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
                StreamAttachmentContext::default(),
                cancel.clone(),
                tx,
            )
            .await
            .expect("should start stream");

        // Read the first delta
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
                StreamAttachmentContext::default(),
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
        ));

        let fctx = FinalizationCtx {
            finalization_svc,
            scope,
            turn_id,
            tenant_id,
            chat_id,
            request_id,
            user_id,
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
        };

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::completed(&["Hello"]));
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(32);
        let cancel = CancellationToken::new();

        let handle = spawn_provider_task(
            provider,
            "test-alias".to_owned(),
            mock_ctx(),
            "hi".into(),
            "gpt-4o-mini".into(), // effective_model passed as the model param
            "gpt-4o-mini".into(),
            4096,
            cancel,
            tx,
            Some(fctx),
            Vec::new(),
            Vec::new(),
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
    ) -> StreamService<TurnRepo, MsgRepo, OrmQuotaUsageRepo, OrmChatRepo> {
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
                StreamAttachmentContext::default(),
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
        let scope = AccessScope::allow_all().tenant_only();
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
                StreamAttachmentContext::default(),
                cancel,
                tx,
            )
            .await
            .expect_err("should be QuotaExhausted");

        match err {
            StreamError::QuotaExhausted {
                error_code,
                http_status,
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
                },
                StreamAttachmentContext::default(),
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
                StreamAttachmentContext::default(),
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
}
