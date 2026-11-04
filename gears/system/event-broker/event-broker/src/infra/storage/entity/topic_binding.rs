//! `event_broker__topic_binding`: one topic's provisioning record (feature
//! 0006 §2.2) - which log the topic has, and whether it is provisioned. Written
//! by the specification manager only. Platform data, so `unrestricted`.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "event_broker__topic_binding")]
#[secure(unrestricted)]
pub struct Model {
    /// `GtsId::to_uuid(topic_id)`, hyphenated.
    #[sea_orm(primary_key, auto_increment = false)]
    pub topic_uuid: String,
    pub topic_id: String,
    /// The current attempt's log id, then the provisioned log's.
    pub log_id: String,
    /// `provisioning` or `provisioned`.
    pub state: String,
    /// When the current attempt last acted, RFC 3339 UTC, database time.
    pub attempt_at: String,
    /// The partition count the backend reported; set once provisioned.
    pub partitions: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
