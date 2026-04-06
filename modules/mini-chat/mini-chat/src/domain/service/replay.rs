//! Side-effect-free replay for completed turns.
//!
//! Structurally separated from the streaming execution path: this module
//! accepts only read-only dependencies (`DbProvider`, `MessageRepository`)
//! and cannot access `QuotaService`, outbox, provider, or finalization types.

use crate::domain::error::DomainError;
use crate::domain::llm::Usage;
use crate::domain::repos::MessageRepository;
use crate::domain::stream_events::{DeltaData, DoneData, StreamEvent, StreamStartedData};
use crate::infra::db::entity::chat_turn::Model as TurnModel;
use modkit_security::AccessScope;

use super::DbProvider;

/// Triple of SSE events produced by replay.
#[derive(Debug)]
#[allow(de0309_must_have_domain_model)]
pub struct ReplayEvents {
    pub stream_started: StreamEvent,
    pub delta: StreamEvent,
    pub done: StreamEvent,
}

/// Reconstruct SSE events from a completed turn's persisted data.
///
/// # Errors
/// - `DomainError::InternalError` if `assistant_message_id` is `None`
/// - `DomainError::InternalError` if the assistant message is not found
/// - `DomainError::Database` on connection / query failure
pub async fn replay_turn<MR: MessageRepository>(
    db: &DbProvider,
    message_repo: &MR,
    scope: &AccessScope,
    turn: &TurnModel,
    selected_model: &str,
) -> Result<ReplayEvents, DomainError> {
    let assistant_msg_id = turn.assistant_message_id.ok_or_else(|| {
        DomainError::internal(format!(
            "completed turn {} has no assistant_message_id",
            turn.id
        ))
    })?;

    let conn = db.conn().map_err(DomainError::from)?;

    let message = message_repo
        .get_by_chat(&conn, scope, assistant_msg_id, turn.chat_id)
        .await?
        .ok_or_else(|| {
            DomainError::internal(format!(
                "assistant message {} not found for turn {}",
                assistant_msg_id, turn.id
            ))
        })?;

    let stream_started = StreamEvent::StreamStarted(StreamStartedData {
        request_id: turn.request_id,
        message_id: assistant_msg_id,
        is_new_turn: false,
    });

    let delta = StreamEvent::Delta(DeltaData {
        r#type: "text",
        content: message.content,
    });

    let done = StreamEvent::Done(Box::new(DoneData {
        usage: Some(Usage {
            input_tokens: message.input_tokens,
            output_tokens: message.output_tokens,
            cache_read_input_tokens: message.cache_read_input_tokens,
            cache_write_input_tokens: message.cache_write_input_tokens,
            reasoning_tokens: message.reasoning_tokens,
        }),
        effective_model: turn.effective_model.clone().unwrap_or_default(),
        selected_model: selected_model.to_owned(),
        quota_decision: reconstruct_quota_decision(turn, selected_model),
        downgrade_from: reconstruct_downgrade_from(turn, selected_model),
        downgrade_reason: None,
        quota_warnings: None,
    }));

    Ok(ReplayEvents {
        stream_started,
        delta,
        done,
    })
}

fn reconstruct_quota_decision(turn: &TurnModel, selected_model: &str) -> String {
    match &turn.effective_model {
        Some(effective) if effective != selected_model => "downgrade".to_owned(),
        _ => "allow".to_owned(),
    }
}

fn reconstruct_downgrade_from(turn: &TurnModel, selected_model: &str) -> Option<String> {
    match &turn.effective_model {
        Some(effective) if effective != selected_model => Some(selected_model.to_owned()),
        _ => None,
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "replay_tests.rs"]
mod tests;
