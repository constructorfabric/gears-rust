//! Scope replacement registry (`cpt-cf-graph-storage-dbtable-scope-registry`).
//!
//! One row per canonical scope identity
//! `(tenant, owning producer, scope attribute, scope value)`. Replacement
//! transactions lock the row exclusively; ordinary ingests into an owned
//! scope lock it in shared mode (Concurrent Ingest Protocol). `generation`
//! is the highest accepted source generation — the fencing token.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "scope_registry")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub scope_attribute: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub scope_value: String,
    /// Producer owning this scope.
    pub owner_producer: String,
    /// Highest accepted source generation (fencing).
    pub generation: i64,
    /// Hash of the last accepted replacement snapshot.
    pub request_hash: String,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
