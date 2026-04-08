//! Domain-level SSE stream event types.
//!
//! These types are the canonical representation of streaming events used by
//! the domain service layer. Axum-specific SSE conversion lives in
//! `api::rest::sse`.

use modkit_macros::domain_model;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use uuid::Uuid;

use crate::domain::llm::{Citation, ToolPhase, Usage};

// ════════════════════════════════════════════════════════════════════════════
// StreamEvent — domain-level event envelope
// ════════════════════════════════════════════════════════════════════════════

/// Stream event envelope for the `messages:stream` pipeline.
///
/// Each variant maps to a distinct SSE `event:` name and `data:` JSON payload.
/// Ordering grammar: `stream_started ping* (delta | tool)* citations? (done | error)`.
#[domain_model]
#[derive(Debug, Clone, ToSchema)]
pub enum StreamEvent {
    StreamStarted(StreamStartedData),
    Ping,
    Delta(DeltaData),
    Tool(ToolData),
    Citations(CitationsData),
    Done(Box<DoneData>),
    Error(ErrorData),
}

/// Delta text chunk.
#[domain_model]
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DeltaData {
    pub r#type: &'static str,
    pub content: String,
}

/// Tool lifecycle event.
#[domain_model]
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ToolData {
    pub phase: ToolPhase,
    pub name: String,
    pub details: serde_json::Value,
}

/// Citations from provider annotations.
#[domain_model]
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CitationsData {
    pub items: Vec<Citation>,
}

/// Successful stream completion.
#[domain_model]
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DoneData {
    pub usage: Option<Usage>,
    pub effective_model: String,
    pub selected_model: String,
    pub quota_decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downgrade_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downgrade_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_warnings: Option<Vec<QuotaWarning>>,
}

/// Stream error (terminal).
#[domain_model]
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorData {
    pub code: String,
    pub message: String,
}

/// Stream header event carrying the stream request ID and server-generated
/// assistant message ID.
///
/// Emitted as the first event in every SSE stream (both new generations and
/// replays). `is_new_turn` distinguishes replayed completed turns from live
/// generations.
#[domain_model]
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StreamStartedData {
    pub request_id: Uuid,
    pub message_id: Uuid,
    /// `true` for a live generation (new turn); `false` when the stream
    /// replays an already-completed turn (idempotent replay).
    pub is_new_turn: bool,
}

/// Quota tier classification.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotaTier {
    Premium,
    Total,
}

/// Quota period classification.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuotaPeriod {
    Daily,
    Monthly,
}

/// Per-tier, per-period quota warning entry in the SSE `done` event.
#[domain_model]
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QuotaWarning {
    pub tier: QuotaTier,
    pub period: QuotaPeriod,
    pub remaining_percentage: u8,
    pub warning: bool,
    pub exhausted: bool,
}

// ════════════════════════════════════════════════════════════════════════════
// StreamEventKind — coarse classification for ordering enforcement
// ════════════════════════════════════════════════════════════════════════════

/// Coarse event classification for ordering enforcement.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEventKind {
    StreamStarted,
    Ping,
    Delta,
    Tool,
    Citations,
    Terminal,
}

impl StreamEvent {
    /// Classify this event for the [`StreamPhase`](crate::api::rest::sse::StreamPhase)
    /// state machine.
    #[must_use]
    pub fn event_kind(&self) -> StreamEventKind {
        match self {
            StreamEvent::StreamStarted(_) => StreamEventKind::StreamStarted,
            StreamEvent::Ping => StreamEventKind::Ping,
            StreamEvent::Delta(_) => StreamEventKind::Delta,
            StreamEvent::Tool(_) => StreamEventKind::Tool,
            StreamEvent::Citations(_) => StreamEventKind::Citations,
            StreamEvent::Done(_) | StreamEvent::Error(_) => StreamEventKind::Terminal,
        }
    }

    /// Whether this is a terminal event (done or error).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, StreamEvent::Done(_) | StreamEvent::Error(_))
    }
}
