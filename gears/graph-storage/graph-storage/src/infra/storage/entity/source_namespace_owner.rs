//! Source-namespace ownership (`cpt-cf-graph-storage-dbtable-source-namespace-owner`).
//!
//! One row per `(tenant, namespace)`. The namespace is the `source.system`
//! value of a reference node's identity triple; the row names the producer
//! principal entitled to write it.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "source_namespace_owner")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub namespace: String,
    /// The producer principal entitled to write this namespace.
    pub owner_principal: String,
    pub claimed_at: OffsetDateTime,
    /// Set only by a transfer: who held it before.
    pub previous_owner: Option<String>,
    pub transferred_at: Option<OffsetDateTime>,
    /// The subject that performed the transfer — the audit of the one flow
    /// that can move a namespace.
    pub transferred_by_subject_id: Option<Uuid>,
    pub transferred_by_subject_type: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
