// @cpt-cf-chat-engine-dbtable-stream-events:p2
// @cpt-cf-chat-engine-design-stream-resume:p2
//
// Short-TTL resume buffer for the SSE delta stream (FR-024). Append-only,
// keyed `(message_id, seq)`; rows are replayed to a reconnecting client
// (`Last-Event-ID`) and swept after `expires_at`. This is NOT durable
// conversation history — the durable record is the persisted message.
//
// No FK to `messages`: the buffer is ephemeral infra decoupled from the
// message tree's lifecycle (a row may briefly exist for an in-flight assistant
// message and is reclaimed by TTL, not by message deletion).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

// Ephemeral resume buffer; no tenant/user scoping columns. Marked
// unrestricted so the secure wrappers expose a `&impl DBRunner` path
// (consistent with the other gear entities) without row scoping.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "stream_events")]
#[secure(unrestricted)]
pub struct Model {
    /// Assistant message whose stream this event belongs to (composite PK).
    #[sea_orm(primary_key, auto_increment = false)]
    pub message_id: Uuid,
    /// Per-message monotonic event ordinal, mirrored in the SSE `id:` line
    /// (composite PK).
    #[sea_orm(primary_key, auto_increment = false)]
    pub seq: i64,
    /// Serialized wire event (`start` / `delta` / `complete` / `error`),
    /// replayed verbatim on resume.
    #[sea_orm(column_type = "JsonBinary")]
    pub event: serde_json::Value,
    /// Emission timestamp.
    pub created_at: OffsetDateTime,
    /// TTL deadline; a periodic sweep deletes rows past this.
    pub expires_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
