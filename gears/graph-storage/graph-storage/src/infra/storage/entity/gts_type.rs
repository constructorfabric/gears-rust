//! Per-tenant projection of the platform types-registry
//! (`cpt-cf-graph-storage-dbtable-gts-type`).
//!
//! Exists for interning (a 4-byte reference on every node and edge row) and
//! for batch validation without a registry round trip per item. The registry
//! stays authoritative — this is a cache with a foreign identity, never a
//! second source of truth.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "gts_type")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// Interned surrogate, referenced as `gts_<entity>_type_id` elsewhere.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i32,
    /// Deterministic `UUIDv5` of the GTS identifier.
    pub gts_type_uuid: Uuid,
    /// The GTS identifier, for logs and API responses.
    pub gts_type_id: String,
    /// node / edge / attribute.
    pub kind: String,
    /// The type's draft-07 JSON Schema.
    pub type_schema: Json,
    /// Trait values resolved across the derivation chain.
    pub effective_traits: Json,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
