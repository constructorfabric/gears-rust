// @cpt-cf-chat-engine-dbtable-messages:p1
// @cpt-cf-chat-engine-adr-message-tree-structure:p1

use sea_orm::entity::prelude::*;
use sea_orm::{Condition, QueryOrder, QuerySelect, SqlErr};
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, DBRunner, SecureEntityExt};
use toolkit_db_macros::Scopable;
use uuid::Uuid;

use crate::domain::error::ChatEngineError;
use crate::infra::db::migrations::{UQ_VARIANT_INDEX, UQ_VARIANT_INDEX_ROOT};

/// Maximum retries when racing the `uq_messages_session_parent_variant`
/// constraint. After exhaustion callers MUST map the returned error to
/// HTTP `409 Conflict` (see DESIGN §3.7 "Variant Index Concurrency").
pub const VARIANT_INDEX_MAX_RETRIES: u32 = 3;

// Tenant / user scoping for messages stays enforced via the owning
// `sessions` row, which the repo joins on per request. The denormalized
// `tenant_id` column below is nullable (un-backfilled legacy rows hold
// NULL) and `user_id` is the message *author*, not an ownership key, so
// neither is a safe authority for row scoping yet — the entity stays
// `unrestricted` and scoping remains the explicit session join.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "messages")]
#[secure(unrestricted)]
#[allow(clippy::struct_excessive_bools)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub message_id: Uuid,
    pub session_id: Uuid,
    /// Owning tenant, denormalized from the parent session. NULL only for
    /// un-backfilled legacy rows.
    pub tenant_id: Option<String>,
    /// Author of this message (NULL for assistant/system and legacy rows).
    pub user_id: Option<String>,
    pub parent_message_id: Option<Uuid>,
    pub role: MessageRole,
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

/// Persisted message role. A `DeriveActiveEnum` over the stored string so
/// invalid roles are unrepresentable at the persistence boundary — the
/// column can only ever hold `user` / `assistant` / `system`. Mirrors
/// mini-chat's `MessageRole`. The SDK/domain
/// [`chat_engine_sdk::models::MessageRole`] is the wire/domain twin; the
/// `From` impls in `crate::domain::message` map between the two.
#[derive(Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
pub enum MessageRole {
    #[sea_orm(string_value = "user")]
    User,
    #[sea_orm(string_value = "assistant")]
    Assistant,
    #[sea_orm(string_value = "system")]
    System,
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
    #[sea_orm(has_many = "super::message_part::Entity")]
    Part,
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

impl Related<super::message_part::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Part.def()
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
pub async fn compute_next_variant_index<R>(
    runner: &R,
    session_id: Uuid,
    parent: Option<Uuid>,
) -> Result<i32, ChatEngineError>
where
    R: DBRunner,
{
    let scope = AccessScope::allow_all();

    let parent_filter = match parent {
        Some(p) => Condition::all().add(Column::ParentMessageId.eq(p)),
        None => Condition::all().add(Column::ParentMessageId.is_null()),
    };

    let row = Entity::find()
        .order_by_desc(Column::VariantIndex)
        .limit(1)
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(Column::SessionId.eq(session_id)))
        .filter(parent_filter)
        .one(runner)
        .await?;

    Ok(match row {
        Some(row) => row.variant_index + 1,
        None => 0,
    })
}

/// Detect whether `err` is a UNIQUE-constraint violation on
/// [`UQ_VARIANT_INDEX`] (`uq_messages_session_parent_variant`) — i.e.,
/// a retryable variant-index collision.
///
/// Classification anchors on SeaORM's structured
/// [`SqlErr::UniqueConstraintViolation`], which is in turn derived from
/// the driver's SQLSTATE / extended result code (Postgres `23505`,
/// SQLite `2067`/`1555`, MySQL `1062`/etc.). The substring narrowing
/// inside the violation's message text only filters out unrelated
/// UNIQUE violations elsewhere in the schema; it is no longer matching
/// on the unstructured `Display` of the whole `DbErr`. A driver locale
/// change or SeaORM `Display` tweak therefore cannot silently turn a
/// retryable conflict into a hard failure: the structured discriminant
/// stays put and only the narrowing message-text fallback would skew.
///
/// Postgres includes the index name (`uq_messages_session_parent_variant`
/// or the root partial index `uq_messages_session_root_variant`) in the
/// violation message; SQLite emits the offending column list, which always
/// carries `messages.session_id` + `messages.variant_index` for either
/// index. Both patterns unambiguously identify the crate's variant-index
/// UNIQUE indexes. If neither matches, we return
/// `false` and the caller surfaces the error without retrying — the
/// safe default given an unrecognised UNIQUE violation might belong to
/// a different schema constraint added later.
pub fn is_variant_unique_violation(err: &DbErr) -> bool {
    let Some(SqlErr::UniqueConstraintViolation(message)) = err.sql_err() else {
        return false;
    };
    // Postgres path — the message embeds the index name verbatim. Both the
    // composite index and the root-only partial index are retryable variant
    // collisions.
    if message.contains(UQ_VARIANT_INDEX) || message.contains(UQ_VARIANT_INDEX_ROOT) {
        return true;
    }
    // SQLite path — no constraint name in the message; identify the UQ by the
    // columns it covers. Both variant-uniqueness indexes
    // (`uq_messages_session_parent_variant` over the column triple and the
    // root partial index over `(session_id, variant_index)`) are the only
    // UNIQUE indexes carrying `variant_index`, so `session_id` +
    // `variant_index` together identify either unambiguously (the non-UNIQUE
    // `idx_messages_session_parent` lacks `variant_index` and never raises
    // this error).
    message.contains("messages.session_id") && message.contains("messages.variant_index")
}

#[cfg(test)]
#[path = "message_tests.rs"]
mod message_tests;
