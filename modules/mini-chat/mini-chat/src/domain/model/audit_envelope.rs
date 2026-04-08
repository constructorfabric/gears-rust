use mini_chat_sdk::{TurnAuditEvent, TurnDeleteAuditEvent, TurnMutationAuditEvent};
use modkit_macros::domain_model;
use serde::{Deserialize, Serialize};

/// Serializable discriminated union of all audit event types.
///
/// Stored as JSON in the `mini-chat.audit` outbox queue. A single queue and
/// a single handler (`AuditEventHandler`) process all variants without
/// requiring separate queues per event type.
// TurnAuditEvent is significantly larger than the other variants but the enum is
// transient (serialized to JSON for the outbox and immediately consumed), so boxing
// the variant would only add noise at call sites without measurable benefit.
#[allow(clippy::large_enum_variant)]
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditEnvelope {
    /// Turn completed or failed (`event_type` = `turn_completed` / `turn_failed`).
    Turn(TurnAuditEvent),
    /// Turn mutation: retry or edit (`event_type` = `turn_retry` / `turn_edit`).
    Mutation(TurnMutationAuditEvent),
    /// Turn deleted.
    Delete(TurnDeleteAuditEvent),
}
