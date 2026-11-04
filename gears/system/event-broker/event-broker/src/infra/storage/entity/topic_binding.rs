//! `event_broker__topic_binding`: one topic's provisioning record (feature
//! 0006 §2.2) - which log the topic has, and whether it is provisioned. Written
//! by the specification manager only. Platform data, so `unrestricted`.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "event_broker__topic_binding")]
#[secure(unrestricted)]
pub struct Model {
    /// `GtsId::to_uuid(topic_id)`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub topic_uuid: Uuid,
    pub topic_id: String,
    /// The current attempt's log id, then the provisioned log's.
    pub log_id: Uuid,
    /// `provisioning` or `provisioned`.
    pub state: String,
    /// When the current attempt last acted, database time, as the 24-character
    /// text `YYYY-MM-DDTHH:MM:SS.sssZ` (compared as text).
    pub attempt_at: String,
    /// The partition count the backend reported; set once provisioned.
    pub partitions: Option<i64>,
    /// The retention max age last recorded, in milliseconds; the backend
    /// holds it while `retention_confirmed`. Set once provisioned, NULL when
    /// unbounded.
    pub retention_max_age_ms: Option<i64>,
    /// The retention max bytes last recorded; the backend holds it while
    /// `retention_confirmed`. Set once provisioned, NULL when unbounded.
    pub retention_max_bytes: Option<i64>,
    /// Bumped by every write of the retention columns or of
    /// `retention_confirmed`; the mark before a re-provision call and the
    /// retention write after it compare-and-set on it.
    pub retention_version: i64,
    /// `false` while the backend may hold a retention other than the recorded
    /// one, so the next pass hands the backend its retention again.
    pub retention_confirmed: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
