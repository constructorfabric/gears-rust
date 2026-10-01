//! `policy_engine__bundle` - a named policy bundle, stable across its
//! versions.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

/// Scoped by the owning tenant; the bundle identity is the resource.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "policy_engine__bundle")]
#[secure(tenant_col = "owner_tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    /// Bundle identity, stable across versions.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// Owning tenant; the scope column.
    pub owner_tenant_id: Uuid,
    /// Bundle name, unique within the owning tenant.
    pub name: String,
    /// What this bundle governs.
    pub description: Option<String>,
    /// Creation time.
    pub created_at: OffsetDateTime,
    /// Author identity.
    pub created_by: Uuid,
    /// Last modification time.
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// The bundle's versions.
    #[sea_orm(has_many = "super::bundle_version::Entity")]
    BundleVersion,
    /// Assignments of this bundle to tenants.
    #[sea_orm(has_many = "super::assignment::Entity")]
    Assignment,
}

impl Related<super::bundle_version::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::BundleVersion.def()
    }
}

impl Related<super::assignment::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Assignment.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
