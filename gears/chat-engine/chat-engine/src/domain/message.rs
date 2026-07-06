//! Message domain primitives.
//!
//! Re-exports the SDK `Message`, `MessageRole`, `VariantInfo`, and the
//! NDJSON streaming-event types so callers have one canonical path. Adds
//! conversion impls between the SDK `Message` and the Phase 1 SeaORM
//! entity model.
//!
//! ### Schema drift between SDK and DB
//!
//! The SDK `Message` has both `created_at` and `updated_at`, but the Phase
//! 1 `messages` table only stores `created_at` (messages are immutable
//! tree nodes per ADR-0001 — `updated_at` exists in the SDK for streaming
//! placeholders that get filled in). On read we synthesize
//! `updated_at = created_at`; on write the `ActiveModel` simply doesn't
//! carry the field.
//!
//! ### `file_ids` shape
//!
//! Phase 1 stores `messages.file_ids` as JSONB array of UUID strings (see
//! `out/phase-01-db-contract.md`). The SDK `file_ids: Vec<Uuid>` is mapped
//! to / from that JSON via `serde_json`.
//
// @cpt-cf-chat-engine-domain-message:p2

pub use chat_engine_sdk::models::{
    Message, MessagePart, MessagePartInput, MessagePartType, MessageRole, StreamingChunkEvent,
    StreamingCompleteEvent, StreamingErrorEvent, StreamingEvent, StreamingStartEvent, TenantId,
    UserId, VariantInfo,
};

/// Concatenate the bodies of all `text`-typed parts of a message in `number`
/// order, joined by newlines. Non-text parts contribute nothing. This is the
/// canonical "plain text of a message" used by search matching, export
/// rendering, and any caller that needs a flat string view of the body.
#[must_use]
pub fn message_text(parts: &[MessagePart]) -> String {
    let mut texts = parts.iter().filter_map(|p| {
        if p.part_type == MessagePartType::Text {
            p.content.get("text").and_then(|v| v.as_str())
        } else {
            None
        }
    });
    let mut out = String::new();
    if let Some(first) = texts.next() {
        out.push_str(first);
    }
    for t in texts {
        out.push('\n');
        out.push_str(t);
    }
    out
}
