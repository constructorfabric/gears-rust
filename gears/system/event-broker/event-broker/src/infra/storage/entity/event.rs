//! The SQLite `EventBrokerBackend`'s event log
//! (eb-single-process-implementation D3/D4) - the durable, append-only
//! `(topic, partition)` log every `Event` lands in. `id` is a real `Uuid`
//! primary key, so (unlike `consumer_group`) this entity gets the full
//! `resource_col` dimension.

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "event_broker_event")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub type_id: String,
    pub topic: String,
    pub tenant_id: Uuid,
    pub source: String,
    pub subject: String,
    pub subject_type: String,
    pub partition_key: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub trace_parent: Option<String>,
    /// JSON-serialized event payload (`sea_orm`'s `Json` column type - not
    /// a plain `String` - so it round-trips through `serde_json::Value`
    /// directly).
    pub data: Json,
    pub partition: i32,
    /// Broker-logical, consumer-visible sequence - monotonic within
    /// `(topic, partition)`, assigned by this backend at persist time.
    pub sequence: i64,
    pub sequence_time: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
