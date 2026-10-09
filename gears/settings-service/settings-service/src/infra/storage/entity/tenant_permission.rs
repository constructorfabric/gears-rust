// Created: 2026-09-07 by Virtuozzo International GmbH
//! `tenant_permissions`: one sparse restriction per `(declaration, tenant)`.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One restriction row.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "tenant_permissions")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    /// Row id.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// The declaration the restriction applies to.
    pub declaration_id: Uuid,
    /// The tenant the restriction is set for.
    pub tenant_id: Uuid,
    /// The access level granted to the tenant, stored as its wire string.
    pub access: String,
    /// Subject who set the restriction.
    pub set_by: String,
    /// When the row was created.
    pub created_at: OffsetDateTime,
    /// When the row was last changed.
    pub updated_at: OffsetDateTime,
}

/// Relations of the `tenant_permission` entity.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::declaration::Entity",
        from = "Column::DeclarationId",
        to = "super::declaration::Column::Id",
        on_delete = "Cascade"
    )]
    /// The declaration this restriction applies to; deleting it cascades.
    Declaration,
}

impl Related<super::declaration::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Declaration.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
