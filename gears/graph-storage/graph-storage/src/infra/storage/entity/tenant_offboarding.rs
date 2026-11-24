//! The tenant offboarding record (`cpt-cf-graph-storage-dbtable-tenant-offboarding`).
//!
//! Its presence fences the tenant; `completed_at` says the deletion finished.
//! `removed` holds the cumulative row counts per kind of state, as JSON so a
//! kind added later needs no migration.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "tenant_offboarding")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    pub deletion_generation: Option<i64>,
    pub fenced_at: OffsetDateTime,
    pub completed_at: Option<OffsetDateTime>,
    pub removed: Json,
    /// Failed steps in a row; reset by a step that removes rows.
    pub failures: i32,
    /// The error class of the last failed step, never its message.
    pub last_failure: Option<String>,
    pub last_failure_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
