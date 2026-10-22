//! `policy_engine__document` - one policy document of a version. Cascades with
//! its version; never updated after the version leaves draft.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

/// Scoped by the owning tenant; the document identity is the resource.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "policy_engine__document")]
#[secure(tenant_col = "owner_tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    /// Document identity, named in refusals.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// Owning version.
    pub version_id: Uuid,
    /// Owning tenant; the scope column.
    pub owner_tenant_id: Uuid,
    /// Author-facing name, unique within the version.
    pub name: String,
    /// Rego source.
    pub content: String,
    /// JSON array of GTS type patterns the document applies to.
    #[sea_orm(column_type = "JsonBinary")]
    pub resource_types: Json,
    /// JSON array of actions the document applies to; empty means all.
    #[sea_orm(column_type = "JsonBinary")]
    pub actions: Json,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// The owning version (`ON DELETE CASCADE`).
    #[sea_orm(
        belongs_to = "super::bundle_version::Entity",
        from = "Column::VersionId",
        to = "super::bundle_version::Column::Id",
        on_delete = "Cascade"
    )]
    BundleVersion,
}

impl Related<super::bundle_version::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::BundleVersion.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
