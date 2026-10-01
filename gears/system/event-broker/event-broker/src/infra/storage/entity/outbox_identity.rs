//! `event_broker__outbox_identity`: the 64-bit identity of each ingest outbox
//! queue (feature 0005 §2.5), the high half of every fence key its messages
//! append under. Platform data, not tenant data, so `unrestricted`.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "event_broker__outbox_identity")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub queue: String,
    /// The identity's 64 bits as a signed integer (`OutboxId::signed`).
    pub outbox_id: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
