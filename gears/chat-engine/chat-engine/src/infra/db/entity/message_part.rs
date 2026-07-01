// @cpt-cf-chat-engine-dbtable-message-parts:p1
// @cpt-cf-chat-engine-design-entity-message-part:p1
//
// A message body is an ordered list of typed parts. Each part is one row in
// `message_parts`, keyed `(message_id, number)`; the parts in `number` order
// form the message body. CASCADE FK to `messages` so a hard message delete
// removes its parts. Like the variant index, `number` is allocated as
// `MAX(number)+1` inside the caller's transaction (see
// `compute_next_part_number`) and guarded by `UNIQUE(message_id, number)`.

use sea_orm::entity::prelude::*;
use sea_orm::{Condition, QueryOrder, QuerySelect};
use toolkit_db::secure::{AccessScope, DBRunner, SecureEntityExt};
use toolkit_db_macros::Scopable;
use uuid::Uuid;

use crate::domain::error::ChatEngineError;

// Tenant / user scoping for parts is enforced via the owning `messages` row
// (and in turn its `sessions` row); parts carry no scoping columns of their
// own, so the entity is marked unrestricted and the repo always reaches them
// through a message the caller has already authorized.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "message_parts")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub message_id: Uuid,
    pub r#type: MessagePartType,
    #[sea_orm(column_type = "JsonBinary")]
    pub content: serde_json::Value,
    pub number: i32,
}

/// Persisted part type. A `DeriveActiveEnum` over the stored string so the
/// column can only ever hold one of the known values; the SDK/domain twin is
/// [`chat_engine_sdk::models::MessagePartType`] and the `From` impls in
/// `crate::domain::message` map between the two.
#[derive(Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
pub enum MessagePartType {
    #[sea_orm(string_value = "text")]
    Text,
    #[sea_orm(string_value = "code")]
    Code,
    #[sea_orm(string_value = "images")]
    Images,
    #[sea_orm(string_value = "videos")]
    Videos,
    #[sea_orm(string_value = "links")]
    Links,
    #[sea_orm(string_value = "statuses")]
    Statuses,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::message::Entity",
        from = "Column::MessageId",
        to = "super::message::Column::MessageId",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Message,
}

impl Related<super::message::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Message.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

/// Compute the next `number` for `message_id` **inside the caller's
/// transaction**, mirroring `message::compute_next_variant_index`. The
/// matching INSERT MUST run against the same transaction handle so a
/// concurrent writer cannot claim the same `number` between the read and the
/// write; callers wrap both in a SERIALIZABLE transaction plus a retry loop.
pub async fn compute_next_part_number<R>(
    runner: &R,
    message_id: Uuid,
) -> Result<i32, ChatEngineError>
where
    R: DBRunner,
{
    let scope = AccessScope::allow_all();
    let row = Entity::find()
        .order_by_desc(Column::Number)
        .limit(1)
        .secure()
        .scope_with(&scope)
        .filter(Condition::all().add(Column::MessageId.eq(message_id)))
        .one(runner)
        .await?;

    Ok(match row {
        Some(row) => row.number + 1,
        None => 0,
    })
}
