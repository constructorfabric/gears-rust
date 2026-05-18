//! `SeaORM` entity for the AM-owned `tenant_metadata` table.
//!
//! Bootstrap persists provider-returned `IdpProvisionResult` metadata here
//! during the same activation transaction that flips a tenant from
//! `provisioning` to `active`.

use modkit_db_macros::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

// @cpt-begin:cpt-cf-account-management-dbtable-tenant-metadata:p2:inst-dbtable-tenant-metadata-entity
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "tenant_metadata")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub schema_uuid: Uuid,
    pub value: Json,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// Monotonic version for optimistic locking. Bumped by every
    /// UPDATE (`current + 1`); seeded at `1` for every INSERT.
    /// Declared on the table in `m0001_initial_schema` so every
    /// fresh deployment starts with the column in place.
    pub version: i64,
}
// @cpt-end:cpt-cf-account-management-dbtable-tenant-metadata:p2:inst-dbtable-tenant-metadata-entity

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
