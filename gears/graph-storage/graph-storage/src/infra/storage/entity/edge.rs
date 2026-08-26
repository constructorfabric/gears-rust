//! Graph edges (`cpt-cf-graph-storage-dbtable-edge`).
//!
//! Endpoint foreign keys are `ON DELETE RESTRICT`, never CASCADE: deletion
//! never cascades into edges, so an analysis edge can never be destroyed as a
//! side effect of removing a static node.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "edge")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    /// Deterministic hash of (type, src, dst, discriminator).
    pub edge_key: String,
    /// Interned type reference into `gts_type`.
    pub gts_edge_type_id: i32,
    pub src_node_id: i64,
    pub dst_node_id: i64,
    /// Distinguishes parallel edges of one type between one endpoint pair.
    pub discriminator: Option<String>,
    /// GTS-validated attributes, including provenance for analysis edges.
    pub payload: Json,
    pub created_at: OffsetDateTime,
    /// Soft-delete tombstone; `NULL` for live rows.
    pub deleted_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
