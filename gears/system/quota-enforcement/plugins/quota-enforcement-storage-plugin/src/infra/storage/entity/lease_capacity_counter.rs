//! `qe_lease_capacity_counters`: the serialization point of the
//! per-`(tenant, metric)` active-lease cap (I7).
//!
//! The row is the lock every acquisition on this pair takes, not the source of
//! truth for the cap: admission counts live leases under that lock, so an
//! expired but unreclaimed lease never occupies the cap (I4). Settlement, the
//! sweeper and the deactivation cascade never take it.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qe_lease_capacity_counters")]
#[secure(tenant_col = "tenant_id", no_owner, no_type, no_resource)]
pub struct Model {
    /// Owning tenant.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// The metric the cap applies to.
    #[sea_orm(primary_key, auto_increment = false)]
    pub metric: String,
    /// Deprecated: no longer written; dropped in a later migration.
    pub active_count: i32,
    /// Increments once per accepted mutation.
    pub record_version: i32,
    /// Last write.
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
