use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Requester type for audit events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequesterType {
    User,
    System,
}

/// Attachment kind metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Document,
    Image,
}

/// Metadata for a file attachment included in a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentMetadata {
    pub attachment_id: Uuid,
    pub attachment_kind: AttachmentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_used_in_turn: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_summary: Option<String>,
}

/// Token usage reported by the provider for audit purposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditUsageTokens {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Latency measurements for a turn.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencyMs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<u64>,
}

/// License-level policy decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseDecision {
    pub feature: String,
    pub decision: String,
}

/// Quota scope for quota decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaScope {
    Tokens,
    WebSearch,
}

/// Quota-level policy decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaDecision {
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_scope: Option<QuotaScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downgrade_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downgrade_reason: Option<String>,
}

/// Combined policy decisions for a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyDecisions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<LicenseDecision>,
    pub quota: QuotaDecision,
}

/// Tool call counts for a turn.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCalls {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_search_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search_calls: Option<u64>,
}

/// Discriminator for [`TurnAuditEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnAuditEventType {
    TurnCompleted,
    TurnFailed,
}

impl std::fmt::Display for TurnAuditEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TurnCompleted => f.write_str("turn_completed"),
            Self::TurnFailed => f.write_str("turn_failed"),
        }
    }
}

/// Full turn audit event — emitted after a turn completes or fails.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnAuditEvent {
    pub event_type: TurnAuditEventType,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    pub tenant_id: Uuid,
    pub requester_type: RequesterType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,

    pub user_id: Uuid,
    pub chat_id: Uuid,
    pub turn_id: Uuid,
    pub request_id: Uuid,
    pub selected_model: String,
    pub effective_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_version_applied: Option<u64>,
    pub usage: AuditUsageTokens,
    pub latency_ms: LatencyMs,
    pub policy_decisions: PolicyDecisions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// User prompt text. The caller MUST redact secret patterns and truncate
    /// to 8 KiB before setting this field (see DESIGN.md "Audit content handling").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Assistant response text. The caller MUST redact secret patterns and
    /// truncate to 8 KiB before setting this field (see DESIGN.md "Audit content handling").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<ToolCalls>,
}

/// Discriminator for [`TurnMutationAuditEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnMutationAuditEventType {
    TurnRetry,
    TurnEdit,
}

impl std::fmt::Display for TurnMutationAuditEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TurnRetry => f.write_str("turn_retry"),
            Self::TurnEdit => f.write_str("turn_edit"),
        }
    }
}

/// Shared audit event structure for turn mutations (retry, edit).
///
/// Both retry and edit carry the same fields: the acting user, the chat,
/// the original request being replaced, and the new request that replaces it.
/// `event_type` distinguishes the two.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnMutationAuditEvent {
    pub event_type: TurnMutationAuditEventType,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    pub tenant_id: Uuid,
    pub requester_type: RequesterType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,

    pub actor_user_id: Uuid,
    pub chat_id: Uuid,
    pub turn_id: Uuid,
    pub original_request_id: Uuid,
    pub new_request_id: Uuid,
}

impl TurnMutationAuditEvent {
    /// Build a `turn_retry` audit event.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new_retry(
        timestamp: OffsetDateTime,
        tenant_id: Uuid,
        requester_type: RequesterType,
        trace_id: Option<String>,
        actor_user_id: Uuid,
        chat_id: Uuid,
        turn_id: Uuid,
        original_request_id: Uuid,
        new_request_id: Uuid,
    ) -> Self {
        Self {
            event_type: TurnMutationAuditEventType::TurnRetry,
            timestamp,
            tenant_id,
            requester_type,
            trace_id,
            actor_user_id,
            chat_id,
            turn_id,
            original_request_id,
            new_request_id,
        }
    }

    /// Build a `turn_edit` audit event.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new_edit(
        timestamp: OffsetDateTime,
        tenant_id: Uuid,
        requester_type: RequesterType,
        trace_id: Option<String>,
        actor_user_id: Uuid,
        chat_id: Uuid,
        turn_id: Uuid,
        original_request_id: Uuid,
        new_request_id: Uuid,
    ) -> Self {
        Self {
            event_type: TurnMutationAuditEventType::TurnEdit,
            timestamp,
            tenant_id,
            requester_type,
            trace_id,
            actor_user_id,
            chat_id,
            turn_id,
            original_request_id,
            new_request_id,
        }
    }
}

/// Audit event emitted when a user retries a turn.
pub type TurnRetryAuditEvent = TurnMutationAuditEvent;

/// Audit event emitted when a user edits a turn.
pub type TurnEditAuditEvent = TurnMutationAuditEvent;

/// Discriminator for [`TurnDeleteAuditEvent`].
///
/// Single-variant: the event type is always `"turn_delete"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnDeleteAuditEventType {
    #[default]
    #[serde(rename = "turn_delete")]
    TurnDelete,
}

impl std::fmt::Display for TurnDeleteAuditEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("turn_delete")
    }
}

/// Audit event emitted when a user deletes a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnDeleteAuditEvent {
    pub event_type: TurnDeleteAuditEventType,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    pub tenant_id: Uuid,
    pub requester_type: RequesterType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,

    pub actor_user_id: Uuid,
    pub chat_id: Uuid,
    pub turn_id: Uuid,
    pub request_id: Uuid,
}
