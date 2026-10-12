use std::sync::Arc;

use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use mini_chat_sdk::RequesterType;

use crate::domain::error::DomainError;
use crate::domain::llm::Usage;
use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::repos::{MessageRepository, TurnRepository};
use crate::infra::db::entity::chat_turn::TurnState;
use crate::infra::llm::{FeatureFlag, LlmProviderError, LlmTool};

use super::super::DbProvider;

// ── RAII guard for active_streams gauge ──────────────────────────────────

/// Ensures `decrement_active_streams` is always called when the guard is
/// dropped, even if a new exit path is added without an explicit decrement.
#[domain_model]
pub(super) struct ActiveStreamGuard(pub(super) Arc<dyn MiniChatMetricsPort>);

impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        self.0.decrement_active_streams();
    }
}

// ── Typed error for attachment validation inside TX boundary ─────────────

#[allow(de0309_must_have_domain_model)]
#[derive(Debug, thiserror::Error)]
#[error("invalid attachment: {message}")]
pub(super) struct InvalidAttachmentError {
    pub(super) message: String,
}

/// A concurrent request reserved between the preflight check and the reserve
/// write, and this reserve no longer fits: the same 429 as a preflight reject.
pub(super) fn reserve_limit_exceeded() -> StreamError {
    StreamError::QuotaExhausted {
        error_code: "quota_exceeded".to_owned(),
        http_status: 429,
        quota_scope: "tokens".to_owned(),
    }
}

pub(super) fn attachment_err(message: impl Into<String>) -> toolkit_db::DbError {
    toolkit_db::DbError::Other(anyhow::Error::new(InvalidAttachmentError {
        message: message.into(),
    }))
}

/// Collects [`FeatureFlag`]s from the tools attached to a request, used to
/// populate [`RequestMetadata`] for observability.
pub(super) fn determine_features(tools: &[LlmTool]) -> Vec<FeatureFlag> {
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
    Replay {
        turn: Box<crate::infra::db::entity::chat_turn::Model>,
    },
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
    /// User message content exceeds the model's maximum input token limit.
    InputTooLong {
        estimated_tokens: u64,
        max_input_tokens: u32,
    },
}

impl From<authz_resolver_sdk::EnforcerError> for StreamError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        // Every enforcer failure denies access (fail closed).
        Self::AuthorizationFailed {
            source: DomainError::from(e),
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
pub(super) struct FinalizationCtx<TR: TurnRepository + 'static, MR: MessageRepository + 'static> {
    pub(super) finalization_svc:
        Arc<crate::domain::service::finalization_service::FinalizationService<TR, MR>>,
    pub(super) db: Arc<DbProvider>,
    pub(super) turn_repo: Arc<TR>,
    pub(super) scope: AccessScope,
    pub(super) turn_id: Uuid,
    pub(super) tenant_id: Uuid,
    pub(super) chat_id: Uuid,
    pub(super) request_id: Uuid,
    pub(super) user_id: Uuid,
    pub(super) requester_type: RequesterType,
    /// Pre-generated assistant message ID, sent in `StreamStartedData` (`stream_started` event).
    pub(super) message_id: Uuid,
    // ── Quota/preflight fields (from PreflightDecision) ──
    pub(super) effective_model: String,
    pub(super) selected_model: String,
    pub(super) reserve_tokens: i64,
    pub(super) max_output_tokens_applied: i32,
    pub(super) reserved_credits_micro: i64,
    pub(super) policy_version_applied: i64,
    pub(super) minimal_generation_floor_applied: i32,
    pub(super) quota_decision: String,
    pub(super) downgrade_from: Option<String>,
    pub(super) downgrade_reason: Option<String>,
    pub(super) period_starts: Vec<(
        crate::infra::db::entity::quota_usage::PeriodType,
        time::Date,
    )>,
    /// Context window size of the effective model (tokens) — for summary trigger.
    pub(super) context_window: u32,
    /// Input token limit of the effective model — for summary trigger.
    pub(super) max_input_tokens: u32,
    /// Estimated input tokens from context assembly (all messages + system prompt).
    pub(super) assembled_context_tokens: u64,
    /// `true` when context assembly dropped older messages due to budget.
    pub(super) messages_truncated: bool,
    /// Provider ID for metrics labels.
    pub(super) provider_id: String,
    /// Metrics port for recording stream metrics in the spawned task.
    pub(super) metrics: Arc<dyn MiniChatMetricsPort>,
    /// Quota warnings provider for computing `quota_warnings` in the `done` event.
    pub(super) quota_warnings_provider:
        Arc<dyn crate::domain::service::quota_settler::QuotaWarningsProvider>,
}

impl<TR: TurnRepository + 'static, MR: MessageRepository + 'static> FinalizationCtx<TR, MR> {
    /// Build a [`FinalizationInput`] from this context and stream outcome data.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn to_finalization_input(
        &self,
        terminal_state: TurnState,
        accumulated_text: &str,
        usage: Option<Usage>,
        error_code: Option<String>,
        error_detail: Option<String>,
        provider_response_id: Option<String>,
        web_search_calls: u32,
        code_interpreter_calls: u32,
        file_search_calls: u32,
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
            file_search_calls,
            context_window: self.context_window,
            max_input_tokens: self.max_input_tokens,
            assembled_context_tokens: self.assembled_context_tokens,
            messages_truncated: self.messages_truncated,
            ttft_ms,
            total_ms,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Input token validation
// ════════════════════════════════════════════════════════════════════════════

/// Estimate text-only tokens for `content` and return `Err(InputTooLong)` if
/// the estimate exceeds the model's `max_input_tokens`.
///
/// Surcharges (images, tools, web search, code interpreter) are intentionally
/// excluded: this is a fast pre-flight guard on the raw message text, not a
/// full context budget check.
pub(super) fn check_input_token_limit(
    content: &str,
    pf: &PreflightResult,
) -> Result<(), StreamError> {
    let estimate = super::super::token_estimator::estimate_tokens(
        &super::super::token_estimator::EstimationInput {
            utf8_bytes: content.len() as u64,
            num_images: 0,
            tools_enabled: false,
            web_search_enabled: false,
            code_interpreter_enabled: false,
        },
        &pf.estimation_budgets,
    );
    // 0 means the catalog sets no separate input limit, as in the context budget.
    if pf.max_input_tokens > 0 && estimate.estimated_input_tokens > u64::from(pf.max_input_tokens) {
        return Err(StreamError::InputTooLong {
            estimated_tokens: estimate.estimated_input_tokens,
            max_input_tokens: pf.max_input_tokens,
        });
    }
    Ok(())
}

// ════════════════════════════════════════════════════════════════════════════
// Error normalization
// ════════════════════════════════════════════════════════════════════════════

/// Map an optional subject-type string from [`SecurityContext`] to [`RequesterType`].
pub(super) fn requester_type_from_str(s: Option<&str>) -> RequesterType {
    match s {
        Some("system") => RequesterType::System,
        _ => RequesterType::User,
    }
}

/// Value stored in `chat_turns.requester_type` (`CHECK IN ('user', 'system')`).
///
/// The authenticated `subject_type` is a GTS type id (for example
/// `gts.cf.core.security.subject_user.v1~`), not a column value, so it must be
/// mapped rather than stored verbatim.
pub fn requester_type_column(subject_type: Option<&str>) -> &'static str {
    match requester_type_from_str(subject_type) {
        RequesterType::User => "user",
        RequesterType::System => "system",
    }
}

/// Normalize an [`LlmProviderError`] to a `(code, message)` pair for the SSE
/// error event. Messages are already sanitized by the infra layer.
///
/// The `code` is a stable token the UI maps to a category (see
/// `error-messages.ts` `CODE_MAP`). The `message` is shown verbatim as the
/// detail text and is intentionally crafted to be informative — the upstream
/// provider's classification, when available, is preserved here so operators
/// debugging from the UI alone can identify whether a failure is a
/// `rate_limit_error`, `invalid_request_error`, etc.
pub(super) fn normalize_error(err: &LlmProviderError) -> (String, String) {
    match err {
        LlmProviderError::RateLimited { retry_after_secs } => {
            let message = match retry_after_secs {
                Some(secs) => format!("Rate limited by provider; retry in {secs}s"),
                None => "Rate limited by provider".to_owned(),
            };
            ("rate_limited".to_owned(), message)
        }
        LlmProviderError::Timeout => (
            "provider_timeout".to_owned(),
            "Provider request timed out".to_owned(),
        ),
        LlmProviderError::ProviderError { message, .. } => {
            // `message` already carries the upstream code and HTTP status —
            // see `parse_anthropic_error` and friends. Pass it through so the
            // UI shows the full story.
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
pub(super) struct PreflightResult {
    pub(super) effective_model: String,
    pub(super) effective_provider_model_id: String,
    /// Provider routing key of the effective model; after a downgrade it can
    /// differ from the selected model's provider.
    pub(super) effective_provider_id: String,
    pub(super) reserve_tokens: i64,
    pub(super) max_output_tokens_applied: i32,
    pub(super) reserved_credits_micro: i64,
    pub(super) policy_version_applied: i64,
    pub(super) minimal_generation_floor_applied: i32,
    pub(super) quota_decision: String,
    pub(super) downgrade_from: Option<String>,
    pub(super) downgrade_reason: Option<String>,
    pub(super) system_prompt: String,
    pub(super) context_window: u32,
    pub(super) max_input_tokens: u32,
    pub(super) estimation_budgets: crate::config::EstimationBudgets,
    pub(super) file_search_max_num_results: u32,
    pub(super) max_tool_calls: u32,
    pub(super) tool_support: mini_chat_sdk::ModelToolSupport,
    pub(super) api_params: mini_chat_sdk::ModelApiParams,
    pub(super) web_search_context_size: mini_chat_sdk::models::WebSearchContextSize,
    /// Whether the effective model accepts image input.
    pub(super) vision_input: bool,
}

/// Convert a `PreflightDecision` into a flat `PreflightResult` or a `StreamError`.
pub(super) fn flatten_preflight(
    decision: crate::domain::model::quota::PreflightDecision,
) -> Result<PreflightResult, StreamError> {
    use crate::domain::model::quota::PreflightDecision;
    match decision {
        PreflightDecision::Allow {
            effective_model,
            effective_provider_model_id,
            effective_provider_id,
            reserve_tokens,
            max_output_tokens_applied,
            reserved_credits_micro,
            policy_version_applied,
            minimal_generation_floor_applied,
            system_prompt,
            context_window,
            max_input_tokens,
            estimation_budgets,
            file_search_max_num_results,
            max_tool_calls,
            tool_support,
            api_params,
            web_search_context_size,
            vision_input,
            ..
        } => Ok(PreflightResult {
            effective_model,
            effective_provider_model_id,
            effective_provider_id,
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
            max_input_tokens,
            estimation_budgets,
            file_search_max_num_results,
            max_tool_calls,
            tool_support,
            api_params,
            web_search_context_size,
            vision_input,
        }),
        PreflightDecision::Downgrade {
            effective_model,
            effective_provider_model_id,
            effective_provider_id,
            reserve_tokens,
            max_output_tokens_applied,
            reserved_credits_micro,
            policy_version_applied,
            minimal_generation_floor_applied,
            downgrade_from,
            downgrade_reason,
            system_prompt,
            context_window,
            max_input_tokens,
            estimation_budgets,
            file_search_max_num_results,
            max_tool_calls,
            tool_support,
            api_params,
            web_search_context_size,
            vision_input,
            ..
        } => Ok(PreflightResult {
            effective_model,
            effective_provider_model_id,
            effective_provider_id,
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
            max_input_tokens,
            estimation_budgets,
            file_search_max_num_results,
            max_tool_calls,
            tool_support,
            api_params,
            web_search_context_size,
            vision_input,
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

/// Interval between progress timestamp updates for orphan detection.
pub(super) const PROGRESS_UPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
