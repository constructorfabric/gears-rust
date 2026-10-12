//! Per-tenant graph metadata (`cpt-cf-graph-storage-dbtable-graph-meta`).
//!
//! Two keys are normative: `graph_revision`, the per-tenant monotonic counter
//! advanced by every committed mutation, and `source_epoch`, the
//! deployment-wide, non-reusable timeline identifier (stored under the nil
//! tenant), rotated by operator action after a restore or store replacement.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "graph_meta")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    pub value: Json,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

/// The per-tenant monotonic revision counter.
pub const KEY_GRAPH_REVISION: &str = "graph_revision";
/// The deployment-wide source epoch (stored under the nil tenant).
pub const KEY_SOURCE_EPOCH: &str = "source_epoch";
