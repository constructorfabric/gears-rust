// @cpt-cf-chat-engine-dbtable-messages:p1
// @cpt-cf-chat-engine-adr-message-tree-structure:p1

use sea_orm::entity::prelude::*;
use sea_orm::{
    AccessMode, ConnectionTrait, IsolationLevel, QueryFilter, QueryOrder, QuerySelect,
    TransactionError, TransactionTrait,
};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::infra::db::migrations::UQ_VARIANT_INDEX;

/// Maximum retries when racing the `uq_messages_session_parent_variant`
/// constraint. After exhaustion callers MUST map the returned error to
/// HTTP `409 Conflict` (see DESIGN §3.7 "Variant Index Concurrency").
pub const VARIANT_INDEX_MAX_RETRIES: u32 = 3;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "messages")]
#[allow(clippy::struct_excessive_bools)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub message_id: Uuid,
    pub session_id: Uuid,
    pub parent_message_id: Option<Uuid>,
    pub role: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub content: serde_json::Value,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub file_ids: Option<serde_json::Value>,
    pub variant_index: i32,
    pub is_active: bool,
    pub is_complete: bool,
    pub is_hidden_from_user: bool,
    pub is_hidden_from_backend: bool,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub metadata: Option<serde_json::Value>,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::session::Entity",
        from = "Column::SessionId",
        to = "super::session::Column::SessionId",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Session,
    #[sea_orm(
        belongs_to = "Entity",
        from = "Column::ParentMessageId",
        to = "Column::MessageId",
        on_update = "NoAction",
        on_delete = "Restrict"
    )]
    Parent,
    #[sea_orm(has_many = "super::message_reaction::Entity")]
    Reaction,
}

impl Related<super::session::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Session.def()
    }
}

impl Related<super::message_reaction::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Reaction.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// Compute the next `variant_index` for the sibling group identified by
/// `(session_id, parent_message_id)`.
///
/// Strategy (DESIGN §3.7 — Variant Index Concurrency):
///   1. Open a SERIALIZABLE transaction scoped to this sibling group.
///   2. `SELECT MAX(variant_index)` for the sibling group.
///   3. Return `max + 1` to the caller (the caller is responsible for
///      issuing the INSERT in the same transaction or, more commonly, the
///      retry loop below replays the helper after a unique-constraint
///      violation).
///
/// Hard cap: `VARIANT_INDEX_MAX_RETRIES` retries. After exhaustion the
/// final error is propagated and callers map it to HTTP 409 Conflict.
///
/// This helper covers the "compute the next variant index" half of the
/// algorithm. Phase 5 (message creation) and Phase 6 (variants) implement
/// the matching INSERT-with-retry loop because the exact INSERT shape
/// depends on the message being created and is out of scope for the
/// db-schema phase.
pub async fn assign_variant_index<C>(
    runner: &C,
    session_id: Uuid,
    parent: Option<Uuid>,
) -> Result<i32, DbErr>
where
    C: ConnectionTrait + TransactionTrait,
{
    let mut last_err: Option<DbErr> = None;

    for _attempt in 0..VARIANT_INDEX_MAX_RETRIES {
        let outcome: Result<i32, TransactionError<DbErr>> = runner
            .transaction_with_config::<_, i32, DbErr>(
                |txn| {
                    Box::pin(async move {
                        let mut query = Entity::find()
                            .filter(Column::SessionId.eq(session_id))
                            .order_by_desc(Column::VariantIndex)
                            .limit(1);

                        query = match parent {
                            Some(p) => query.filter(Column::ParentMessageId.eq(p)),
                            None => query.filter(Column::ParentMessageId.is_null()),
                        };

                        let next = match query.one(txn).await? {
                            Some(row) => row.variant_index + 1,
                            None => 0,
                        };
                        Ok(next)
                    })
                },
                Some(IsolationLevel::Serializable),
                Some(AccessMode::ReadWrite),
            )
            .await;

        match outcome {
            Ok(next) => return Ok(next),
            Err(TransactionError::Transaction(e)) | Err(TransactionError::Connection(e)) => {
                if !is_unique_violation(&e) {
                    return Err(e);
                }
                last_err = Some(e);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| {
        DbErr::Custom(format!(
            "assign_variant_index exhausted {VARIANT_INDEX_MAX_RETRIES} retries"
        ))
    }))
}

/// Crude `DbErr` classifier: returns `true` when the error message refers to
/// the named UNIQUE constraint `uq_messages_session_parent_variant`.
///
/// `SeaORM` does not expose a typed `UniqueConstraintViolation` variant, so
/// downstream retry logic matches on the constraint name embedded in the
/// driver-level error. Phase 6 (variants) is expected to refine this with a
/// SQLSTATE-aware classifier when it materializes the full INSERT path.
fn is_unique_violation(err: &DbErr) -> bool {
    let msg = err.to_string();
    msg.contains(UQ_VARIANT_INDEX) || msg.contains("UNIQUE constraint failed")
}
