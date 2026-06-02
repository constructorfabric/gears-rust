// @cpt-cf-chat-engine-dbtable-messages:p1
// @cpt-cf-chat-engine-adr-message-tree-structure:p1

use sea_orm::entity::prelude::*;
use sea_orm::{ConnectionTrait, QueryFilter, QueryOrder, QuerySelect};
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
/// `(session_id, parent_message_id)` **inside the caller's transaction**.
///
/// This is the SELECT half of the variant-index allocation; the matching
/// INSERT MUST be issued against the same transaction handle so a
/// concurrent caller cannot observe-then-claim the same index between the
/// read and the write. Callers wrap both operations in a single
/// SERIALIZABLE transaction plus a retry loop bounded by
/// [`VARIANT_INDEX_MAX_RETRIES`] — see the call sites in
/// `infra::db::repo::message_repo` and `infra::db::repo::variant_repo`.
///
/// The earlier `assign_variant_index` helper opened its own transaction
/// for the SELECT only and returned `i32` to the caller, who then issued
/// the INSERT in a *separate* transaction. That created a race window
/// where a concurrent caller could claim the same index between the two
/// transactions, surfacing the unique violation as a raw 500 instead of
/// retrying. The helper was removed in favour of this primitive plus
/// inline retry at every call site.
pub async fn compute_next_variant_index<C>(
    txn: &C,
    session_id: Uuid,
    parent: Option<Uuid>,
) -> Result<i32, DbErr>
where
    C: ConnectionTrait,
{
    let mut query = Entity::find()
        .filter(Column::SessionId.eq(session_id))
        .order_by_desc(Column::VariantIndex)
        .limit(1);

    query = match parent {
        Some(p) => query.filter(Column::ParentMessageId.eq(p)),
        None => query.filter(Column::ParentMessageId.is_null()),
    };

    Ok(match query.one(txn).await? {
        Some(row) => row.variant_index + 1,
        None => 0,
    })
}

/// Crude `DbErr` classifier: returns `true` when the error message refers to
/// the named UNIQUE constraint `uq_messages_session_parent_variant`.
///
/// `SeaORM` does not expose a typed `UniqueConstraintViolation` variant, so
/// downstream retry logic matches on the constraint name embedded in the
/// driver-level error. Phase 6 (variants) is expected to refine this with a
/// SQLSTATE-aware classifier when it materializes the full INSERT path.
pub fn is_variant_unique_violation(err: &DbErr) -> bool {
    let msg = err.to_string();
    msg.contains(UQ_VARIANT_INDEX) || msg.contains("UNIQUE constraint failed")
}
