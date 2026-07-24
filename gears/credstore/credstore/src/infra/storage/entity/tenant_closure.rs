//! Read-only `SeaORM` entity for the `tenant_closure` system table.
//!
//! `#[secure(unrestricted)]` — this is a platform-managed hierarchy table,
//! not a tenant-scoped resource. Queried only to check descendant membership.
//!
//! **Ownership / creation**: credstore does **not** create this table — it is
//! owned and migrated by Account Management (AM's `m0001_initial_schema`).
//! credstore only reads it, and only in the **co-located** deployment where the
//! table lives in the same database as `credstore_secrets` (config
//! `hierarchy.tenant_closure_colocated`, default `false` — see DESIGN §4.4).
//! When co-located, the gear advertises the `TenantHierarchy` PDP capability and
//! the PDP emits `InTenantSubtree` predicates resolved by a closure subquery;
//! when not, the PDP emits a flat `In` list of tenant ids and this table is
//! never queried (degraded-but-correct mode), so a standalone credstore database
//! without the table is a supported configuration. Tests provision the table via
//! a test-only `CreateTenantClosure` migration.

use sea_orm::entity::prelude::*;
use toolkit_db::secure::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "tenant_closure")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub ancestor_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub descendant_id: Uuid,
    pub barrier: i16,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
