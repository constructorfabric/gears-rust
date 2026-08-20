//! The SQLite `EventBrokerBackend`'s per-`(topic, partition)` bookkeeping
//! (eb-single-process-implementation D3/D4): the next broker-logical
//! sequence to assign, and the outbox-retry dedup's `last_chain_sequence`
//! (the producer chain-sequence field, per DESIGN.md:835-846 - not an
//! `event.id`). Backend-internal state, not tenant-scoped data (a topic can
//! carry events from many tenants) - `#[secure(unrestricted)]`.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "event_broker_partition_state")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub topic: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub partition: i32,
    pub next_sequence: i64,
    /// `NULL` until the first chained/monotonic-mode event lands in this
    /// `(topic, partition)` - stateless-mode events never update this
    /// (DESIGN.md's documented "no broker-side dedup" for stateless).
    pub last_chain_sequence: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
