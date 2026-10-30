//! `event_broker__outbox_identity`: the 64-bit identity of each ingest outbox
//! group (feature 0005 §2.5), the high half of every fence key its messages
//! append under. Platform data, not tenant data, so `unrestricted`.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "event_broker__outbox_identity")]
#[secure(unrestricted)]
pub struct Model {
    /// The group's name, which is also its toolkit-db queue name.
    #[sea_orm(primary_key, auto_increment = false)]
    pub name: String,
    /// The identity's 16 lowercase hex digits (`OutboxId`'s `Display`).
    pub outbox_id: String,
    /// When the identity was minted, RFC 3339 UTC.
    pub created_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
